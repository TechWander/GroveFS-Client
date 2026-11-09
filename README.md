# grovefs-client

Platform-native GroveFS clients. Each member mounts a GroveFS *view* using the
host OS's own kernel filesystem bridge — no Linux FUSE, no NFS — while talking
to the **same** cluster services over the **same** framed TCP protocol: the
central `meta-server` for the authoritative view tree and a `data-server` for
object bytes.

| member | kernel bridge | state |
|--------|---------------|-------|
| [`win/`](win/README.md) | WinFsp | Working: mounts a view as a drive letter (`M:`, `next`). Re-implements the WinFsp callback surface. |
| [`mac/`](mac/README.md) | macFUSE | Working: mounts a view at a directory, reusing the `GroveFS` FUSE implementation from the main library. |

Both crates are client-only. `win/` takes the transport seam alone:

```toml
grovefs = { path = "../../grovefs", default-features = false, features = ["remote-only"] }
```

`mac/` adds the FUSE bridge so it can serve the remote view through the SAME
`GroveFS` mount path as `grovefs mount`:

```toml
grovefs = { path = "../../grovefs", default-features = false,
            features = ["remote-only", "fuse-bridge"] }
```

Neither pulls the on-disk metadata engine or the servers (`rusqlite`,
`postgres`, `tokio`).

## Layout

```
grovefs-client/
├── Cargo.toml          # workspace: members = ["win", "mac"], lto in release
├── win/                # WinFsp client (binary: grovefs-win.exe)
│   ├── Cargo.toml
│   ├── build.rs        # delay-load flags for winfsp-x64.dll
│   ├── src/mapping.rs  # FUSE/WinFsp-free translation, host-tested
│   ├── src/fs.rs       # #[cfg(windows)] WinFsp callback surface
│   ├── src/main.rs     # CLI + mount wiring
│   └── vendor/         # winfsp API-mirror stub (offline type-check only)
└── mac/                # macFUSE client (binary: grovefs-mac)
    ├── Cargo.toml
    ├── src/main.rs     # CLI + fuser Session over grovefs::fs_impl::GroveFS
    └── README.md
```

The two members choose different reuse seams: Windows maps WinFsp callbacks in
`win/src/fs.rs` onto the same `MetaProvider`/`StorageBackend` traits, while
macOS reuses the `GroveFS` FUSE implementation verbatim (the `fuse-bridge`
feature) and only mounts it through fuser's macFUSE backend.

## Build and test

```sh
# Workspace-wide, on any host (win/ runs its mapping tests).
cargo test

# Per member.
cargo test -p grovefs-win          # 10 mapping tests on Linux/macOS hosts
cargo run  -p grovefs-mac -- --help

# Real Windows binary (needs WinFsp 2.1 + MSVC or LLVM-MinGW, see win/README).
cargo build -p grovefs-win --release --target x86_64-pc-windows-msvc

# macOS binary (needs macFUSE 4.x + pkgconf on the Mac; builds and even mounts
# on Linux via fuser's pure-Rust mount, see mac/README).
cargo build -p grovefs-mac --release
```

Binaries land in the workspace `target/` directory.
