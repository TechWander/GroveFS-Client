//! `grovefs-win` — mount a GroveFS *view* as a native Windows drive via WinFsp.
//!
//! This is the Windows counterpart of `grovefs mount` on Linux. It talks to the
//! SAME cluster services over the SAME framed TCP protocol — the central
//! `meta-server` (authoritative view tree) and a `data-server` (object bytes) —
//! but presents the result through WinFsp's signed kernel driver instead of
//! FUSE, so it appears as an ordinary drive letter in Explorer / any Win32 app.
//!
//! The library seam that makes this possible is grovefs's `remote-only`
//! feature: `RemoteMeta` + `RemoteBackend` + `proto` build with neither `fuser`
//! nor `libc` in the graph, so a Windows target links cleanly. The translation
//! logic that needs no WinFsp type lives in [`mapping`] (host-tested); the
//! WinFsp callback surface lives in [`fs`] (`#[cfg(windows)]` only).
//!
//!   grovefs-win --meta-addr 10.0.0.1:50111 --data-addr 10.0.0.1:50110 \
//!               --view alice --token s3cr3t --mount M:
//!
//! Or let the deployment's `grovefs manager` resolve both coordinates from one
//! signed roster:
//!
//!   grovefs-win --manager 10.0.0.5:50120 --roster-key-file C:\\grovefs\\roster.hex \\
//!               --view alice --token s3cr3t --mount next

use std::path::PathBuf;

// WinFsp-free logic, always compiled so it is unit-testable on any host. Its
// helpers are consumed by `fs.rs` on Windows and by the tests everywhere; on a
// non-Windows, non-test build they are simply unused.
#[allow(dead_code)]
mod mapping;

#[cfg(windows)]
mod fs;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "grovefs-win", about = "Mount a GroveFS view as a Windows drive (WinFsp)")]
struct Cli {
    /// Central metadata service address: `host:port` (the `meta-server`). May
    /// also be a failover list (`primary,standby`), which `RemoteMeta` walks in
    /// order. Give this together with `--data-addr`, or use `--manager` to have
    /// both resolved for you.
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "manager")]
    meta_addr: Option<String>,
    /// Data service address: `host:port` (the `data-server`). One address: this
    /// build cannot link the sharded backend, so it does not stripe.
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "manager")]
    data_addr: Option<String>,
    /// Mount bootstrap: ask this `grovefs manager` for the deployment's SIGNED
    /// roster and take the two service coordinates from it, instead of writing
    /// both by hand. Requires `--roster-key-file`: an unverified roster is an
    /// instruction to connect wherever someone else wants. The manager is
    /// consulted once, during the mount — a bootstrap, never a proxy, so its
    /// restart or outage cannot disturb a live mount. Falls back to env
    /// `GROVEFS_MANAGER`.
    #[arg(long, value_name = "HOST:PORT")]
    manager: Option<String>,
    /// Which deployment `--manager` should resolve (default `default`). Falls
    /// back to env `GROVEFS_DEPLOYMENT`.
    #[arg(long, value_name = "NAME")]
    deployment: Option<String>,
    /// The roster VERIFICATION key for `--manager`: a file of 64 hex characters,
    /// the same key the manager signs with as its own `--roster-key-file`. A
    /// file rather than a value on purpose — an HMAC key is symmetric, so this
    /// is a secret that does not belong in a command line. Falls back to env
    /// `GROVEFS_ROSTER_KEY_FILE`.
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
    /// Where to mount: a drive letter (`M:`), a directory path, or `next` for
    /// the first free drive letter.
    #[arg(long, value_name = "MOUNT")]
    mount: String,
    /// Present the view strictly read-only regardless of what the server allows.
    #[arg(long)]
    ro: bool,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("grovefs-win: {e}");
        std::process::exit(1);
    }
}

// The coordinate resolution is WinFsp-free, so it is compiled for the tests on
// any host and only ever called from the Windows mount path (which is why it is
// gated `any(windows, test)` rather than always present).

/// The roster → two-coordinates decision, kept free of I/O so it can be
/// unit-tested on any host.
///
/// `meta` is handed over as the whole ordered list because `RemoteMeta` takes a
/// failover list and walks it in order — dropping a standby would silently
/// weaken HA with no error at all. `data` is deliberately NOT: client-side
/// striping lives in `backend::sharded`, which is `native`-gated, so a
/// `remote-only` build cannot link it. A roster declaring several data members
/// is therefore refused rather than quietly mounted against one of them:
/// choosing a member on the client's behalf would give this mount a layout
/// other than the one the deployment declared, and that is a correctness call
/// a mount has no business making (`check_layout` in the striped backend exists
/// precisely to refuse this class of mismatch).
#[cfg(any(windows, test))]
fn roster_coords(r: &grovefs::manager::Roster) -> Result<(String, String), String> {
    if r.meta.is_empty() {
        return Err(format!(
            "roster for deployment '{}' carries no meta-server address",
            r.deployment
        ));
    }
    match r.data.as_slice() {
        [] => Err(format!(
            "roster for deployment '{}' carries no data-server address",
            r.deployment
        )),
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
///
/// `allow(dead_code)`: its caller lives in the `#[cfg(windows)]` mount path, so
/// a Linux test build compiles it (for the `roster_coords` coverage below)
/// without ever calling it.
#[cfg(any(windows, test))]
#[allow(dead_code)]
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
                "grovefs-win: bootstrapped from manager {mgr}: deployment '{}' meta [{}] data [{}] (roster epoch {}, expires {})",
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
            _ => Err(
                "give both --meta-addr and --data-addr, or --manager HOST:PORT with --roster-key-file".into(),
            ),
        },
    }
}

