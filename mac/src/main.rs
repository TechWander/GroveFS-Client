//! `grovefs-mac` — mount a GroveFS *view* at a directory on macOS.
//!
//! Skeleton for the Apple counterpart of `grovefs mount` (Linux) and
//! `grovefs-win` (Windows). It parses the SAME connection/mount options and
//! reuses grovefs's `remote-only` seam — `RemoteMeta` for the authoritative
//! view tree, `RemoteBackend` for object bytes — over the SAME framed TCP
//! protocol, so only the kernel bridge is Apple-specific.
//!
//! Mount host (to implement): macFUSE, driven through `fuser`'s macOS backend
//! (the same FUSE bridge grovefs already uses on Linux), or FSKit on newer
//! macOS. The planned layout mirrors `../win`: translation logic that needs no
//! FUSE types in a host-testable module, and the macOS-only glue behind
//! `#[cfg(target_os = "macos")]`.
//!
//!   grovefs-mac --meta-addr 10.0.0.1:50111 --data-addr 10.0.0.1:50110 \
//!               --view alice --token s3cr3t --mount /Volumes/alice

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "grovefs-mac",
    about = "Mount a GroveFS view as a directory on macOS (skeleton)"
)]
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
    /// Existing empty directory to mount on, e.g. `/Volumes/alice`.
    #[arg(long, value_name = "DIR")]
    mount: String,
    /// Present the view strictly read-only regardless of what the server allows.
    #[arg(long)]
    ro: bool,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("grovefs-mac: {e}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // The CLI and the grovefs `remote-only` seam are in place; the mount host
    // is not. Keep the parsed configuration in the message so the argument
    // surface can be verified from any host (including Linux CI).
    let host = if cfg!(target_os = "macos") {
        "macFUSE/FSKit mount backend not implemented yet"
    } else {
        "grovefs-mac only mounts on macOS; macFUSE is unavailable here"
    };
    Err(format!(
        "{host} (parsed: --meta-addr {} --data-addr {} --view {} --mount {} --ro {}; \
         to check the CLI on this host run `cargo run -p grovefs-mac -- --help`)",
        cli.meta_addr, cli.data_addr, cli.view, cli.mount, cli.ro
    )
    .into())
}
