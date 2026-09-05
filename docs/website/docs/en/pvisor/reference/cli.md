# `pvisor` command reference

`pvisor` is the product command for a single Run and for durable
environments.
Full command examples for Host, OCI VM, and transparent host-rootfs VM
are in
[Run workloads with pVisor](../guides/execution.md).

```text
pvisor
├── run                 execute one Agent Run
├── replay              replay and continue an Agent-native trajectory
├── env                 manage durable reusable environments
├── status              aggregate Run, filesystem, and network state
├── inspect             open a read-only Run view
├── review              review the durable Run Bundle
├── checkpoint          snapshot a stopped transactional upper
├── fork                start a child Run from a logical checkpoint
├── apply               commit a stopped Run's filesystem stage
└── drop                discard a stopped Run's filesystem stage
```

## Safe first run

```bash
pvisor -- codex
pvisor review last
```

Host execution uses safe-best-effort isolation by default. `--stage <PATH>` opts
into an OverlayFS stage for the current workspace, creates an independent Run
and writable stage at the supplied path (or in the generated Run record
directory when no explicit stage is supplied),
retains changes for manual review, and writes `run-bundle.json` with mode `0600`.
On Linux, the default host executor self-executes through pVisor's rootless
launcher before the async runtime reaches the Agent. User/mount/PID namespaces,
an in-namespace PID 1 descendant reaper,
minimal bind-projected root plus `chroot`, a kernel-negotiated Landlock ABI v1-v3 policy, closed
inherited descriptors, `no_new_privs`, and an empty capability set make
workspace containment non-bypassable for the Agent process tree.
`--overlaynet-deny-all` adds a private network namespace; the
public/allowlist proxy modes remain cooperative. On macOS the default safe
host executor installs a generated Seatbelt policy that makes staged writes
non-bypassable. For deny-all Runs it blocks IP and ambient host Unix sockets,
while retaining the exact AgentCtl and Run-local IPC. Reads and selective
network policy remain ambient/cooperative and are labeled separately in the
Bundle. Docker and KVM transports retain the same outer Run, OverlayFS,
AgentCtl state observation, and pChronicle control plane.

After completion:

```bash
pvisor review last
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
pvisor apply last --all # or: pvisor drop last
```

The CLI checkpoint is stopped-consistent. Embedded hosts can call
`RunHandle::checkpoint`: pVisor publishes an AgentCtl quiesce directive,
requires every Session frozen into the checkpoint to report the matching
quiesced state, snapshots the raw upper, then publishes `continue`. Logical
checkpoints preserve filesystem and cooperative client safe-point boundaries,
not process memory.

A durable environment has a stable name and a reusable OverlayFS upper:

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env stop dev
pvisor env start dev
pvisor env apply dev --path src   # commit the selection; the rest stays staged
pvisor env apply dev --all        # commit remaining changes and reset to an empty stage
pvisor env drop dev        # discard changes and reset to an empty stage
pvisor env delete dev --force
```

Default metadata lives in `~/.persisting/envs` and can be overridden
with `--root` or `PERSISTING_ENV_HOME`. `start` / `stop` control whether
new sessions are accepted; they do not mean a resident VM. Each
`exec` / `shell` mounts the same writable upper, so changes persist
across commands. `inspect` uses a kernel-enforced read-only view.
`apply --all` or `drop` do not flip a terminal Overlay back to `staged`
in place; they create a monotonically increasing Overlay generation.
After a command takes the environment lease it re-reads the generation
so metadata from before the reset cannot overwrite the new stage.

## Replay an Agent trajectory

`pvisor replay` assumes the caller has normally created a fresh sandbox. It
replays complete tool batches through `after_step`, rebuilds the selected
Agent native context with fresh observations, and then starts the live Agent:

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
```

