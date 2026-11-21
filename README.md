# grovefs-client

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

Platform-native GroveFS clients. Each member mounts a GroveFS *view* using the
host OS's own kernel filesystem bridge — no Linux FUSE, no NFS — while talking
to the **same** cluster services over the **same** framed TCP protocol: the
central `meta-server` for the authoritative view tree and a `data-server` for
object bytes.

Full usage guide — installation, every flag, the three admission gates, what each
view kind does to a mount, troubleshooting, and how to verify a change:
**[`docs/usage.html`](docs/usage.html)** (self-contained page, open it in a browser).

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
cargo test -p grovefs-win          # 11 mapping tests on Linux/macOS hosts
cargo run  -p grovefs-mac -- --help

# Real Windows binary (needs WinFsp 2.1 + MSVC or LLVM-MinGW, see win/README).
cargo build -p grovefs-win --release --target x86_64-pc-windows-msvc

# macOS binary (needs macFUSE 4.x + pkgconf on the Mac; builds and even mounts
# on Linux via fuser's pure-Rust mount, see mac/README).
cargo build -p grovefs-mac --release
```

Binaries land in the workspace `target` directory.

## Staying in sync with the kernel

The framed protocol carries **no version number**: compatibility is decided by
how strictly each side parses the `M_OPEN` handshake. Two rules follow, and
both clients implement them:

* **Read every field the current server sends.** `M_OPEN` ack byte [0] marks a
  read-only view (a DERIVED lens, or a read-only API key), byte [2] marks a
  FORK (structure writable, content `EROFS`), and the tail names what the
  mounted view resolved to. Ignoring byte [2] is not cosmetic: the content gate
  lives in the mount, the server never gates the byte stream.
* **Replay what the meta plane hands the data plane.** A `meta-server
  --data-access-key` mints a Data-Access Capability at `M_OPEN`; both clients
  forward it to every `data-server OP_OPEN` (`RemoteBackend::new_with_dac`), so
  a `--data-access-key` cluster mounts normally and refuses an out-of-date
  binary with the server's own refusal text instead of degrading silently.

Practically: rebuild these clients against the kernel you deploy, and refresh
`dist/` in the same change that adopts a new handshake field. The kernel board
runs the shipped `dist/grovefs-mac` against its own servers precisely so this
pairing is tested rather than assumed.

That pairing is the board's `client` row — kernel-side `.build-bin/test_client.sh`,
38 assertions (39 with its negative control), sections A–G: SQLite and PostgreSQL
control planes with a kernel mount reading back this client's bytes; an unknown
view refused; the static `--token` gate both ways; a `--data-access-key` pair
where the capability has to travel `M_OPEN` → `OP_OPEN` (and an UNGATED pair on
the same store still mounting the old byte-for-byte way); the FORK mount serving
`mkdir`/`rename` while `write(2)` returns `EROFS` and the base view stays
writable; the DERIVED lens wholly read-only; and `--require-api-key` admitting
`--api-key`. Run it from the kernel checkout with `PRE_DAC_REV=<rev>` — a
`grovefs-client` revision whose `dist/` predates the capability handshake — and
its one extra assertion proves the gate refuses that older binary rather than
letting it mount. The script lives in `.build-bin/`, which the kernel's
`.gitignore` keeps local (`/.*/`), so it ships with the checkout, not with a
clone: extending the handshake means extending that script there and syncing the
assertion counts the kernel's `README.md` / `USAGE.md` / `docs/docs.html` quote
for the `client` row.

## License

Apache License 2.0（见仓库根目录 `LICENSE`）。

Licensed under the [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0);
see the [`LICENSE`](LICENSE) file at the repository root for the full text.
