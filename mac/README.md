# grovefs-mac

Native **macOS** client that mounts a GroveFS *view* at a directory — the
macOS counterpart of `grovefs mount` on Linux and [`grovefs-win`](../win/README.md)
on Windows. It is a **skeleton**: the CLI and the `grovefs` client seam are
wired, the mount host is not implemented.

## Current state

```sh
cargo run -p grovefs-mac -- --help     # full option surface, any host
```

`run()` parses the configuration and exits non-zero with a clear
"not implemented yet" message. Nothing is mounted — on macOS or elsewhere.

## Planned mount host

Two routes, both keeping the platform glue thin:

* **macFUSE** through `fuser`'s macOS backend — the same high-level `fuser`
  0.18 bridge grovefs already uses on Linux, so the existing `fs_impl` logic
  can be reused rather than rewritten.
* **FSKit** — Apple's user-space filesystem framework on newer macOS, if
  macFUSE (a kernel extension requiring user approval) is not acceptable.

Layout mirrors `../win`: put every path/attribute/time conversion that needs no
FUSE types in a host-testable module (`mapping.rs` equivalent), and keep the
macOS-only glue behind `#[cfg(target_os = "macos")]` so `cargo test` still runs
the translation logic on Linux CI.

## Interface

The option surface is identical to `grovefs-win`, except `--mount` takes an
existing empty directory instead of a drive letter:

```
grovefs-mac --meta-addr 10.0.0.1:50111 --data-addr 10.0.0.1:50110 \
            --view alice --token s3cr3t --mount /Volumes/alice [--ro]
```

* `--token` / `--api-key` are the server-side credential gate (same semantics
  as the Windows client).
* `--ro` forces read-only; a view the server reports as *derived* will also be
  mounted read-only.

## Dependency seam

Same as `win/`: `grovefs` with `default-features = false, features =
["remote-only"]` — `RemoteMeta` + `RemoteBackend` + wire protocol only, no
`fuser`/`libc`/metadata engine in the graph.