OpenHands, mini-swe-agent, Pi agent, and SWE-agent use the model endpoint and
credentials already present in their environment. Pi requires its exact
`0.83.0` runtime and accepts native RPC event JSONL containing the core
`read`, `bash`, `edit`, and `write` tools. Claude Code uses a temporary bridge owned
by SandboxReplay because its native resume transport inserts wake-up messages.
The bridge validates and removes that exact Resume Transport envelope before
forwarding the model request. It does not enable pVisor Gateway, capture model
traffic, or persist a bridge audit.

The equivalent strict replay TOML is:

```toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "Review the fresh observation before continuing."
```

Pi uses the same CLI/TOML surface. Its default SweEval entrypoint is
`/opt/pi-agent/bin/pi`, for example:

```bash
pvisor replay --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
```

Replay has three modes. The default replays the prefix and continues;
`--replay-only` executes the prefix and stops before a model request; and
`--prepare-only` constructs the prefix without executing tools or requiring a
runtime. `--max-steps` is the total action budget, including replayed actions.
`--allow-stale-observations` is an explicit Claude-only escape hatch that marks
the v3 result `degraded`.

`--boundary-user-prompt TEXT` appends one user message after the final fresh
observation and before the first live model inference. The TOML spelling is
`replay.boundary_user_prompt`. It is ignored for inference in prepare-only and
replay-only modes, and an omitted option preserves the unmodified replay
boundary. Structured results and replay journals store only injection state,
length, and a digest; Agent-native prepared or continued trajectories may
contain the user message.

The result schema is `sandbox-playback.result/v3`, with typed `phase`, `quality`,
and `agent_status` fields plus state/output locations, artifacts, and an optional
structured failure. Existing non-Claude callers that used `replay_only = true`
only to construct a prefix must migrate to `prepare_only = true`.

`disable_thinking` belongs to `[replay]` and is also exposed as
`--disable-thinking`; it is applied by the Claude protocol bridge without
turning on Gateway capture. Optional `[run]`, `[overlayfs]`, and `[overlaynet]` sections create an outer
managed `pvisor run`; they do not change the inner replay model path.

By default, replay's internal state, WAL, manifest, fresh-observation
comparisons, and native working files remain under
`/tmp/pvisor-sandbox-replay` and disappear with the sandbox. Replay does not
enable pVisor Gateway, pChronicle, a model-traffic capture store, or a Claude
Resume Transport audit. A caller that explicitly selects `--state-dir` or
`--output-dir` owns those files. Use `--replay-only` to execute the prefix and
stop before live inference, or `--prepare-only` to construct it without execution.

## One configuration model

`pvisor run` has one canonical `RunConfig`. TOML and command-line options are
two representations of the same fields. `--spec` is optional and explicit; a
file beginning with a JSON object is treated as a prepared RunSpec, otherwise
it is read as TOML RunConfig. pVisor does not discover a hidden project file.

```bash
pvisor run \
  --name codex \
  --overlayfs-path /workspace \
  --overlayfs-compose /path/to/project \
  --overlayfs-backend directory \
  --overlayfs-commit manual \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --record-format lance \
  --record-destination ./warehouse \
  -- codex
```

`--record-format lance` starts `pchronicle serve --control 127.0.0.1:0 DATASET`;
pVisor sends shared `EventRecord` values and waits for durable
acknowledgements. Use `--record-format json` for local JSONL or a JSON warehouse
archive.

All newly persisted records contain both `timestamp` (RFC3339 UTC) and
`timestamp_unix_ms` (Unix milliseconds). They describe the same observation
time and must agree within one millisecond. Record ordering remains defined by
`source + seq`; timestamps are correlation metadata rather than the ordering
source of truth.

The equivalent TOML is:

```toml
[run]
agent = "codex"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"
image = "example/codex-agent:latest"
network = "host"

[overlayfs]
path = "/workspace"
compose = ["/path/to/project"]
backend = "directory"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"
level = "dialogue"

[[gateway.routes]]
name = "openai"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[chronicle]
mode = "lance"
```

