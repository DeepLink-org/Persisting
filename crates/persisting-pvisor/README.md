# pVisor

**Foreground Agent Run Manager and Portable Execution Runtime.**

The shortest product path is a transactional Agent Run:

```bash
pvisor run --safe codex
pvisor review last
# Then choose one:
pvisor apply last
pvisor drop last
```

`--safe` uses the current directory as a reusable project workspace, creates
an independent durable Run under `PERSISTING_RUN_HOME`, stages changes through
OverlayFS, enables the cooperative OverlayNet proxy, and writes a private
versioned `run-bundle.json`. On Linux, the host path self-executes through a
small launcher that installs an unprivileged user/mount namespace, constructs
a minimal bind-projected root, enters it with `chroot`, and applies Landlock
before running the Agent. The policy confines the complete descendant process
tree to the staged workspace, a read-only host runtime, and explicit
capabilities; unprojected host files and pathname Unix sockets are absent. It
fails closed instead of falling back to an ordinary host process. A deny-all
network request additionally creates a private network namespace; public and
allowlist proxy modes remain cooperative. On macOS, the host path wraps the
Agent in a generated Seatbelt profile: writes are kernel-confined to the staged
workspace, explicit read-write capabilities, exact device handles, and a
Run-owned temporary directory. Full-disk reads remain ambient for local
toolchain compatibility; `--overlaynet-deny-all` additionally blocks IP and
ambient host Unix sockets while retaining Run-local IPC. Seatbelt setup is
attested by the hidden launcher and fails closed.
Docker and KVM remain stronger placement choices, and every Run Bundle records
read, write, and network enforcement separately.

The default pVisor build is deliberately lightweight: directory OverlayFS and
the compact pChronicle event model do not link Lance, DataFusion, Jujutsu,
`prost`, or a protobuf toolchain. Full local Lance history and the Jujutsu
upper backend are explicit `lance-chronicle` and `jujutsu-overlay` features.

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

The Run id is also the Gateway root-session id. A Run becomes terminal only
after driver teardown, local RunRecord persistence, and terminal-event sink
acceptance; finalization failure is reported as a retryable infrastructure
failure rather than a successful Run with warnings.

There is no network control daemon. Hosts call the crate API directly;
`persisting-control` is the shared state/transition protocol used by runtime
drivers. The separate OverlayFS control socket remains limited to discovery
and owner-mediated read-only inspection.

## Modules

| Module | Role |
|--------|------|
| `pvisor` | [`PVisor`] / [`PVisorBuilder`] / [`RunHandle`] |
| `config` | canonical `RunConfig` plus programmatic driver configuration |
| `runtime` | Attempt preparation and driver ownership |
| `control` | Re-export of the shared `persisting-control` state protocol |
| `process` | Host process and Linux rootless executor |
| `artifact` | target-specific static pVisor runtime discovery |
| `delegated` | RunSpec/RunResult hand-off between pVisor placements |
| `container` | Docker/Podman transport that injects pVisor |
| `kvm` | libkrun/KVM guest over pVisor's full-root OverlayFS |

## Runtime configuration

pVisor owns one canonical `RunConfig`. TOML and command-line options map to the
same fields; runtime drivers consume the resolved in-memory value and never
re-read a Gateway-specific file:

```toml
[run]
workspace = "/path/to/project"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"                 # `podman` is also supported
image = "example/codex-agent:latest"
pvisor_binary = "/opt/persisting/pvisor-linux-amd64"
platform = "linux-amd64"
network = "host"                   # required by the in-process Gateway
read_only_rootfs = false

[[container.mounts]]
source = "/host/cache"
target = "/cache"
read_only = false

[overlayfs]
base = "/path/to/project"
backend = "directory"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]
transports = ["tcp_tunnel"]

[gateway]
mode = "capture"

[[gateway.routes]]
name = "openai"
upstream = "https://api.openai.com/v1"

[chronicle]
mode = "lance"
dir = "s3://trajectory-bucket/persisting/runs"
```

`chronicle.dir` accepts either a local directory or an S3 URI. The equivalent
CLI form keeps the reusable project workspace local while offloading the canonical event log:

```bash
AWS_REGION=us-east-1 pvisor run \
  --workspace /path/to/project \
  --chronicle-mode lance \
  --chronicle-dir s3://trajectory-bucket/persisting/runs \
  -- codex
```

The resulting dataset is
`s3://trajectory-bucket/persisting/runs/<agent>/<run-id>/events.lance`.
Credentials use the AWS provider chain and are not persisted in Run metadata.
The configured pChronicle writer receives both Gateway trajectory records and
pVisor `run.*` lifecycle records as the same canonical `EventRecord`; pVisor no
longer defines a second runtime event envelope outside pChronicle.

