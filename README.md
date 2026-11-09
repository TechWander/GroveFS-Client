# grovefs-client

Platform-native GroveFS clients. Each member mounts a GroveFS *view* using the
host OS's own kernel filesystem bridge — no Linux FUSE, no NFS — while talking
to the **same** cluster services over the **same** framed TCP protocol: the
central `meta-server` for the authoritative view tree and a `data-server` for
object bytes.

| member | kernel bridge | state |
|--------|---------------|-------|
| [`win/`](win/README.md) | WinFsp | Working: mounts a view as a drive letter (`M:`, `next`). |
| [`mac/`](mac/README.md) | macFUSE / FSKit | Skeleton: CLI wired, mount backend not implemented. |

Both crates depend on the main library the same way — client half only:

```toml
grovefs = { path = "../../grovefs", default-features = false, features = ["remote-only"] }
```

`remote-only` compiles `RemoteMeta` + `RemoteBackend` + the wire protocol and
stubs the `libc`/`fuser` call sites, so neither client pulls the Linux FUSE
mount, the on-disk metadata engine, the servers, or their crate deps
(`rusqlite`, `postgres`, `tokio`).

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
└── mac/                # macOS client skeleton (binary: grovefs-mac)
    ├── Cargo.toml
    └── src/main.rs     # same CLI surface, mount backend pending
```

The split inside `win/` is the pattern `mac/` will follow: every conversion
that needs no FUSE type lives in a host-testable module, and the platform-only
glue is gated behind `#[cfg(target_os = ...)]`, so the translation logic is
exercised by `cargo test` on any machine.

## Build and test

```sh
# Workspace-wide, on any host (mac/ is a stub; win/ runs its mapping tests).
cargo test

# Per member.
cargo test -p grovefs-win          # 10 mapping tests on Linux/macOS hosts
cargo run  -p grovefs-mac -- --help

# Real Windows binary (needs WinFsp 2.1 + MSVC or LLVM-MinGW, see win/README).
cargo build -p grovefs-win --release --target x86_64-pc-windows-msvc
```

Binaries land in the workspace `target/` directory.
