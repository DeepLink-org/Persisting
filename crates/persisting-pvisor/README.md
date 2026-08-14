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
ambient host Unix sockets while retaining the exact Agent ABI and Run-local
IPC. Seatbelt setup is attested by the hidden launcher and fails closed.
Container and VM executors remain stronger placement choices, and every Run Bundle records
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
`persisting-agentctl` is the shared state/transition protocol used by runtime
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
| `control` | Re-export of the shared `persisting-agentctl` state protocol |
| `process` | Host process and Linux rootless executor |
| `artifact` | target-specific static pVisor runtime discovery |
| `delegated` | RunSpec/RunResult hand-off between pVisor placements |
| `container` | Docker/Podman transport that injects pVisor |
| `vm` | libkrun VM backend over pVisor's full-root OverlayFS |

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
Docker and VM placements start a complete pVisor inside the isolation
boundary. That injected pVisor creates the Agent ABI locally and executes the
Agent through the same ProcessExecutor used by a native Run; the host ABI token
is deliberately removed from the delegated RunSpec.
The compact protocol is owned by pVisor and currently uses the injected Unix
socket directly. The v2 handshake authenticates the client and opens a session.
Heartbeats return pVisor's current desired state (`continue`, `quiesce`, or
`shutdown`). Quiesce acknowledgements must match the active directive
generation and the server's open-effect view.

Hosts use `RunHandle::agent_abi()` to publish desired state and inspect the
registered clients, processes, and effects. The reusable
`persisting-agentctl` crate owns the client SDK; pPilot re-exports it for
compatibility and remains the reference quiescence/effect integration.

## Runtime configuration

pVisor owns one canonical `RunConfig`. TOML and command-line options map to the
same fields; runtime drivers consume the resolved in-memory value and never
re-read a Gateway-specific file:

```toml
[run]
executor = "container"
command = ["codex"]
inherit_env = false
pass_env = ["EXPLICIT_NON_GATEWAY_TOKEN"]

[run.resource_limits]
memory_bytes = 2147483648
processes = 128
cpu_time_ms = 600000
open_files = 1024
file_size_bytes = 1073741824

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

Library callers select the same network boundary with
`PVisorBuilder::network(NetworkDriverConfig::new(mode, network))`. `Auto`
attaches `vm-smoltcp` to a `VmExecutor`, `Off` leaves that guest offline, and
`Proxy` remains the cooperative host/container driver. A Gateway configured on
the same builder shares the Attempt controller, metrics, and bandwidth
registry with VM egress.

`chronicle.dir` accepts either a local directory or an S3 URI. The equivalent
CLI form keeps the reusable project workspace local while offloading the canonical event log:

```bash
AWS_REGION=us-east-1 pvisor run \
  --overlayfs-base /path/to/project \
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

Container runtimes use `--container-pvisor-binary`; libkrun uses an explicit
`--vm-rootfs` and does not require a second pVisor binary in that rootfs.
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

### VM executor

The `vm` executor statically links libkrun and its guest init into pVisor. The
same implementation boots a minimal Linux guest through KVM on Linux and HVF
on Apple Silicon macOS. pVisor can pull an OCI/Docker image directly from its
registry, without Docker, Podman, or Buildah, and cache its verified layers as
an immutable rootfs. The default image is `ubuntu:latest` when neither
`vm.rootfs` nor `--vm-rootfs` is supplied.

Linux can instead reuse the host userland while changing kernels:

```bash
pvisor run \
  --executor vm \
  --host-rootfs \
  --overlayfs-base /home/reiase/workspace \
  --overlayfs-target /home/workspace \
  --overlayfs-stage /tmp/pvisor-host-rootfs-stage \
  --overlayfs-commit manual \
  -- /bin/bash
```

`--host-rootfs` is Linux-only and mutually exclusive with `--image` and
`--vm-rootfs`. It serves host `/` as the guest rootfs lower through virtio-fs;
rootfs writes use a temporary upper, while the separately targeted workspace
uses the durable stage.

```bash
pvisor run \
  --image ubuntu:latest \
  --overlayfs-base . \
  --overlayfs-target /workspace \
  --overlayfs-stage /tmp/pvisor-stage \
  --overlayfs-commit manual \
  -- /bin/bash -lc 'uname -a'
```

