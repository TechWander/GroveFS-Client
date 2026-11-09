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
    /// Central metadata service address: `host:port` (the `meta-server`).
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

    // 1. Authoritative view tree over the LAN.
    let rm = RemoteMeta::new_with_creds(
        &cli.meta_addr,
        &cli.view,
        &cli.token,
        // self-reported audit uid; the Windows client has no POSIX uid.
        0,
        &cli.api_key,
    )?;
    // The `M_OPEN` ack byte flags a DERIVED (virtual, read-only) view. Capture
    // it before the concrete client is boxed behind the trait object.
    let view_ro = rm.is_readonly_view();
    let meta: Arc<dyn MetaProvider> = Arc::new(rm);

    // 2. Object bytes over the LAN (single data-server; no striping in v1).
    let backend: Arc<dyn StorageBackend> = Arc::new(RemoteBackend::new_with_creds(
        &cli.data_addr,
        &cli.token,
        &cli.api_key,
    )?);

    // 3. Read-only is the CLI flag OR a server-side derived lens the `M_OPEN`
    //    ack marked virtual (a physical view stays writable).
    let read_only = cli.ro || view_ro;
    let fs = GroveWinFs::new(meta, backend, read_only);

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
        "grovefs-win: mounted view '{}' at {mount_arg} (read-only: {read_only}) — press Ctrl-C to unmount",
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
         (parsed: --meta-addr {} --data-addr {} --view {} --mount {}; \
         to test on this host run `cargo test`)",
        cli.meta_addr, cli.data_addr, cli.view, cli.mount
    )
    .into())
}
