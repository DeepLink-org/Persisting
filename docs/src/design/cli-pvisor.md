# `persisting execute` / `env`（pVisor）

面向用户的入口是 `persisting execute` 和 `persisting env`；它们分别转发到组件命令
`pvisor run` 和 `pvisor env`。直接使用 `pvisor` 适合组件部署与调试。

```text
pvisor
├── run       (default) execute one Agent Run
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
pvisor run --safe codex
pvisor review last
```

`--safe` chooses a durable workspace under `PERSISTING_RUN_HOME` (default
`~/.persisting/runs`), uses the current directory as the OverlayFS lower,
retains changes for manual review, enables the explicit OverlayNet proxy on
ephemeral loopback ports, and writes `run-bundle.json` with mode `0600`.
If that workspace root overlaps the selected lower (for example, running from
the home directory), pVisor uses the system temporary run root instead.
With the default host executor, the profile is deliberately described as
review-safe rather than sandboxed: filesystem access outside the project is
not contained, and direct sockets can bypass the cooperative proxy. Docker and
KVM transports inject a target-specific static pVisor and execute the same
ProcessExecutor inside their placement boundary while retaining the outer Run,
OverlayFS, Agent ABI observation, and pChronicle control plane.

After completion:

```bash
pvisor review last
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment \
  --workspace /tmp/experiment-b -- codex
pvisor apply last       # or: pvisor drop last
```

The CLI checkpoint is stopped-consistent. Embedded hosts can call
`RunHandle::checkpoint`: pVisor publishes an Agent ABI quiesce directive,
requires every connected client to acknowledge the checkpoint with no open
effects, snapshots the raw upper, then publishes `continue`. Logical
checkpoints preserve filesystem and Agent/effect boundaries, not process
memory.

持久环境拥有稳定名称和可复用 OverlayFS upper：

```bash
persisting env create dev --target ./project
persisting env exec dev -- make test
persisting env shell dev
persisting env inspect dev -- git status --short
persisting env stop dev
persisting env start dev
persisting env apply dev       # 提交修改并重置为空 stage
persisting env drop dev        # 丢弃修改并重置为空 stage
persisting env delete dev --force
```

默认元数据位于 `~/.persisting/envs`，可用 `--root` 或 `PERSISTING_ENV_HOME`
覆盖。`start` / `stop` 控制是否接受新会话，并不表示常驻虚拟机；每次 `exec` / `shell`
都会挂载同一个 writable upper，所以修改会跨命令保留。`inspect` 使用内核强制的只读视图。

## One configuration model

`persisting execute` / `pvisor run` has one canonical `RunConfig`. TOML and command-line options are
two representations of the same fields. `--config` is optional and explicit;
pVisor does not discover a hidden project configuration file.

```bash
pvisor run \
  --workspace /tmp/my-run \
  --agent codex \
  --overlayfs-mode overlay \
  --overlayfs-target /path/to/project \
  --overlayfs-backend directory \
  --overlayfs-commit manual \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --chronicle-mode lance \
  -- codex
```

The equivalent TOML is:

```toml
[run]
workspace = "/tmp/my-run"
agent = "codex"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"
image = "example/codex-agent:latest"
network = "host"

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

Run it with `pvisor run --config run.toml`. Explicit CLI scalars replace TOML
scalars. Supplying any repeated CLI field (`--overlayfs-lower`,
`--overlaynet-allow`, `--overlaynet-deny`, `--overlaynet-limit`, or
`--gateway-route`) replaces that complete TOML list.
The command after `--` replaces `run.command`.

`--container-image IMAGE` selects the OCI container executor automatically;
`--executor container` makes the choice explicit. The transport resolves a
matching static `linux-amd64`/`linux-arm64` pVisor, mounts it into the image,
overrides the entrypoint, and invokes the normal
`pvisor run --executor host --run-spec ...` path. The Agent command is carried
inside the RunSpec rather than exposed in Docker/Podman argv. The injected
pVisor creates its own Agent ABI and returns a typed RunResult. The final
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

`--executor kvm` uses the same delegation protocol through QEMU/KVM. It boots
a qcow2/raw Linux guest, forwards SSH over loopback, copies the static pVisor
and RunSpec, shares a host cwd through QEMU 9p, and retrieves the RunResult
before destroying the VM. The guest must have SSH enabled and accept the
configured key. KVM requires a Linux host; host Gateway/OverlayNet endpoints
are rejected until a guest-visible transport is available.

The three OverlayNet policy flags and Gateway capture automatically enable the
proxy driver. In TOML, driver modes remain explicit;
OverlayFS overlay mode requires `--workspace` and `--overlayfs-target`.
OverlayNet policy applies to traffic routed through the explicit proxy and does
not claim non-bypassable host network isolation.

## Run workspace and discovery

`--workspace` is the exact durable Run directory. pVisor never appends a hidden
Run-id child:

```text
workspace/
├── run.json
├── run-bundle.json    # mode 0600; outcome + safety + changes + effects
├── overlay.json       # when OverlayFS is enabled
├── upper/             # or a Jujutsu workspace upper
├── merged/
├── checkpoints/       # named logical upper snapshots
├── lease.lock
├── control.sock       # while a live OverlayFS Run is available
├── .capture/          # when OverlayNet/Gateway is enabled
└── chronicle/         # default pChronicle location
```

Lifecycle commands accept a workspace, `run.json`, upper or merged path:

```bash
pvisor status /tmp/my-run
pvisor inspect /tmp/my-run -- rg TODO .
pvisor apply /tmp/my-run
pvisor apply /tmp/my-run --target /path/to/another-target
pvisor drop /tmp/my-run
```

`inspect` creates a separate kernel-read-only view. `apply` and `drop` refuse
to mutate a live Run. `drop` affects only the staged filesystem upper and never
deletes pChronicle history.