// ---- Windows: the real WinFsp mount path ---------------------------------

#[cfg(windows)]
fn run(cli: &Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use std::sync::Arc;

    use fs::GroveWinFs;
    use grovefs::backend::StorageBackend;
    use grovefs::meta::MetaProvider;
    use grovefs::remote::{RemoteBackend, RemoteMeta};
    use winfsp::host::{FileSystemHost, MountPoint, VolumeParams};
    use winfsp::winfsp_init;

    // Initialize the WinFsp runtime; the returned token must outlive the host.
    let _init = winfsp_init()?;

    // 1. Authoritative view tree over the LAN. The two coordinates come either
    //    from the CLI or from one signed roster lookup — never from both.
    let (meta_addr, data_addr) = resolve_addrs(cli)?;
    let rm = RemoteMeta::new_with_creds(
        &meta_addr,
        &cli.view,
        &cli.token,
        // self-reported audit uid; the Windows client has no POSIX uid.
        0,
        &cli.api_key,
    )?;
    // The `M_OPEN` ack is the mount's contract, and the same three capability
    // bytes the Linux `grovefs mount` reads drive this mount:
    //   [0] read-only view (a server-side DERIVED lens, or a read-only key),
    //   [2] FORK view — its tree is the caller's to organize, its BYTES are the
    //       base's, so content writes come back `EROFS`.
    // A pre-three-byte client that ignored byte [2] would hand out a "writable"
    // fork mount whose writes the server does not gate (see USAGE §15.1), so
    // this client must keep parsing the ack as it grows.
    let view_ro = rm.is_readonly_view();
    let content_ro = rm.is_content_ro_view();
    // Data-Access Capability for a `data-server --data-access-key`: minted by
    // the meta-server at `M_OPEN`, replayed verbatim on every data-plane
    // `OP_OPEN`. Empty when the deployment issues none — the pre-DAC path.
    // Forwarding it is what lets a mount that passed a genuine meta admission
    // reach the data plane at all once the server starts verifying.
    let dac = rm.data_access_capability();
    // What the mounted name resolved to, so the operator sees the contract at
    // mount time rather than inferring it from the first failing write.
    match rm.def_kind() {
        "fork" => eprintln!(
            "grovefs-win: fork view '{v}' of base '{b}': independent tree — structure writable, content read-only",
            v = cli.view,
            b = rm.def_base_view()
        ),
        "derived" => eprintln!(
            "grovefs-win: derived view '{v}' over base '{b}': read-only lens — write to the base view",
            v = cli.view,
            b = rm.def_base_view()
        ),
        "alias" => eprintln!(
            "grovefs-win: alias view '{v}' resolves to real view '{r}' (full tree, writable)",
            v = cli.view,
            r = rm.resolved_view()
        ),
        _ => {}
    }
    let meta: Arc<dyn MetaProvider> = Arc::new(rm);

    // 2. Object bytes over the LAN (single data-server; no striping in v1 — the
    //    sharded backend is `native`-gated, so a `remote-only` client cannot
    //    link it; a roster declaring several members is refused by
    //    `roster_coords`, and a comma-separated `--data-addr` is one address).
    let backend: Arc<dyn StorageBackend> = Arc::new(RemoteBackend::new_with_dac(
        &data_addr,
        &cli.token,
        &cli.api_key,
        &dac,
    )?);

    // 3. Two independent gates, exactly as the Linux mount splits them:
    //    the WHOLE volume is read-only for `--ro` or a derived lens; a FORK
    //    keeps its tree writable and refuses CONTENT writes instead.
    let read_only = cli.ro || view_ro;
    let fs = GroveWinFs::new(meta, backend, read_only, content_ro);

    // 4. Volume shape. WinFsp needs a filesystem id + sane geometry.
    let mut vp = VolumeParams::new();
    vp.filesystem_name("GroveFs")
        .sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(255)
        // Generous timeouts so a slow LAN round-trip is not mis-read as a hang.
        .volume_info_timeout(3000)
        .security_timeout(1000)
        .dir_info_timeout(1000)
        .file_info_timeout(1000)
        .case_preserved_names(true)
        .case_sensitive_search(false)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .reparse_points(false)
        .named_streams(false)
        .read_only_volume(read_only);

    let mut host = FileSystemHost::new(vp, fs)?;

    // 5. Mount, then start the dispatcher (WinFsp serves IRPs on its own
    //    worker threads). `host` is kept alive until Ctrl-C, and its `Drop`
    //    stops the dispatcher and removes the mount point.
    let mount_arg = &cli.mount;
    if mount_arg.eq_ignore_ascii_case("next") {
        host.mount(MountPoint::NextFreeDrive)?;
    } else {
        host.mount(mount_arg.as_str())?;
    }
    host.start()?;
    eprintln!(
        "grovefs-win: mounted view '{}' at {mount_arg} (read-only: {read_only}, content read-only: {content_ro}) — press Ctrl-C to unmount",
        cli.view
    );

    // Block until Ctrl-C; the unmount then happens as `host` is dropped.
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    let _ = rx.recv();
    eprintln!("grovefs-win: unmounting {mount_arg}");
    Ok(())
}

