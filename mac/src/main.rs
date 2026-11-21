//! `grovefs-mac` — mount a GroveFS *view* at a directory on macOS.
//!
//! The Apple counterpart of `grovefs mount` (Linux) and `grovefs-win`
//! (Windows). It talks to the SAME cluster services over the SAME framed TCP
//! protocol — the central `meta-server` for the authoritative view tree, a
//! `data-server` for object bytes — and serves the result through macFUSE,
//! reusing the real `GroveFS` FUSE implementation (`grovefs::fs_impl`) rather
//! than re-deriving the VFS surface.
//!
//! The library seam: grovefs's `fuse-bridge` feature supplies `fs_impl` +
//! `bwlimit` + the real `libc` shims without the on-disk engine or the cluster
//! servers; `remote-only` supplies `RemoteMeta` + `RemoteBackend` + the wire
//! protocol. Together they are exactly "the client half of `grovefs mount`".
//!
//!   grovefs-mac --meta-addr 10.0.0.1:50111 --data-addr 10.0.0.1:50110 \
//!               --view alice --token s3cr3t --mount /Volumes/alice
//!
//! Or let the deployment's `grovefs manager` resolve both coordinates from one
//! signed roster:
//!
//!   grovefs-mac --manager 10.0.0.5:50120 --roster-key-file /etc/grovefs/roster.hex \
//!               --view alice --token s3cr3t --mount /Volumes/alice
//!
//! Building for real on macOS needs macFUSE 4.x installed (fuser's build
//! script probes it with pkg-config); see README.

use std::path::PathBuf;

use clap::Parser;
use fuser::{Config as SessionConfig, MountOption, Session, SessionACL};
use grovefs::backend::StorageBackend;
use grovefs::fs_impl::GroveFS;
use grovefs::meta::MetaProvider;
use grovefs::remote::{RemoteBackend, RemoteMeta};

#[derive(Parser, Debug)]
#[command(
    name = "grovefs-mac",
    about = "Mount a GroveFS view as a directory on macOS (macFUSE)"
)]
struct Cli {
    /// Central metadata service address: `host:port` (the `meta-server`).
    /// May also be a failover list (`primary,standby`). Give this together
    /// with `--data-addr`, or use `--manager` to have both resolved for you.
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "manager")]
    meta_addr: Option<String>,
    /// Data service address: `host:port` (the `data-server`).
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "manager")]
    data_addr: Option<String>,
    /// Mount bootstrap: ask this `grovefs manager` for the deployment's SIGNED
    /// roster and take the two service coordinates from it, instead of writing
    /// both by hand. Requires `--roster-key-file`: an unverified roster is an
    /// instruction to connect wherever someone else wants. The manager is
    /// consulted once, during the mount — it is a bootstrap, never a proxy, so
    /// its restart or outage cannot disturb a live mount. Falls back to env
    /// `GROVEFS_MANAGER`.
    #[arg(long, value_name = "HOST:PORT")]
    manager: Option<String>,
    /// Which deployment `--manager` should resolve (default `default`). Falls
    /// back to env `GROVEFS_DEPLOYMENT`.
    #[arg(long, value_name = "NAME")]
    deployment: Option<String>,
    /// The roster VERIFICATION key for `--manager`: a file of 64 hex characters,
    /// the same key the manager signs with as its `--roster-key-file`. A file
    /// rather than a value on purpose — an HMAC key is symmetric, so this is a
    /// secret and a command-line one would land in `ps` and shell history.
    /// Falls back to env `GROVEFS_ROSTER_KEY_FILE`.
    #[arg(long, value_name = "PATH")]
    roster_key_file: Option<PathBuf>,
    /// Which view tree to present at the mount point.
    #[arg(long, value_name = "NAME")]
    view: String,
    /// Cluster access token (matches `meta-server`/`data-server --token`).
    #[arg(long, value_name = "TOKEN", default_value = "")]
    token: String,
    /// Optional per-caller API key `<id>:<secret>` for a server started with
    /// `--require-api-key`. Empty means gate by the static token only.
    #[arg(long, value_name = "ID:SECRET", default_value = "")]
    api_key: String,
    /// Existing directory to mount on, e.g. `/Volumes/alice`.
    #[arg(long, value_name = "DIR")]
    mount: PathBuf,
    /// Present the view strictly read-only regardless of what the server allows.
    #[arg(long)]
    ro: bool,
    /// Strict cache coherence for multi-client mounts: disable the kernel
    /// attribute/dentry cache and read file data straight through
    /// (FOPEN_DIRECT_IO), so a peer's writes are visible immediately.
    #[arg(long = "no-cache")]
    no_cache: bool,
    /// Kernel attribute/dentry cache TTL in seconds (default 2). Ignored when
    /// `--no-cache` is set.
    #[arg(long = "cache-ttl", value_name = "SECS", default_value_t = 2)]
    cache_ttl: u64,
    /// Extra fuser/macFUSE options, e.g. allow_root, allow_other,
    /// default_permissions, auto_unmount, volname=GroveFS.
    #[arg(short = 'o', long, num_args = 0..)]
    options: Vec<String>,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("grovefs-mac: {e}");
        std::process::exit(1);
    }
}