### Container executor

The command after `--` is resolved inside the image rather than against the
host `PATH`:

```bash
pvisor run \
  --executor container \
  --container-runtime docker \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  -- codex --help
```

Supplying `--container-image` selects the container executor automatically.
The executor requires a compatible `linux-amd64` or `linux-arm64` pVisor via
`--container-pvisor-binary`, mounts it read-only at `/opt/persisting/pvisor`, overrides the image
entrypoint, and executes `pvisor run --executor host --run-spec ...`. The Agent
command never appears in the Docker/Podman argument list. The injected pVisor
returns a typed RunResult through a private mounted control directory. The
transport maps cancellation to `stop` followed by `kill` when necessary.

Container and KVM runtimes must be configured explicitly with
`--container-pvisor-binary` or `--kvm-pvisor-binary`, respectively.
The final OverlayFS cwd and per-session Gateway configuration are mounted at
their existing paths. Additional mounts use `--container-mount
'source="/host/path", target="/container/path", read_only=true'`.
The injected pVisor currently runs as container root; `container.user` is
rejected until Agent identity can be applied after pVisor bootstrap rather than
to the control process itself. A read-only rootfs receives a private `/tmp`
tmpfs for the Agent process.

`network = "host"` is the default because Gateway and OverlayNet currently run
inside pVisor and advertise loopback endpoints. Bridge or no-network modes are
available when those drivers are disabled. Container placement is reported as
real isolation, but `PolicyMode::Enforce` remains unavailable until every
Persisting capability is translated into an OCI runtime restriction.

### KVM executor

The KVM executor uses `libkrun`, `libkrunfw`, and `libkrun_init` to boot a
minimal Linux guest without a disk image or SSH. pVisor mounts an OverlayFS
whose lower layer is the host `/`, exposes that merged mount as the guest root
through virtio-fs, and runs the command with the invoking host UID, GID, and
supplementary groups. Writes and whiteouts stay in the Run upper layer.

```bash
pvisor run \
  --executor kvm \
  --kvm-library-dir /opt/libkrun/lib \
  --kvm-memory-mib 4096 \
  --kvm-cpus 4 \
  -- agent --help
```

KVM execution requires a Linux host, `/dev/kvm`, and compatible libkrun
libraries. OverlayNet crosses the VM boundary through an explicit
vsock-to-Unix-socket relay; the VMM runs in private user, mount, and network
namespaces with Landlock restrictions, so the guest has no ambient host
network path. Guest `/proc`, `/sys`, `/dev`, `/run`, and `/tmp` are guest-local
mounts. The complete host root remains readable wherever the invoking UID can
read it, including credentials; this mode isolates the kernel, not identity or
secrets.

Full-root upper layers are review/checkpoint/fork artifacts. `pvisor apply`
refuses to replay them onto host `/`; use `pvisor checkpoint`, `pvisor fork`, or
`pvisor drop` instead. The Run Bundle records the root device/inode, host
UID/GID, excluded pVisor backing paths, and the staged change summary.

### Overlay model

```text
base (real FS) ───────────RO──┐
compose layers (optional) ────┼─► merged (Agent cwd)
stage/upper (deltas) ─────────┘

Attempt ends → unmount, keep upper/
  → pvisor status|inspect|apply|drop
```

Any OverlayFS option enables the driver. `base` defaults to the project
workspace, `stage` defaults to the generated per-Run directory, and repeated
`compose` paths form read-only layers above the base. Directory is the default
backend. With `backend = "jujutsu"`, pVisor creates a Run-named Jujutsu
workspace in `<stage>/jujutsu`. Reusable environments can use the same backend
directly:

```bash
pvisor env create experiment-a --backend jujutsu --target ./project
pvisor env create experiment-b --backend jujutsu --target ./project
```

Both environments use `<env-root>/.jujutsu` by default but retain separate
working-copy commits and directory uppers.

### Embedded overlay runtime

pVisor links the `persisting-overlayfs` crate and owns its background FUSE
session directly. The pVisor process is the userspace filesystem server and
does not spawn a separate overlay process.

```bash
# macOS
brew install --cask macfuse   # + enable kext on Apple Silicon
cargo build -p persisting-pvisor --release
```

The standalone `persisting-overlayfs` binary remains available only for
diagnostics and manual mounts.

### Network enforcement roadmap

Today's network driver is an explicit proxy: coverage is opt-in and
`RuntimeCapabilities.network` honestly reports observe-grade behavior. The
Run record now includes an OverlayNet interception profile (`explicit-proxy`,
`cooperative`) and the child receives `PERSISTING_OVERLAYNET_DRIVER` plus
`PERSISTING_OVERLAYNET_STRENGTH`. Gateway `/admin/status` exposes counters for
requests that actually reached the proxy; these counters never imply that
direct sockets were observed.