Run it with `pvisor run --spec run.toml`. Explicit CLI scalars replace TOML
scalars. Supplying any repeated CLI field (`--overlayfs-compose`,
`--overlaynet-allow`, `--overlaynet-deny`, `--overlaynet-limit`, or
`--gateway-route`) replaces that complete TOML list.
The command after `--` replaces `run.command`.

`--container-image IMAGE` selects the OCI container executor automatically;
`--executor container` makes the choice explicit. The transport resolves a
matching static `linux-amd64`/`linux-arm64` pVisor, mounts it into the image,
overrides the entrypoint, and invokes the normal
`pvisor run --executor host --spec ...` path. The Agent command is carried
inside the RunSpec rather than exposed in OCI runner argv. The injected
pVisor creates its own AgentCtl and returns a typed RunResult. The final
OverlayFS cwd and session Gateway configuration are mounted at stable paths.
User mounts are repeatable TOML inline tables, for example:

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

The in-process Gateway and explicit OverlayNet proxy currently require
`container.network = "host"`, because their injected addresses are host
loopback endpoints. Bridge and no-network modes are valid when these drivers
are off. The executor records container isolation but does not claim full
capability enforcement.

`--executor vm` uses statically linked libkrun and its embedded init to boot a
minimal Linux guest. `--rootfs image=IMAGE` selects this executor and pulls an
OCI/Docker image directly, without invoking Docker, Podman, or Buildah. When no
explicit rootfs is supplied, the default is `ubuntu:latest`. Manifests
and layer digests are verified, the host architecture selects `linux/arm64` or
`linux/amd64`, and the unpacked rootfs becomes the immutable lower layer of a
pVisor OverlayFS. `--image-store` overrides the platform cache directory.
OCI cache targets are marked immutable, and this protection survives logical
checkpoint/fork, so `pvisor apply` cannot mutate a rootfs shared by other Runs.

On Linux, `--rootfs host` selects the host `/` as the VM rootfs lower and
selects the VM executor when `--executor` is omitted. `--rootfs <PATH>` selects
a prepared directory and `--rootfs image=<PATH>` selects an OCI image or image
path. These forms are mutually exclusive, and host rootfs is rejected on macOS.
The OverlayFS view is selected with `--overlayfs-path`, the absolute path the
Agent sees. Repeat `--overlayfs-compose` to layer host directories in
bottom-to-top order; the current workspace remains the implicit bottom layer.
When `--overlayfs-path` is omitted, the current workspace is used as the view
source and pVisor places the merged mount in a managed per-Run path to avoid
mounting over its own lower directory.
With a guest workspace path, writes outside that workspace use a temporary
root upper and are discarded when the VM exits; workspace changes use the
durable OverlayFS stage.

The merged rootfs is guest `/`, and `/workspace` becomes the guest cwd. On both
Linux and macOS, a vendored libkrun serves pVisor's rootfs and workspace
copy-on-write unions directly over virtio-fs. The VMM never re-exports a host
FUSE mount and does not materialize or reconcile either tree. Linux uses
KVM and Apple Silicon macOS uses HVF through the same executor. libkrunfw is
installed beside pVisor in wheels. Source builds otherwise download the pinned
official release into a SHA-256-verified platform cache; on macOS `/usr/bin/cc`
turns its prebuilt kernel bundle into the required dylib. A system directory can
still be selected with `--vm-library-dir`. OverlayNet `auto` uses the
non-bypassable VM smoltcp IPv4 TCP/DNS driver, while Gateway capture uses an
internal route through the guest virtual router. Linux additionally confines
the VMM with namespaces and Landlock. The macOS VMM still has the invoking
user's host permissions, so the first OCI-image version must not be treated as
a hostile multi-tenant boundary despite the guest-kernel isolation.

