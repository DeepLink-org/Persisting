# pVisor

**Portable Agent Execution Runtime** — library API for one Agent Run.

```text
CLI / pPilot / host
        │  PVisor::builder()…build()
        │  PVisor::run(spec) → RunHandle
        ▼
pVisor
    │  prepare: capture TOML → proxy + [network] + [overlay]
    │  execute Attempt
    │  teardown
        ▼
RunHandle::wait / cancel / events
```

No separate control plane. Hosts call the crate API directly.

## Modules

| Module | Role |
|--------|------|
| `pvisor` | [`PVisor`] / [`PVisorBuilder`] / [`RunHandle`] |
| `runtime` | Attempt prepare (capture, network, overlay) |
| `access` | Re-export of `persisting-access` |
| `process` | Host process executor |

## Shared config

Capture, network, and overlay share the capture TOML (`ProxyConfig`):

```toml
listen = "127.0.0.1:19081"

[network]
mode = "allowlist"
allowed_hosts = ["pypi.org"]

[overlay]
enabled = true
target = "/path/to/project"     # RO lower + apply destination
backend = "redb"                # default; alternative: "directory"
# database_path = "…"           # default: {stage_dir}/upper.redb
# stage_dir = "…"               # default: {storage}/.overlay/{session}/
# auto_apply = false            # review then apply
```

### Overlay model

```text
target (real FS) ──RO──┐
                       ├─► merged (Agent cwd)
upper.redb (deltas) ───┘

Attempt ends → unmount, keep upper.redb
  → runtime overlay status|apply|discard
```

The upper is one exclusive backend: either a redb file or a directory tree.
The default redb backend stores contents and metadata directly and never
creates a parallel materialized upper directory. Use `backend = "directory"`
with optional `upper_dir`/`work_dir` for the traditional layout.

### Embedded overlay runtime

pVisor links the `persisting-overlayfs` crate and owns its background FUSE
session directly. The pVisor process is the userspace filesystem server; it
does not spawn `persisting-overlayfs` or `fuse-overlayfs`.

```bash
# macOS
brew install --cask macfuse   # + enable kext on Apple Silicon
cargo build -p persisting-pvisor --release
```

The standalone `persisting-overlayfs` binary remains available only for
diagnostics and fuse-overlayfs-compatible manual mounts.

The macOS implementation supports multi-layer merge, metadata-preserving
copy-up, whiteouts/opaque directories, lower-directory rename, links, xattrs,
directory snapshots and synchronization/statistics operations. pVisor's
`overlay apply` path preserves symlinks, hard links, modes, ownership,
timestamps and xattrs, and processes opaque markers before staged children.

## Usage

```rust
let pvisor = PVisor::builder()
    .capture_config("proxy.toml")
    .capture_output_dir("./store")
    .build();
let handle = pvisor.run(spec).await?;
let result = handle.wait().await?;
```

CLI:

- `persisting agent execute -c FILE -o DIR --overlay-target PATH -- <cmd>`
- `persisting runtime overlay status|apply|discard -o DIR --id run-…`
- `persisting traj capture …` — thin wrapper over the same API
