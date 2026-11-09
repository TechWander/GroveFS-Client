# dist — prebuilt client binaries

Linux x86_64 builds of both workspace members, produced from the source in this
commit. They exist so the clients can be exercised (and the mac client really
mounted) without a Rust toolchain on the target host.

| file | what it is |
|------|-----------|
| `grovefs-mac` | Full macOS client, built on Linux. fuser uses its pure-Rust mount here, so this binary mounts a real GroveFS view on Linux; on macOS the same source builds against macFUSE 4.x. |
| `grovefs-win` | **Not a Windows executable.** Built on Linux it is the `cfg(not(windows))` stub: it parses the CLI and exits with "only runs on Windows". The real `grovefs-win.exe` must be built on Windows with WinFsp 2.1 + MSVC/LLVM-MinGW (see `../win/README.md`). |

Verify with `sha256sum -c SHA256SUMS.txt`; build details in `BUILDINFO.txt`.

Both binaries are stripped release builds. Rebuild them with:

```sh
cargo build --release -p grovefs-mac -p grovefs-win
strip --strip-unneeded target/release/grovefs-mac target/release/grovefs-win
```