On host/container execution, the four visible OverlayNet policy flags and
Gateway capture automatically enable the proxy driver. Any `--overlayfs-path`,
`--overlayfs-compose`,
`--stage`, `--overlayfs-backend`, or `--overlayfs-commit` option
automatically enables OverlayFS; no separate mode switch exists. The workspace
is the implicit base; explicit compose layers are applied in the order given.
An explicit `--stage` is the unified record
directory for metadata, trajectory, and filesystem state. When a stage is nested inside a base or compose layer, pVisor hides
that subtree from the merged view and rejects guest attempts to recreate it.
libkrun Runs create no live host mountpoint, preventing host indexers from
recursively entering `<stage>/merged`. The reverse topology, where a
stage contains a lower layer, is rejected. Both
`commit=apply` and the later `pvisor apply` command are rejected
for composed Runs until pVisor can materialize a complete merged-vs-base diff
safely.
On host/container execution, OverlayNet policy applies to traffic routed
through the explicit proxy and does not claim non-bypassable host network
isolation. On a libkrun VM, `auto` attaches non-bypassable smoltcp IPv4
TCP/DNS; `off` leaves the guest offline. `--overlaynet-deny-all` supplies the
same default-deny policy to the active driver. Host/container direct sockets
remain ambient, while a VM Gateway route remains available through the guest's
virtual router for configured model traffic.

## Run project discovery

The current directory is the default project association. When OverlayFS is
enabled, `--overlayfs-compose` identifies reusable host layers and
`--overlayfs-path` identifies the Agent-visible view. Each Run receives an
independent directory under pVisor's default records root. If that root would be inside
the selected OverlayFS base or a compose layer, pVisor instead uses the system
temporary Run root to keep the writable stage disjoint:

```text
project/                         # reusable workspace / default base

~/.persisting/runs/
└── run-<uuid>/                  # one generated Run and default stage
    ├── run.json
    ├── run-bundle.json          # mode 0600; outcome + safety + changes + effects
    ├── overlay.json             # when OverlayFS is enabled
    ├── upper/                   # or a Run-named Jujutsu workspace upper
    ├── merged/
    ├── checkpoints/
    ├── lease.lock
    ├── control.sock             # while a live OverlayFS Run is available
    ├── .capture/                # when OverlayNet/Gateway is enabled
    └── chronicle/               # default pChronicle location
```

Lifecycle commands accept a Run id, Run directory, project workspace,
`run.json`, upper, or merged path. A project workspace selects its latest Run:

```bash
pvisor status /path/to/project
pvisor inspect /path/to/project -- rg TODO .
pvisor apply /path/to/project --all
pvisor apply /path/to/project --path src --path tests/unit
pvisor apply /path/to/project --include 'docs/**' --exclude 'docs/generated/**'
pvisor apply /path/to/project --target /path/to/another-target --all
pvisor drop /path/to/project
```

`inspect` creates a separate kernel-read-only view. `apply` and `drop` refuse
to mutate a live Run. A filtered apply is dependency-closed and repeatable:
unselected paths remain staged, while opaque directories and hard-link groups
remain atomic. Each successful batch is persisted in `apply-ledger.json`.
The overlay records a durable first-touch fingerprint for every mutated target
path. `apply` fails closed if a selected target path changed after staging;
prepared batches recover forward, and individual non-directory replacements
commit with a same-directory atomic rename. The host filesystem still provides
no single atomic commit point for an arbitrary multi-file batch.
Applying all remaining changes or dropping the stage is terminal; `drop` cannot
undo already applied batches, and `apply` cannot recover discarded changes.
Terminal cleanup removes `upper`, `work`, and other disposable staging data but
retains compact Run/Overlay metadata, the apply ledger, and pChronicle history.

## Related workflows

- [Run your first Agent](../get-started.md) for the shortest complete loop.
- [Execution environments](../guides/execution.md) for choosing a provider.
- [Review and apply changes](../guides/review-apply.md) for filtered, repeatable apply.
- [Network control](../guides/network.md) and [capture](../guides/capture.md) for other Effect dimensions.