/// The roster → two-coordinates decision, kept free of I/O so it can be
/// unit-tested on any host.
///
/// `meta` is handed over as the whole ordered list because `RemoteMeta` takes a
/// failover list and walks it in order — dropping a standby would silently
/// weaken HA with no error at all. `data` is deliberately NOT: client-side
/// striping lives in `backend::sharded`, which is `native`-gated, so a
/// `remote-only` build cannot link it. A roster declaring several data members
/// is therefore refused here rather than quietly mounted against one of them:
/// choosing a member on the client's behalf would give this mount a layout
/// other than the one the deployment declared, and that is a correctness call
/// a mount has no business making (`check_layout` in the striped backend exists
/// precisely to refuse this class of mismatch).
fn roster_coords(r: &grovefs::manager::Roster) -> Result<(String, String), String> {
    if r.meta.is_empty() {
        return Err(format!("roster for deployment '{}' carries no meta-server address", r.deployment));
    }
    match r.data.as_slice() {
        [] => Err(format!("roster for deployment '{}' carries no data-server address", r.deployment)),
        [one] => Ok((r.meta.join(","), one.clone())),
        many => Err(format!(
            "roster for deployment '{}' declares {} data servers: this client does not stripe, \
             so it cannot mount this deployment — use the kernel `grovefs mount`, which takes \
             the whole list (pointing --data-addr at one member is a DIFFERENT layout, not a \
             smaller one)",
            r.deployment,
            many.len()
        )),
    }
}

/// Resolve the two service coordinates — exactly as given, or from the
/// deployment's signed roster. Never from both sources at once.
fn resolve_addrs(cli: &Cli) -> std::result::Result<(String, String), Box<dyn std::error::Error>> {
    // Same env fallbacks as `grovefs mount`, so a deployment can set the
    // bootstrap once in the service environment and keep it out of argv.
    let manager = cli
        .manager
        .clone()
        .or_else(|| std::env::var("GROVEFS_MANAGER").ok());
    match &manager {
        Some(mgr) => {
            let key_file = cli
                .roster_key_file
                .clone()
                .or_else(|| std::env::var("GROVEFS_ROSTER_KEY_FILE").ok().map(PathBuf::from))
                .ok_or(
                    "--manager needs --roster-key-file (or GROVEFS_ROSTER_KEY_FILE): an \
                     unverified roster would let someone else choose the addresses we connect to",
                )?;
            let key = grovefs::manager::read_hex_key_file(&key_file)
                .map_err(|e| format!("--roster-key-file {}: {e}", key_file.display()))?;
            let deployment = cli
                .deployment
                .clone()
                .or_else(|| std::env::var("GROVEFS_DEPLOYMENT").ok())
                .unwrap_or_else(|| grovefs::manager::DEFAULT_DEPLOYMENT.to_string());
            // Name the manager that was asked: `lookup`'s own IO error only
            // carries the OS reason, and a bare "Connection refused" does not
            // say WHICH service was unreachable — the explicit-coordinate path
            // has always printed its address, so do not regress next to it.
            let roster = grovefs::manager::lookup(mgr, &deployment, &key)
                .map_err(|e| format!("asking manager {mgr} for deployment '{deployment}': {e}"))?;
            let coords = roster_coords(&roster)?;
            eprintln!(
                "grovefs-mac: bootstrapped from manager {mgr}: deployment '{}' meta [{}] data [{}] (roster epoch {}, expires {})",
                roster.deployment,
                roster.meta.join(","),
                roster.data.join(","),
                roster.epoch,
                roster.expires_at,
            );
            Ok(coords)
        }
        None => match (&cli.meta_addr, &cli.data_addr) {
            (Some(m), Some(d)) => Ok((m.clone(), d.clone())),
            _ => Err("give both --meta-addr and --data-addr, or --manager HOST:PORT with --roster-key-file".into()),
        },
    }
}

