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
    /// May also be a failover list (`primary,standby`).
    #[arg(long, value_name = "HOST:PORT")]
    meta_addr: String,
    /// Data service address: `host:port` (the `data-server`).
    #[arg(long, value_name = "HOST:PORT")]
    data_addr: String,
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

    // 1. Authoritative view tree over the LAN.
    let rm = RemoteMeta::new_with_creds(
        &cli.meta_addr,
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
    //    `backend::sharded` is `native`-gated, so a client build cannot link it).
    let backend: std::sync::Arc<dyn StorageBackend> = std::sync::Arc::new(
        RemoteBackend::new_with_dac(&cli.data_addr, &cli.token, &cli.api_key, &dac)?,
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
