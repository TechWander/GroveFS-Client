//! Build script: WinFsp must be DELAY-LOADED.
//!
//! The `winfsp-x64.dll` a built binary calls into ships with the WinFsp 2.1
//! *install*, not with the Rust toolchain, so the linker is told to delay-load
//! it (resolve the import lazily at first call rather than requiring the import
//! library at link time).
//!
//! `winfsp::build::winfsp_link_delayload()` is the crate's own helper for this,
//! but it is nothing more than a printer over `cargo:rustc-link-*` directives
//! keyed on the target triple's env vars — and calling it would force a
//! `winfsp` build-dependency, which on a Linux host drags in `winfsp-sys`
//! (bindgen + the Windows SDK) and fails to even build. So we inline the exact
//! same logic here, self-contained: on a non-Windows target we emit nothing and
//! the crate still builds for `cargo test`; on Windows we emit the delay-load
//! flags for the supported linkage environments (MSVC, or LLVM-MinGW).
//!
//! See the two arm bodies in the `winfsp` crate's `src/build.rs` for the source
//! of these directives.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        // Linux/macOS host running `cargo test` of the winfsp-free mapping
        // module — there is nothing Windows-specific to link.
        println!("cargo:rustc-check-cfg=cfg(windows)");
        return;
    }

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let dll = match target_arch.as_str() {
        "x86_64" => "winfsp-x64.dll",
        "x86" => "winfsp-x86.dll",
        "aarch64" => "winfsp-a64.dll",
        _ => return, // unsupported arch: leave linking to the toolchain default
    };

    match (target_env.as_str(), target_abi.as_str()) {
        // MSVC toolchain: link the delay-load helper and name the DLL.
        ("msvc", _) => {
            println!("cargo:rustc-link-lib=dylib=delayimp");
            println!("cargo:rustc-link-arg=/DELAYLOAD:{dll}");
        }
        // LLVM-MinGW (lld-link / ld.lld in mingw mode): GNU-style flag; the
        // linker's own --delayload lowering provides the helper, so `delayimp`
        // is not linked explicitly here.
        ("gnu", "llvm") => {
            println!("cargo:rustc-link-arg=-Wl,--delayload={dll}");
        }
        // Plain GNU (x86_64-pc-windows-gnu with mingw-w64 gcc) is not a
        // supported WinFsp linkage — `winfsp-sys` rejects it at build time —
        // so we emit nothing and let the dependency's own build fail loudly.
        _ => {}
    }
}