fn run(cli: &Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // macFUSE requires an EXISTING mount point (it mounts *onto* a directory,
    // unlike a drive letter); creating one silently would leave an empty dir
    // behind on a failed mount, so refuse early with a clear message.
    let mountpoint = &cli.mount;
    let md = std::fs::metadata(mountpoint).map_err(|e| {
        format!(
            "mount point {} is not usable: {e} (create an empty directory first)",
            mountpoint.display()
        )
    })?;
    if !md.is_dir() {
        return Err(format!("mount point {} is not a directory", mountpoint.display()).into());
    }

    // 1. Authoritative view tree over the LAN. The two coordinates come either
    //    from the CLI or from one signed roster lookup — never from both.
    let (meta_addr, data_addr) = resolve_addrs(cli)?;
    let rm = RemoteMeta::new_with_creds(
        &meta_addr,
        &cli.view,
        &cli.token,
        // self-reported audit uid; access control is the token / API key.
        grovefs::platform::getuid(),
        &cli.api_key,
    )?;
    // The `M_OPEN` ack is the mount's contract, and this client reads the SAME
    // capability bytes the Linux `grovefs mount` does: [0] marks a read-only
    // view (a server-side DERIVED lens, or a read-only API key) — the whole
    // mount is read-only; [2] marks a FORK — the tree is writable, the CONTENT
    // is shared with the base view, so writes/truncates must come back EROFS.
    // Handing a fork out as fully writable is not a cosmetic miss: the server
    // does not gate the byte stream, so this layer is the only enforcement
    // (see the version-consistency note in the kernel manual §15.1).
    let view_ro = rm.is_readonly_view();
    let content_ro = rm.is_content_ro_view();
    // Data-Access Capability for a `data-server --data-access-key`: minted by
    // the meta-server at `M_OPEN`, replayed verbatim on every data-plane
    // `OP_OPEN`. Empty when the deployment issues none (pre-DAC path).
    let dac = rm.data_access_capability();
    match rm.def_kind() {
        "fork" => eprintln!(
            "grovefs-mac: fork view '{v}' of base '{b}': independent tree — structure writable, content read-only (EROFS)",
            v = cli.view,
            b = rm.def_base_view()
        ),
        "derived" => eprintln!(
            "grovefs-mac: derived view '{v}' over base '{b}': read-only lens — write to base view '{b}'",
            v = cli.view,
            b = rm.def_base_view()
        ),
        "alias" => eprintln!(
            "grovefs-mac: alias view '{v}' resolves to real view '{r}' (full tree, writable)",
            v = cli.view,
            r = rm.resolved_view()
        ),
        _ => {}
    }
    let meta: std::sync::Arc<dyn MetaProvider> = std::sync::Arc::new(rm);

    // 2. Object bytes over the LAN (single data-server; no striping in v1 —
    //    `backend::sharded` is `native`-gated, so a client build cannot link it;
    //    a roster declaring several members is refused by `roster_coords`).
    let backend: std::sync::Arc<dyn StorageBackend> = std::sync::Arc::new(
        RemoteBackend::new_with_dac(&data_addr, &cli.token, &cli.api_key, &dac)?,
    );

    // 3. Read-only is the CLI flag, an `-o ro`, or a server-side derived lens.
    let o_ro = cli.options.iter().any(|s| s == "ro");
    let read_only = cli.ro || o_ro || view_ro;

    // The SAME FUSE implementation `grovefs mount` uses, fed the remote seam.
    // `content_ro` is forwarded verbatim, so the fork gets the kernel-mount
    // behaviour for free: EROFS on write/truncate, no writeback-cache
    // negotiation (the refusal lands on the first `write(2)`), while
    // mkdir/rename/unlink/chmod stay served.
    let fs = GroveFS::new(
        meta,
        backend,
        read_only,
        content_ro,
        cli.no_cache,
        true, // READDIRPLUS
        cli.cache_ttl,
        0, // no client-side bandwidth cap
        0,
    );

    // 4. Session config. macFUSE names the volume in Finder via `volname=`;
    //    other options pass through exactly like `grovefs mount -o`.
    let mut cfg = SessionConfig::default();
    cfg.mount_options
        .push(MountOption::FSName(format!("grovefs:{}", cli.view)));
    if read_only {
        cfg.mount_options.push(MountOption::RO);
    }
    #[cfg(target_os = "macos")]
    cfg.mount_options
        .push(MountOption::CUSTOM(format!("volname=GroveFS-{}", cli.view)));
    for raw in &cli.options {
        match raw.as_str() {
            "allow_root" => cfg.acl = SessionACL::RootAndOwner,
            "allow_other" => cfg.acl = SessionACL::All,
            "default_permissions" => cfg.mount_options.push(MountOption::DefaultPermissions),
            "auto_unmount" => cfg.mount_options.push(MountOption::AutoUnmount),
            "ro" => {
                if !cfg.mount_options.contains(&MountOption::RO) {
                    cfg.mount_options.push(MountOption::RO);
                }
            }
            other => cfg.mount_options.push(MountOption::CUSTOM(other.to_string())),
        }
    }

    // 5. Mount, then serve. `run()` is the blocking loop; Ctrl-C takes the
    //    mount down through the callable unmounter so macFUSE releases the
    //    mount point instead of leaving a stale volume behind.
    let mut session = Session::new(fs, mountpoint, &cfg).map_err(|e| {
        if cfg!(target_os = "macos") {
            format!(
                "mount failed: {e} — is macFUSE installed and its kernel extension \
                 approved? (`brew install --cask macfuse`)"
            )
        } else {
            format!("mount failed: {e}")
        }
    })?;
    let mut unmounter = session.unmount_callable();
    ctrlc::set_handler(move || {
        let _ = unmounter.unmount();
    })?;

    eprintln!(
        "grovefs-mac: mounted view '{}' at {} (read-only: {read_only}, content read-only: {content_ro}) — press Ctrl-C to unmount",
        cli.view,
        mountpoint.display()
    );
    session.run()?;
    eprintln!("grovefs-mac: unmounted {}", mountpoint.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::roster_coords;
    use grovefs::manager::Roster;

    fn roster(meta: &[&str], data: &[&str]) -> Roster {
        Roster {
            deployment: "prod".into(),
            meta: meta.iter().map(|s| s.to_string()).collect(),
            data: data.iter().map(|s| s.to_string()).collect(),
            epoch: 7,
            issued_at: 1,
            expires_at: 2,
        }
    }

    #[test]
    fn one_of_each_is_taken_verbatim() {
        let (m, d) = roster_coords(&roster(&["10.0.0.1:50111"], &["10.0.0.1:50110"])).unwrap();
        assert_eq!(m, "10.0.0.1:50111");
        assert_eq!(d, "10.0.0.1:50110");
    }

    /// Dropping a standby from the meta list would weaken HA silently — the
    /// whole ordered list has to reach `RemoteMeta`.
    #[test]
    fn the_whole_meta_failover_list_survives() {
        let (m, d) =
            roster_coords(&roster(&["10.0.0.1:50111", "10.0.0.2:50111"], &["10.0.0.1:50110"])).unwrap();
        assert_eq!(m, "10.0.0.1:50111,10.0.0.2:50111");
        assert_eq!(d, "10.0.0.1:50110");
    }

    /// This build cannot link `backend::sharded`, so a striped roster must be
    /// an error rather than a mount against an arbitrary one of the members.
    #[test]
    fn a_striped_data_roster_is_refused() {
        let err = roster_coords(&roster(&["10.0.0.1:50111"], &["10.0.0.1:50110", "10.0.0.2:50110"]))
            .unwrap_err();
        assert!(err.contains("does not stripe"), "{err}");
        assert!(err.contains("2 data servers"), "{err}");
    }

    #[test]
    fn an_incomplete_roster_is_refused() {
        assert!(roster_coords(&roster(&[], &["10.0.0.1:50110"])).unwrap_err().contains("no meta-server"));
        assert!(roster_coords(&roster(&["10.0.0.1:50111"], &[])).unwrap_err().contains("no data-server"));
    }
}
