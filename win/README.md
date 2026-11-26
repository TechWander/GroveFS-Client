# grovefs-win

Native **Windows** client that mounts a GroveFS *view* as a drive letter, via
[WinFsp](https://winfsp.dev/) — the Windows analogue of FUSE. It lives in the
`win/` member of the [`grovefs-client`](../README.md) workspace, next to the
`mac/` skeleton. It is the Windows
counterpart of `grovefs mount` on Linux: it talks to the **same** cluster
services over the **same** framed TCP protocol (the central `meta-server` for
the authoritative view tree, a `data-server` for object bytes), but presents
the result through WinFsp's signed kernel driver so it appears as an ordinary
drive in Explorer and any Win32 application.

```
grovefs-win --manager 10.0.0.5:50055 --roster-key-file roster.key \
            --view alice --token s3cr3t --mount M:
```

## How it reuses `grovefs`

The whole point of this crate is that it needs **none** of GroveFS's POSIX/FUSE
machinery — only the client half of the two storage traits:

* [`MetaProvider`] — the view tree, served by `grovefs::remote::RemoteMeta`
* [`StorageBackend`] — object bytes, served by `grovefs::remote::RemoteBackend`

To let a Windows target (and the Linux host, for tests) build those two without
the Linux-only `fuser` and `libc` crates, `grovefs` gained a **`remote-only`**
Cargo feature. This crate consumes GroveFS with it:

```toml
grovefs = { path = "../../grovefs", default-features = false, features = ["remote-only"] }
```

`remote-only` compiles `RemoteMeta` + `RemoteBackend` + the `proto` framing, and
stubs out the few `libc`/`fuser` call sites through `grovefs::platform` (errno
constants, `getuid`, self-SIGTERM). The entire server-side/control-plane
subtree — `fs_impl` (the FUSE bridge), the local backend, the on-disk meta
engine, the meta-*server*, MCP, apikeys, `control`, `pg_store` — is gated
behind `native` and absent from this build's dependency graph. That also drops
their crate deps: **no `rusqlite`/`libsqlite3-sys`** (so no bundled-SQLite C
compilation), **no `postgres`/`tokio`**, no `fuser`/`libc`. Verify with:

```sh
cd win       # cargo commands target the workspace member from here
cargo tree --target x86_64-pc-windows-gnu \
  | grep -cE 'rusqlite|libsqlite3|postgres|tokio|fuser'   # 0
```

## Source layout

| file | compiled on | role |
|------|-------------|------|
| `src/mapping.rs` | **every host** | WinFsp-free translation: path decomposition, `FILETIME` epoch math, Windows `FILE_ATTRIBUTE_*` bits, NTSTATUS codes, `NodeInfo → file-info` projection, `MetaError → NTSTATUS`, and the read-only / content-read-only write gate. Unit-tested here on Linux. |
| `src/fs.rs` | `#[cfg(windows)]` | The thin `winfsp::filesystem::FileSystemContext` implementation: marshals `U16CStr` names and WinFsp's `FileInfo`/`FileSecurity`/`VolumeInfo` to and from `mapping`'s plain integers. |
| `src/main.rs` | every host | CLI (`--manager/--roster-key-file/--deployment`, plus `--view/--token/--api-key/--mount/--ro`) — the manager bootstrap is the only way coordinates are supplied; hand-typed `--meta-addr`/`--data-addr` are no longer accepted. On Windows it wires `RemoteMeta`+`RemoteBackend` into a `FileSystemHost` and mounts; on other hosts `run()` is a compiling stub that points you at `cargo test`. |

Keeping every WinFsp-free decision in `mapping.rs` means the fiddly conversions
are exercised by `cargo test` on any machine — the Windows-only file is
deliberately just glue.

## Verifying on a Linux host

`winfsp`/`winfsp-sys` are declared under `[target.'cfg(windows)'.dependencies]`,
so on Linux they are **not** in the build graph at all:

```sh
cd win
cargo test          # builds the grovefs `remote-only` seam + mapping + CLI contract, runs 19 tests (11 mapping, 8 roster/bootstrap/credential)
```

(`cargo test -p grovefs-win` from the workspace root does the same.)

This compiles the entire non-Windows surface and validates the path/time/
attribute/NTSTATUS logic against the real `grovefs` metadata types.

## 离线类型检查（无 libclang / WinFsp / MSVC 也能验证 cfg(windows) 代码）

`fs.rs` 与 `main.rs` 的挂载分支被 `#[cfg(windows)]` 门住，正常 Linux 主机根本不
编译它们；而真实 `winfsp-sys` 会跑 bindgen（需 libclang + WinFsp SDK），只支持
MSVC / LLVM-MinGW，沙箱里造不出来。替代办法：用 cargo 的 `[patch.crates-io]`
把 `winfsp` 换成一个**纯 Rust 的 API 镜像 stub**
（[`vendor/winfsp-api-mirror`](vendor/winfsp-api-mirror)，签名逐字抄自
`winfsp-0.13.2+winfsp-2.1` 源码），让 `#[cfg(windows)]` 代码进入类型检查：

```sh
cd win
# 临时取消 Cargo.toml 里那两行注释：
#   [patch.crates-io]
#   winfsp = { path = "vendor/winfsp-api-mirror" }
cargo check --target x86_64-pc-windows-gnu   # 编译 fs.rs + main.rs 挂载路径
# ⚠ 检查完必须重新注释掉：stub 没有 FFI，链接出来的二进制无法服务 IRP。
```

这一步**确实抓出了两个真机构建也会撞的编译错误**（因为 stub 忠实复刻了真实约
束）：`let mut di = DirInfo::new()` 的 const 泛型 `BUFFER_SIZE` 不参与推断、必须
钉死为 `DirInfo::<255>::new()`；`host.mount(&MountPoint::NextFreeDrive)` 应
按值传 `MountPoint::NextFreeDrive`（`&'b M: Into<MountPoint<'b>>` 的 HRTB 会拒绝
双重引用）。它验证 trait 签名、字段名、`Sync` bound 与泛型推断，**唯一验证不到
的是与真 crate 的 ABI 链接**——那仍需下面的真实 Windows 构建。

## Building for real on Windows

A linkable Windows binary needs the WinFsp runtime and a toolchain that
`winfsp-sys` can generate bindings for (it runs **bindgen**, so it requires
**libclang**, and it only supports **MSVC** or **LLVM-MinGW** — not plain
GNU/MinGW-w64 gcc).

1. **Install WinFsp 2.1** (provides `winfsp-x64.dll`) and the **WinFsp SDK /
   developer files** (the `libwinfsp` import library + headers, so bindgen has a
   `windows.h`-based include path).
2. **Install libclang** (e.g. via the LLVM installer; set `LIBCLANG_PATH` if it
   is not found automatically).
3. Use a supported toolchain:
   * **MSVC** — *Recommended*: install *Desktop development with C++* in
     Visual Studio Build Tools, and the Windows SDK. Then
     `rustup default stable-msvc` (or add the `x86_64-pc-windows-msvc` target).
   * **LLVM-MinGW** — an alternative; `build.rs` emits the matching
     `--delayload` linker flag for the `gnu`/`llvm` ABI.
4. Build:

   ```powershell
   cargo build --release --target x86_64-pc-windows-msvc
   ```

   Run it from `win/` (or add `-p grovefs-win` from the workspace root); the
   binary lands in the workspace `target/` directory.

`build.rs` is **self-contained**: it inlines the delay-load directives
(`cargo:rustc-link-lib=dylib=delayimp` + `/DELAYLOAD:winfsp-x64.dll` for MSVC, or
`-Wl,--delayload=winfsp-x64.dll` for LLVM-MinGW) rather than depending on the
`winfsp` crate at build time, so the build script still compiles on non-Windows
hosts. Delay-loading means the binary does not require the WinFsp import library
at link time — the DLL is resolved from the WinFsp install at first call.

## Run

Point at a `grovefs manager serve` and let a signed roster supply both
coordinates — the only way this client learns where to connect. ADR-0002
removed every other mount form: there is no local mount, and hand-typed
`--meta-addr`/`--data-addr` are no longer accepted (passing them is an
unknown-argument error):

```powershell
.\..\target\x86_64-pc-windows-msvc\release\grovefs-win.exe `
  --manager 10.0.0.5:50055 --roster-key-file C:\etc\grovefs\roster.key `
  --view alice --token s3cr3t --mount next
```

`--meta-addr`/`--data-addr` are no longer flags on this client, so there is no
second form to conflict with; `--manager` without
`--roster-key-file` is refused by name; `GROVEFS_MANAGER` /
`GROVEFS_ROSTER_KEY_FILE` / `GROVEFS_DEPLOYMENT` supply the same three without
any flags. The manager is asked once while the mount is being made and carries
no traffic afterwards, so its later outage can only refuse a new mount.

`--token` / `--api-key` are the server-side credential gate; a blank
`--api-key` falls back to env `GROVEFS_API_KEY` (the same entry
`grovefs mount` offers, so the secret stays out of the process listing).
`--mount next` picks the first free drive letter; `--ro` forces read-only even
if the view is writable. A view the server reports as *derived* (a virtual,
read-only lens) is mounted read-only automatically. A view reported as a
**fork** mounts with its tree writable (`mkdir`/`rename`/`delete` are served)
but its CONTENT read-only — `write` and `set_file_size` return
`STATUS_MEDIA_WRITE_PROTECTED`, because those bytes belong to the base view.
Both flags come from the `M_OPEN` ack, and the content flag is enforced HERE:
the server never gates the byte stream, so this client must be rebuilt
whenever the kernel's handshake grows a field. Press `Ctrl-C` to unmount.

When the cluster's `meta-server` runs with `--data-access-key`, the
Data-Access Capability it mints at `M_OPEN` is latched and replayed on every
data-plane `OP_OPEN` automatically — no flag to pass. A `data-server` in
verify mode refuses a client that carries none, so an out-of-date binary fails
at mount with the server's refusal text rather than silently losing writes.

## v1 scope

* Single `data-server` for the data plane (no client-side striping —
  `backend::sharded` is `native`-gated, so a `remote-only` build cannot link
  it; the resolved roster must carry exactly one data address). A roster declaring several data members is
  therefore REFUSED, not narrowed to one member: picking a member here would
  hand this mount a layout the deployment did not declare. The kernel
  `grovefs mount` takes the whole list and is the way to mount such a
  deployment; the board's `client` suite pins both halves of that pair.
* No POSIX record locks surfaced through the mount (GroveFS has the primitive;
  mapping it to WinFsp byte-range locks is future work).
* No `--layout` lens yet (the mac shell has one): this shell decomposes WinFsp
  `\path\to\name` strings, a different path space from the lens's POSIX `/`,
  and wiring it without a real Windows mount to verify against would ship an
  untested claim rather than a port.
* No NT ACLs: security is the server-side credential gate (`--token` /
  `--api-key`); files report existence + read-only attribute only.
* Symlink nodes are represented but not rendered as Windows reparse points.

[`MetaProvider`]: ../../grovefs/src/meta.rs
[`StorageBackend`]: ../../grovefs/src/backend/mod.rs
