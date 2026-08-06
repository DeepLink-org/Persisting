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

`--safe` creates a durable workspace automatically, stages changes through
OverlayFS, enables the cooperative OverlayNet proxy, and writes a private
versioned `run-bundle.json`. With the default host executor it is a
review-oriented low-privilege profile, not a VM boundary: the Agent can still
reach host paths outside the staged workspace and bypass the proxy with direct
sockets. Docker and KVM executors inject the matching static Linux pVisor and
run the same ProcessExecutor inside a stronger placement boundary. pVisor
records the selected executor and its actual boundary in the Run Bundle.

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
drivers. Each live Run also gets a versioned, owner-only Agent ABI Unix socket.
The separate OverlayFS control socket remains limited to discovery and
owner-mediated read-only inspection.

## Modules

| Module | Role |
|--------|------|
| `pvisor` | [`PVisor`] / [`PVisorBuilder`] / [`RunHandle`] |
| `agent_abi` | Run-scoped Agent ABI server, desired state, and observations |
| `config` | canonical `RunConfig` plus programmatic driver configuration |
| `runtime` | Attempt preparation and driver ownership |
| `control` | Re-export of the shared `persisting-control` state protocol |
| `process` | Host process executor |
| `artifact` | target-specific static pVisor runtime discovery |
| `delegated` | RunSpec/RunResult hand-off between pVisor placements |
| `container` | Docker/Podman transport that injects pVisor |
| `kvm` | QEMU/KVM transport that copies pVisor over SSH |

## Agent ABI

pVisor injects a Run-scoped endpoint and bearer token into every process
invocation:

```text
PERSISTING_AGENT_ABI_ENDPOINT=/tmp/pvisor-agent-….sock
PERSISTING_AGENT_ABI_TOKEN=…
PERSISTING_AGENT_ABI_VERSION=2
PERSISTING_AGENT_ABI_TRANSPORT=unix
```

The token is intentionally not written to Run metadata. The socket is mode
`0600`, exists only for the Attempt lifetime, and accepts bounded JSON frames.
Docker and KVM placements start a complete pVisor inside the isolation
boundary. That injected pVisor creates the Agent ABI locally and executes the
Agent through the same ProcessExecutor used by a native Run; the host ABI token
is deliberately removed from the delegated RunSpec.
The compact protocol is owned by pVisor and currently uses the injected Unix
socket directly. The v2 handshake authenticates the client and opens a session.
Heartbeats return pVisor's current desired state (`continue`, `quiesce`, or
`shutdown`). Quiesce acknowledgements must match the active directive
generation and the server's open-effect view.

Hosts use `RunHandle::agent_abi()` to publish desired state and inspect the
registered clients, processes, and effects. pPilot exposes `AgentAbiClient` for
the client side.

## Runtime configuration

pVisor owns one canonical `RunConfig`. TOML and command-line options map to the
same fields; runtime drivers consume the resolved in-memory value and never
re-read a Gateway-specific file:

```toml
[run]
workspace = "/tmp/my-run"
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
mode = "overlay"
target = "/path/to/project"
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
CLI form keeps the Run workspace local while offloading the canonical event log:

```bash
AWS_REGION=us-east-1 pvisor run \
  --workspace /tmp/run-001 \
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
The executor discovers the matching `linux-amd64` or `linux-arm64` static
pVisor, mounts it read-only at `/opt/persisting/pvisor`, overrides the image
entrypoint, and executes `pvisor run --executor host --run-spec ...`. The Agent
command never appears in the Docker/Podman argument list. The injected pVisor
returns a typed RunResult through a private mounted control directory. The
transport maps cancellation to `stop` followed by `kill` when necessary.

Packaged runtimes are discovered under
`~/.persisting/runtimes/<version>/<platform>/pvisor`, under the installation
prefix's `libexec/persisting/<version>/`, or through
`PERSISTING_PVISOR_RUNTIME_DIR`. `--container-pvisor-binary` overrides discovery.
The final OverlayFS cwd and per-session Gateway configuration are mounted at
their existing paths. Additional mounts use `--container-mount
'source="/host/path", target="/container/path", read_only=true'`.
The injected pVisor currently runs as container root; `container.user` is
rejected until Agent identity can be applied after pVisor bootstrap rather than
to the control process itself. A read-only rootfs receives a private `/tmp`
tmpfs for the inner Agent ABI socket.