The accepted Linux design for non-bypassable interception — an unprivileged
network namespace whose only egress is a pVisor-owned in-process userspace
stack (mirroring the embedded FUSE decision), with a seccomp user-notify +
`ADDFD` fallback for hosts without user namespaces — is specified in
`docs/src/design/overlaynet.md`. Once a transparent driver is attached,
`PolicyMode::Enforce` becomes satisfiable for network capabilities on Linux;
other hosts keep observe mode.

Structured OverlayNet rules accept exact hosts, wildcard suffixes, IPs, and
CIDRs. Empty `ports` or `transports` mean unrestricted, so production policy
should usually state both. `http`, `https`, and `tcp_tunnel` are the available
transport values. Hostname rules deny private and loopback DNS results by
default; set `allow_private_ips = true` only for an intentional private
service. Link-local and other special-purpose destinations still need an
explicit IP/CIDR rule. The older `allow = [...]` host-only form remains
compatible but cannot scope ports or transports.

The common CLI surface uses three repeatable options and infers proxy mode and
the default policy automatically. Any `--overlaynet-allow` switches to default
deny; with deny rules alone, unmatched destinations remain allowed. Deny rules
always win. Global and target limits stack, so the strictest effective rate is
observed:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-deny bad.example.com \
  --overlaynet-limit 10mbps \
  --overlaynet-limit api.openai.com:443=2mbps \
  -- codex
```

`--workspace` defaults to the current directory and can be shared by any
number of Runs. Run records and Bundles normally remain independent under
`PERSISTING_RUN_HOME` (default `~/.persisting/runs`). If that Run Home would
sit inside an OverlayFS base or compose layer, pVisor uses the system temporary
Run root instead so the writable stage cannot overlap a read-only layer.

`kbps`/`mbps`/`gbps` use network-standard bits per second; `kb/s`/`mb/s`/`gb/s`
use bytes per second. Limits aggregate upload and download across matching
intercepted connections. They do not grant access.

When pVisor is launched by pPilot, `RunSpec` may contain an optional Supervisor
bootstrap. pVisor authenticates, registers `(run_id, attempt_id, lease_epoch)`,
and applies an initial network quota before starting OverlayNet. The connection
also accepts fenced runtime directives such as cancellation and emits
heartbeats. Failure to connect, or loss of the connection after startup, never
prevents the local Run from continuing; standalone `pvisor run` requires no
Supervisor. The Supervisor-delivered limit still covers only traffic that
reaches the explicit proxy.

The macOS implementation supports multi-layer merge, metadata-preserving
copy-up, whiteouts/opaque directories, lower-directory rename, links, xattrs,
directory snapshots and synchronization/statistics operations. pVisor's
`apply` path preserves symlinks, hard links, modes, ownership,
timestamps and xattrs, and processes opaque markers before staged children.

## Usage

- `pvisor run --safe <agent> [ARGS...]`
- `pvisor run --executor container --container-image IMAGE --container-pvisor-binary BIN -- <agent>`
- `pvisor run --executor kvm [--kvm-library-dir DIR] -- <agent>`
- `pvisor run --workspace DIR [DRIVER OPTIONS] -- <agent>`
- `pvisor run --config run.toml [OVERRIDES] [-- <agent>]`
- `pvisor review [RUN|WORKSPACE] [--json]`
- `pvisor checkpoint [RUN|WORKSPACE] [--name NAME]`
- `pvisor fork RUN --checkpoint NAME [--workspace PROJECT] -- <agent>`
- `pvisor status [RUN|STAGE|UPPER]`
- `pvisor inspect [RUN|STAGE|UPPER] [-- COMMAND...]`
- `pvisor apply|drop [RUN|STAGE|UPPER]`

Each Run writes `run.json`, `lease.lock`, and (while live) `control.sock` next
to `overlay.json`. Completed CLI Runs also write a mode-`0600`
`run-bundle.json` containing outcome, safety boundary, filesystem summary,
network profile, output, metrics, and artifact references. `review` presents
this bundle as an approval-oriented
summary. `status` remains the lower-level live diagnostic view.

`checkpoint` copies the raw upper into `checkpoints/<id>/` only after a Run is
stopped. It is a stopped-consistent filesystem checkpoint and does not preserve
process memory or claim application-level consistency.
`fork` restores one of these checkpoints into a new directory upper and starts
a new safe Run whose `run.json` and Run Bundle record the parent Run and
checkpoint.

Capture is a Gateway capability, not pVisor's component identity.
