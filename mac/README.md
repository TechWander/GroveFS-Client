# grovefs-mac

Native **macOS** client that mounts a GroveFS *view* at a directory — the
macOS counterpart of `grovefs mount` on Linux and [`grovefs-win`](../win/README.md)
on Windows. It talks to the **same** cluster services over the **same** framed
TCP protocol (central `meta-server` for the view tree, a `data-server` for
object bytes) and serves the result through **macFUSE**.

Unlike the Windows client, it does not re-implement the VFS surface: it reuses
the real `GroveFS` FUSE implementation (`grovefs::fs_impl`) that `grovefs mount`
uses on Linux. Only the kernel bridge host differs.

Point at a `grovefs manager serve` and let a signed roster supply both cluster
coordinates — this is the only way this client learns where to connect. ADR-0002
removed every other mount form: there is no local mount, and hand-typed
`--meta-addr`/`--data-addr` are no longer accepted (passing them is an
unknown-argument error), so the manager bootstrap is required:

```
grovefs-mac --manager 10.0.0.5:50055 --roster-key-file roster.key \
            --view alice --token s3cr3t --mount /Volumes/alice
```

## How it reuses `grovefs`

The main library exposes two client-facing feature sets:

| feature | provides |
|---------|----------|
| `remote-only` | `RemoteMeta` + `RemoteBackend` + the wire protocol, no `fuser`/`libc`, no engine |
| `fuse-bridge` | the `fs_impl` mount path + `bwlimit` + real `libc` POSIX shims, no engine/servers |

This crate combines them:

```toml
grovefs = { path = "../../grovefs", default-features = false,
            features = ["remote-only", "fuse-bridge"] }
```

So the dependency graph is the client half only — **no `rusqlite`/`libsqlite3`,
no `postgres`/`tokio`**:

```sh
cargo tree -p grovefs-mac | grep -cE 'rusqlite|libsqlite3|postgres|tokio'   # 0
```

## Building

### macOS

macFUSE 4.x must be installed (its kernel extension approved) and discoverable
by `pkg-config`; fuser's build script probes it:

```sh
brew install --cask macfuse
brew install pkgconf
cargo build -p grovefs-mac --release
```

On Apple Silicon, enable third-party kernel extensions in *System Settings →
Privacy & Security* before the first mount.

### Linux (for testing)

fuser 0.18 uses its pure-Rust mount on Linux, so the same binary builds and
mounts there — handy for end-to-end checks without a Mac:

```sh
cd grovefs-client
cargo build -p grovefs-mac
```

## Usage

```
grovefs-mac --manager HOST:PORT --roster-key-file PATH [--deployment NAME] \
            --view NAME --token TOKEN [--api-key ID:SECRET] --mount DIR [--ro] \
            [--no-cache] [--cache-ttl SECS] [--layout FILE] [-o OPT]...
```

* The manager bootstrap is the **only** way coordinates are supplied — there is
  no second form to conflict with; `--meta-addr`/`--data-addr` are no longer
  flags on this client. `--manager` without `--roster-key-file` is refused by
  name. The key file is the manager's own `manager/roster.key`, which `manager serve`
  mints on first start and names in its banner (`manager: roster key … — copy
  to every client as mount --roster-key-file`); it is 64 hex characters
  (`openssl rand -hex 32` makes one), the file is checked for that shape
  before the manager is asked anything, and since it signs the answer with a
  plain HMAC the manager's copy must stay secret. `GROVEFS_MANAGER` /
  `GROVEFS_ROSTER_KEY_FILE` / `GROVEFS_DEPLOYMENT` carry the same mount with no
  flags. The manager answers once, at mount time, and serves no traffic
  afterwards: its later outage refuses new mounts, it cannot move a live one.

* `--mount` is an **existing** directory (macFUSE mounts onto it; it does not
  create one). `volname=GroveFS-<view>` is passed on macOS so Finder shows a
  named volume.
* `--token` / `--api-key` are the server-side credential gate, same semantics
  as the Windows client. `--api-key` left unset falls back to env
  `GROVEFS_API_KEY` — the same entry `grovefs mount` offers, so a deployment
  can keep the secret out of `ps` and shell history on every client kind.
* `--ro` forces read-only; an `-o ro` does the same, and a view the server
  reports as *derived* is mounted read-only automatically.
* A view the server reports as a **fork** mounts differently: its directory
  tree is yours (`mkdir`/`rename`/`unlink`/`chmod` are served) while its
  CONTENT is shared with the base view, so `write`/truncate come back `EROFS`
  from the first `write(2)`. The flag arrives in `M_OPEN` ack byte [2] and is
  enforced HERE — the server never gates the byte stream — which is why this
  client must be rebuilt whenever the kernel's handshake grows a field.
* `--no-cache` turns on strict cross-client coherence (dentry/attr TTL 0 +
  direct IO); otherwise the kernel cache TTL is `--cache-ttl` (default 2 s).
* `--layout FILE` puts this mount behind a presentation lens — the same rules
  file and the same client-side `LayoutMeta` the kernel mount's `--layout`
  builds, so the cut is per-mount: the stored tree, the server, and every other
  client keep seeing the whole view. Writes through the lens land on the real
  entries; creating directly inside a synthesized (lens-only) dir is refused
  with EPERM.
* `-o` passes extra fuser/macFUSE options (`allow_root`, `allow_other`,
  `default_permissions`, `auto_unmount`, `volname=...`).
* `Ctrl-C` unmounts through the session's unmounter, so no stale volume is left
  behind.

## v1 scope

* Single `data-server` for the data plane (no client-side striping).
  A roster from `manager serve` that declares several data members is therefore
  **refused** rather than narrowed to one of its members: choosing a member here
  would give this mount a layout other than the one the deployment declared, and
  that is the kernel `grovefs mount`'s call to make, not a client's.
* No Data-Access Capability to configure by hand: when the cluster's
  `meta-server` runs with `--data-access-key`, the capability it mints at
  `M_OPEN` is latched and replayed on every data-plane `OP_OPEN` automatically
  (a `data-server --data-access-key` refuses a client that never passed a
  genuine meta admission, so an out-of-date binary simply fails to mount).
* Single-threaded session dispatch (fuser `n_threads`/`clone_fd` is a Linux
  optimisation; macFUSE serves the one loop).
* No macOS-specific metadata features (tags, resource forks, ACLs); the tree is
  the GroveFS view, presented POSIX-style.