`network = "host"` is the default because Gateway and OverlayNet currently run
inside pVisor and advertise loopback endpoints. Bridge or no-network modes are
available when those drivers are disabled. Container placement is reported as
real isolation, but `PolicyMode::Enforce` remains unavailable until every
Persisting capability is translated into an OCI runtime restriction.

### KVM executor

The KVM executor boots a Linux qcow2/raw image with QEMU `-enable-kvm`, forwards
a loopback host port to guest SSH, copies the matching static pVisor and
prepared RunSpec into the guest, and runs the same `pvisor run --executor host`
path. A host cwd is exposed through QEMU 9p and mounted at
`/run/persisting/workspace`; the VM disk defaults to snapshot mode.

```bash
pvisor run \
  --executor kvm \
  --kvm-image /var/lib/persisting/agent.qcow2 \
  --kvm-ssh-key ~/.ssh/persisting-kvm \
  --kvm-pvisor-binary ./dist/pvisor-linux-amd64 \
  --kvm-memory-mib 4096 \
  --kvm-cpus 4 \
  -- agent --help
```

The image must boot with SSH enabled and accept the configured key (root is the
default because mounting the 9p workspace requires privilege). KVM execution
requires a Linux host and `/dev/kvm`; Gateway/OverlayNet host-loopback injection
is rejected until a guest-visible transport is implemented. Cancellation or a
transport watchdog tears down the entire VM.

### Overlay model

```text
target (real FS) ──RO──┐
                       ├─► merged (Agent cwd)
upper/ (deltas) ───────┘

Attempt ends → unmount, keep upper/
  → pvisor status|inspect|apply|drop
```

The upper is one exclusive backend: a directory tree or a named Jujutsu
workspace. Directory is the default backend and accepts optional
`upper_dir`/`work_dir` paths.

Use `backend = "jujutsu"` with `jujutsu_store` and `jujutsu_workspace` in the
pVisor run configuration when multiple Attempts should keep independent heads
in one shared `jj-lib` repository. The CLI selects both with one stage address,
for example `--overlayfs-stage jj:/tmp/shared.jj@fork-a`. Reusable environments
can use the same model directly:

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

OverlayNet-only Runs do not require `--workspace`; pVisor uses an internal
directory below the system temporary directory. Supply `--workspace` when the
Run record and Bundle must remain at a stable, user-selected path.

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
- `pvisor run --executor kvm --kvm-image IMAGE --kvm-ssh-key KEY -- <agent>`
- `pvisor run --workspace DIR [DRIVER OPTIONS] -- <agent>`
- `pvisor run --config run.toml [OVERRIDES] [-- <agent>]`
- `pvisor review [RUN|WORKSPACE] [--json]`
- `pvisor checkpoint [RUN|WORKSPACE] [--name NAME]`
- `pvisor fork RUN --checkpoint NAME --workspace DIR -- <agent>`
- `pvisor status [RUN|STAGE|UPPER]`
- `pvisor inspect [RUN|STAGE|UPPER] [-- COMMAND...]`
- `pvisor apply|drop [RUN|STAGE|UPPER]`

Each Run writes `run.json`, `lease.lock`, and (while live) `control.sock` next
to `overlay.json`. Completed CLI Runs also write a mode-`0600`
`run-bundle.json` containing outcome, safety boundary, filesystem summary,
network profile, Agent ABI clients/processes/effects, output, metrics, and
artifact references. `review` presents this bundle as an approval-oriented
summary. `status` remains the lower-level live diagnostic view.

`checkpoint` copies the raw upper into `checkpoints/<id>/` only after a Run is
stopped. `RunHandle::checkpoint` is the live API: it requests Agent ABI
quiescence, waits for every connected client to acknowledge the same directive
generation with no open effects, snapshots the upper, and resumes clients.
Both are logical Agent checkpoints; neither claims to preserve process memory.
`fork` restores one of these checkpoints into a new directory upper and starts
a new safe Run whose `run.json` and Run Bundle record the parent Run and
checkpoint.

Capture is a Gateway capability, not pVisor's component identity.