The staged view of `--overlayfs-base` appears at `--overlayfs-target`, which is
also the guest cwd. On Linux and macOS, libkrun serves pVisor's copy-on-write
union directly over virtio-fs; no host FUSE mount, full-tree snapshot, or exit
reconciliation is involved. With
`--overlayfs-commit manual`, the host base remains unchanged and guest writes
remain under the configured stage. The image rootfs uses its own copy-on-write
upper, so system writes never modify the cached image. `--image-store` selects an
explicit content-addressed cache; otherwise pVisor uses the platform cache
directory. Apple Silicon automatically selects `linux/arm64`, while x86-64
Linux selects `linux/amd64`.

An already prepared directory remains supported:

```bash
pvisor run \
  --executor vm \
  --vm-rootfs /opt/persisting/rootfs \
  --vm-memory-mib 4096 \
  --vm-cpus 4 \
  --overlayfs-base ./project \
  --overlayfs-target /workspace \
  -- /usr/bin/agent --help
```

Linux requires `/dev/kvm`; macOS requires Apple
Silicon, macOS 14 or newer, and the Hypervisor entitlement included in packaged
builds. libkrunfw remains a runtime payload: wheels install it beside `pvisor`,
while source builds automatically download the pinned official release into the
platform cache and verify its SHA-256. On macOS the downloaded kernel bundle is
compiled with `/usr/bin/cc`; `--vm-library-dir` can still select a system copy.
Building from source on macOS requires Zig (`brew install zig`) to cross-compile
libkrun's embedded Linux guest init. After a local `cargo build`, sign the
development binary before using HVF:

```bash
just pvisor          # release build + macOS signing
just pvisor debug    # debug build + macOS signing

# Equivalent manual signing:
codesign --force --sign - \
  --entitlements crates/persisting-pvisor/macos-hypervisor.entitlements \
  target/debug/pvisor
```

The guest has no ambient network path around pVisor. In the default
`[overlaynet] mode = "auto"`, libkrun virtio-net is attached to pVisor's
in-process smoltcp backend. It serves `192.0.2.2/24` by DHCP, answers DNS with
stable synthetic `198.18.0.0/15` addresses, and terminates/re-originates IPv4
TCP only after the hostname/IP and port pass Control policy. TCP and DNS are
non-bypassable for the guest process tree; general UDP, IPv6, ICMP, QUIC, and
inbound forwarding are deliberately unsupported and fail closed. `mode =
"off"` leaves the VM offline. Gateway capture, when configured, is exposed at
the virtual router address instead of its host loopback address.

On Linux the self-exec VMM runner additionally enters pVisor's
namespace/Landlock confinement. On macOS the VM provides a guest-kernel
boundary, but the VMM still runs with the invoking user's host permissions;
because libkrun's virtio-fs security model requires host-side confinement, this
first version is not a hostile multi-tenant boundary on macOS.

Rootfs upper layers remain normal review/checkpoint/fork/drop artifacts.
Explicit prepared rootfs targets may also be applied; OCI cache targets are
persistently marked immutable, including across checkpoint/fork, so `apply`
cannot corrupt a rootfs shared by later Runs. The Run Bundle records the rootfs
target and staged change summary.

### Overlay model

```text
base (real FS) ───────────RO──┐
compose layers (optional) ────┼─► merged (Agent cwd)
stage/upper (deltas) ─────────┘

Attempt ends → unmount, keep upper/
  → pvisor status|inspect
  → apply or drop (terminal; clean upper/work, retain audit metadata)
```

Any OverlayFS option enables the driver. `base` defaults to the project
workspace, `stage` defaults to the generated per-Run directory, and repeated
`compose` paths form read-only layers above the base. Directory is the default
backend. A stage nested inside `base` or a compose layer is automatically
excluded from the merged view, so the guest cannot observe or recreate it. A
stage that contains a lower layer remains invalid. For a nested stage, the live
merged mountpoint is placed in the Run storage outside the lower tree, avoiding
recursive traversal by host indexers and file watchers.

With `backend = "jujutsu"`, pVisor creates a Run-named Jujutsu
workspace in `<stage>/jujutsu`. Reusable environments can use the same backend
directly:

```bash
pvisor env create experiment-a --backend jujutsu --target ./project
pvisor env create experiment-b --backend jujutsu --target ./project
```

Both environments use `<env-root>/.jujutsu` by default but retain separate
working-copy commits and directory uppers.

### Embedded overlay runtime

pVisor links the `persisting-overlayfs` crate for host-process Runs and owns its
background FUSE session directly. libkrun Runs instead use the same portable
overlay core inside libkrun's virtio-fs server and do not require macFUSE.

```bash
# macOS
brew install --cask macfuse   # + enable kext on Apple Silicon
cargo build -p persisting-pvisor --release
```

The standalone `persisting-overlayfs` binary remains available only for
diagnostics and manual mounts.

### Network enforcement

Host and container runs retain the explicit proxy: coverage is opt-in and
observe-grade. VM Auto uses the non-bypassable `vm-smoltcp` driver. Run records
distinguish the two profiles and persist DNS/TCP flow, denial, failure, byte,
active/peak flow, and unsupported-packet counters. Gateway and VM egress share
the Attempt policy controller and bandwidth buckets.

The accepted Linux host-process design for non-bypassable interception — an
unprivileged network namespace whose only egress is a pVisor-owned in-process
userspace stack, with a seccomp user-notify + `ADDFD` fallback — is specified in
`docs/src/design/overlaynet.md`. That host-process path remains future work.
VM `auto` already satisfies network-only `PolicyMode::Enforce` on Linux and
Apple Silicon macOS; unsupported VM hosts and cooperative host/container proxy
paths remain observe-only.

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

The current directory is the default project association. An explicit
`--overlayfs-base` can be shared by any number of Runs. Run records and Bundles normally remain independent under
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
- `pvisor run --image IMAGE [--image-store DIR] -- <agent>`
- `pvisor run --host-rootfs [--overlayfs-target GUEST_PATH] -- <agent>` (Linux only)
- `pvisor run --executor vm [--vm-rootfs DIR] [--vm-library-dir DIR] -- <agent>`
- `pvisor run --overlayfs-base DIR [DRIVER OPTIONS] -- <agent>`
- `pvisor run --config run.toml [OVERRIDES] [-- <agent>]`
- `pvisor review [RUN|WORKSPACE] [--json|--diff]`
- `pvisor checkpoint [RUN|WORKSPACE] [--name NAME]`
- `pvisor fork RUN --checkpoint NAME -- <agent>`
- `pvisor status [RUN|STAGE|UPPER]`
- `pvisor inspect [RUN|STAGE|UPPER] [-- COMMAND...]`
- `pvisor apply|drop [RUN|STAGE|UPPER]`

Each Run writes `run.json`, `lease.lock`, and (while live) `control.sock` next
to `overlay.json`. Completed CLI Runs also write a mode-`0600`
`run-bundle.json` containing outcome, safety boundary, filesystem summary,
network profile, requested resource limits, environment-key projection,
classified filesystem changes, Agent ABI clients/processes/effects, output,
metrics, and artifact references. `review` presents the complete A/M/D/T/O
manifest; `--diff` adds bounded text diffs while marking binary, large,
symlink, and opaque changes structurally. `status` remains the lower-level live
diagnostic view.

`pvisor run --safe` disables full host-environment inheritance. A small
compatibility baseline is projected and additional host keys require repeated
`--pass-env NAME` or `run.pass_env`. Gateway upstream credentials stay in the
trusted Gateway; SDK-facing authentication variables, when required, contain
only a Run-scoped local placeholder. Bundles record names and provenance, never
environment values.

`checkpoint` copies the raw upper into `checkpoints/<id>/` only after a Run is
stopped. `RunHandle::checkpoint` is the live API: it requests Agent ABI
quiescence, waits for every connected client to acknowledge the same directive
generation with no open effects, snapshots the upper, and resumes clients.
Both are logical Agent checkpoints; neither claims to preserve process memory.
`fork` restores one of these checkpoints into a new directory upper and starts
a new safe Run whose `run.json` and Run Bundle record the parent Run and
checkpoint.

Capture is a Gateway capability, not pVisor's component identity.
