# pVisor

**Foreground Agent Run Manager and Portable Execution Runtime.**

pVisor is a first-class Persisting component alongside pPilot and pChronicle:

- pVisor owns one Run and its Attempts;
- pPilot plans and orchestrates many Runs;
- pChronicle stores canonical Run history and derived views.

```text
CLI / pPilot / host
        │  PVisor::builder()…build()
        │  PVisor::run(spec) → RunHandle
        ▼
pVisor
    │  prepare drivers: Gateway/OverlayNet + Control + OverlayFS
    │  execute Attempt
    │  teardown
        ▼
RunHandle::wait / cancel / events
```

There is no network control daemon. Hosts call the crate API directly;
`persisting-control` is the shared state/transition protocol used by runtime
drivers. A Run-local Unix socket exists only for discovery and owner-mediated
read-only inspection of a live OverlayFS workspace.

## Modules

| Module | Role |
|--------|------|
| `pvisor` | [`PVisor`] / [`PVisorBuilder`] / [`RunHandle`] |
| `config` | canonical `RunConfig` plus programmatic driver configuration |
| `runtime` | Attempt preparation and driver ownership |
| `control` | Re-export of the shared `persisting-control` state protocol |
| `process` | Host process executor |

## Runtime configuration

pVisor owns one canonical `RunConfig`. TOML and command-line options map to the
same fields; runtime drivers consume the resolved in-memory value and never
re-read a Gateway-specific file:

```toml
[run]
workspace = "/tmp/my-run"
command = ["codex"]

[overlayfs]
mode = "overlay"
target = "/path/to/project"
backend = "redb"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"
allow = ["api.openai.com"]

[gateway]
mode = "capture"

[[gateway.routes]]
name = "openai"
upstream = "https://api.openai.com/v1"
```

### Overlay model

```text
target (real FS) ──RO──┐
                       ├─► merged (Agent cwd)
upper.redb (deltas) ───┘

Attempt ends → unmount, keep upper.redb
  → pvisor status|inspect|apply|drop
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

### Network enforcement roadmap

Today's network driver is an explicit proxy: coverage is opt-in and
`RuntimeCapabilities.network` honestly reports observe-grade behavior. The
accepted Linux design for non-bypassable interception — an unprivileged
network namespace whose only egress is a pVisor-owned in-process userspace
stack (mirroring the embedded FUSE decision), with a seccomp user-notify +
`ADDFD` fallback for hosts without user namespaces — is specified in
`docs/src/design/overlaynet.md`. Once a transparent driver is attached,
`PolicyMode::Enforce` becomes satisfiable for network capabilities on Linux;
other hosts keep observe mode.

The macOS implementation supports multi-layer merge, metadata-preserving
copy-up, whiteouts/opaque directories, lower-directory rename, links, xattrs,
directory snapshots and synchronization/statistics operations. pVisor's
`apply` path preserves symlinks, hard links, modes, ownership,
timestamps and xattrs, and processes opaque markers before staged children.

## Usage

- `pvisor run --workspace DIR [DRIVER OPTIONS] -- <agent>`
- `pvisor run --config run.toml [OVERRIDES] [-- <agent>]`
- `pvisor status [RUN|STAGE|UPPER|DB]`
- `pvisor inspect [RUN|STAGE|UPPER|DB] [-- COMMAND...]`
- `pvisor apply|drop [RUN|STAGE|UPPER|DB]`

Each Run writes `run.json`, `lease.lock`, and (while live) `control.sock` next
to `overlay.json`. `status` uses these records to aggregate process, network,
and filesystem state. `inspect` creates a separate kernel-read-only view of the
same upper; the owning pVisor creates that view for a live Redb Run.

Capture is a Gateway capability, not pVisor's component identity.