// ---- Non-Windows: a compiling stub (WinFsp does not exist here) -----------

#[cfg(not(windows))]
fn run(cli: &Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // The `grovefs-win` binary mounts through WinFsp, which is Windows-only;
    // on other hosts the crate still builds (for `cargo test` of the pure
    // [`mapping`] logic) but cannot mount. Echo the parsed configuration so a
    // caller sees the CLI is wired correctly, then exit.
    Err(format!(
        "grovefs-win mounts via WinFsp and only runs on Windows \
         (parsed: --meta-addr {} --data-addr {} --manager {}; \
         to test on this host run `cargo test`)",
        cli.meta_addr.as_deref().unwrap_or("-"),
        cli.data_addr.as_deref().unwrap_or("-"),
        cli.manager.as_deref().unwrap_or("-"),
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::{resolve_addrs, roster_coords, Cli};
    use clap::error::ErrorKind;
    use clap::Parser;
    use grovefs::manager::Roster;

    /// The same CLI contract the mac shell is held to, and the reason it is
    /// duplicated here rather than trusted to look alike: this crate is the one
    /// neither the kernel board nor a Linux host ever mounts, so the only thing
    /// that can catch `--manager` drifting on this side is a test that runs
    /// everywhere. `--manager` has to parse alone (it owns both coordinates) and
    /// be refused as a CONFLICT beside either hand-typed one, never resolved by
    /// precedence.
    #[test]
    fn manager_alone_is_a_complete_coordinate_source() {
        let cli = Cli::try_parse_from([
            "grovefs-win", "--manager", "10.0.0.5:50120", "--view", "alice", "--mount", "M:",
        ])
        .unwrap();
        assert_eq!(cli.manager.as_deref(), Some("10.0.0.5:50120"));
        assert!(
            cli.meta_addr.is_none() && cli.data_addr.is_none(),
            "the manager owns both coordinates"
        );
        // Not clap-required on purpose: GROVEFS_ROSTER_KEY_FILE is a legal way to
        // supply the key, and the message explaining why it is needed is written
        // at resolution time.
        assert!(cli.roster_key_file.is_none());
    }

    #[test]
    fn manager_and_a_typed_coordinate_are_a_conflict_not_a_precedence() {
        for other in ["--meta-addr", "--data-addr"] {
            let err = Cli::try_parse_from([
                "grovefs-win", "--manager", "10.0.0.5:50120", other, "10.0.0.9:50111", "--view",
                "alice", "--mount", "M:",
            ])
            .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::ArgumentConflict, "{other}: {err}");
        }
    }

    /// Verification comes before the request, and this pins the ORDER rather
    /// than the text: `lookup` failures are wrapped as "asking manager …", so a
    /// refusal without that prefix proves nothing was ever sent. Nothing listens
    /// on 127.0.0.1:1, which is what makes the order observable with no cluster
    /// — the only place this half of the symmetry could be checked, since the
    /// kernel board mounts the mac binary.
    #[test]
    fn a_manager_without_a_key_is_refused_before_it_is_asked() {
        let cli = Cli::try_parse_from([
            "grovefs-win", "--manager", "127.0.0.1:1", "--view", "alice", "--mount", "M:",
        ])
        .unwrap();
        let err = resolve_addrs(&cli).unwrap_err().to_string();
        assert!(err.contains("--roster-key-file"), "{err}");
        assert!(!err.contains("asking manager"), "the manager was contacted: {err}");
    }

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
        let err = roster_coords(&roster(
            &["10.0.0.1:50111"],
            &["10.0.0.1:50110", "10.0.0.2:50110"],
        ))
        .unwrap_err();
        assert!(err.contains("does not stripe"), "{err}");
        assert!(err.contains("2 data servers"), "{err}");
    }

    #[test]
    fn an_incomplete_roster_is_refused() {
        assert!(roster_coords(&roster(&[], &["10.0.0.1:50110"]))
            .unwrap_err()
            .contains("no meta-server"));
        assert!(roster_coords(&roster(&["10.0.0.1:50111"], &[]))
            .unwrap_err()
            .contains("no data-server"));
    }
}
