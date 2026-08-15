# `pvisor` 命令参考

`pvisor` 是单个 Run 和持久环境的产品命令。
Host、OCI VM 和透明 host-rootfs VM 的完整命令示例见
[使用 pVisor 运行工作负载](../guides/execution.md)。

```text
pvisor
├── run                 execute one Agent Run
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

`--safe` uses the current directory as the reusable project workspace and
OverlayFS base, creates an independent Run and writable stage under
`PERSISTING_RUN_HOME` (default `~/.persisting/runs`), retains changes for
manual review, enables the explicit OverlayNet proxy on ephemeral loopback
ports, and writes `run-bundle.json` with mode `0600`.
On Linux, the default host executor self-executes through pVisor's rootless
launcher before the async runtime reaches the Agent. A user/mount namespace,
minimal bind-projected root plus `chroot`, Landlock ABI v3 policy, closed
inherited descriptors, `no_new_privs`, and an empty capability set make
workspace containment non-bypassable for the Agent process tree.
`--overlaynet-deny-all` adds a private network namespace; the
public/allowlist proxy modes remain cooperative. On macOS the default safe
host executor installs a generated Seatbelt policy that makes staged writes
non-bypassable. For deny-all Runs it blocks IP and ambient host Unix sockets,
while retaining the exact Agent ABI and Run-local IPC. Reads and selective
network policy remain ambient/cooperative and are labeled separately in the
Bundle. Docker and KVM transports retain the same outer Run, OverlayFS, Agent
ABI observation, and pChronicle control plane.

After completion:

```bash
pvisor review last
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
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
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env stop dev
pvisor env start dev
pvisor env apply dev --path src   # 提交选中部分，其余继续 staged
pvisor env apply dev --all        # 提交剩余修改并重置为空 stage
pvisor env drop dev        # 丢弃修改并重置为空 stage
pvisor env delete dev --force
```

默认元数据位于 `~/.persisting/envs`，可用 `--root` 或 `PERSISTING_ENV_HOME`
覆盖。`start` / `stop` 控制是否接受新会话，并不表示常驻虚拟机；每次 `exec` / `shell`
都会挂载同一个 writable upper，所以修改会跨命令保留。`inspect` 使用内核强制的只读视图。
`apply --all` 或 `drop` 不会把 terminal Overlay 原地改回 `staged`；它们会创建单调递增的
Overlay generation。命令取得环境 lease 后会重新读取 generation，避免用 reset 前的
metadata 覆盖新 stage。

## One configuration model

`pvisor run` has one canonical `RunConfig`. TOML and command-line options are
two representations of the same fields. `--config` is optional and explicit;
pVisor does not discover a hidden project configuration file.

```bash
pvisor run \
  --agent codex \
  --overlayfs-base /path/to/project \
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
agent = "codex"
executor = "container"
command = ["codex"]

[container]
runtime = "docker"
image = "example/codex-agent:latest"
network = "host"

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
scalars. Supplying any repeated CLI field (`--overlayfs-compose`,
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

`--executor vm` uses statically linked libkrun and its embedded init to boot a
minimal Linux guest. `--image IMAGE` selects this executor and pulls an
OCI/Docker image directly, without invoking Docker, Podman, or Buildah. When no
explicit `--vm-rootfs` is supplied, the default is `ubuntu:latest`. Manifests
and layer digests are verified, the host architecture selects `linux/arm64` or
`linux/amd64`, and the unpacked rootfs becomes the immutable lower layer of a
pVisor OverlayFS. `--image-store` overrides the platform cache directory.
OCI cache targets are marked immutable, and this protection survives logical
checkpoint/fork, so `pvisor apply` cannot mutate a rootfs shared by other Runs.

On Linux, `--host-rootfs` selects the host `/` as the VM rootfs lower and
selects the VM executor when `--executor` is omitted. It is mutually exclusive
with `--image` and `--vm-rootfs`, and is rejected on macOS. This is a distinct
semantic option rather than a CLI alias: `--overlayfs-base` and
`--overlayfs-target` continue to select the project workspace independently.
With a guest workspace target, writes outside that workspace use a temporary
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
still be selected with `--vm-library-dir`. OverlayNet/Gateway is rejected for
this executor until a cross-platform guest relay is available. Linux additionally confines the VMM
with namespaces and Landlock. The macOS VMM still has the invoking user's host
permissions, so the first OCI-image version must not be treated as a hostile
multi-tenant boundary despite the guest-kernel isolation.

The four visible OverlayNet policy flags and Gateway capture automatically enable the
proxy driver. Any `--overlayfs-base`, `--overlayfs-compose`,
`--overlayfs-stage`, `--overlayfs-backend`, or `--overlayfs-commit` option
automatically enables OverlayFS; no separate mode switch exists. The base
defaults to the workspace, and the stage defaults to the generated Run
directory. When a stage is nested inside a base or compose layer, pVisor hides
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
enabled, `--overlayfs-base` identifies the reusable project directory. Each Run receives an
independent directory under `PERSISTING_RUN_HOME`. If that root would be inside
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
pvisor apply /path/to/project
pvisor apply /path/to/project --path src --path tests/unit
pvisor apply /path/to/project --include 'docs/**' --exclude 'docs/generated/**'
pvisor apply /path/to/project --target /path/to/another-target
pvisor drop /path/to/project
```

`inspect` creates a separate kernel-read-only view. `apply` and `drop` refuse
to mutate a live Run. A filtered apply is dependency-closed and repeatable:
unselected paths remain staged, while opaque directories and hard-link groups
remain atomic. Each successful batch is persisted in `apply-ledger.json`.
Applying all remaining changes or dropping the stage is terminal; `drop` cannot
undo already applied batches, and `apply` cannot recover discarded changes.
Terminal cleanup removes `upper`, `work`, and other disposable staging data but
retains compact Run/Overlay metadata, the apply ledger, and pChronicle history.

## Related workflows

- [Run your first Agent](../get-started.md) for the shortest complete loop.
- [Execution environments](../guides/execution.md) for choosing a provider.
- [Review and apply changes](../guides/review-apply.md) for filtered, repeatable apply.
- [Network control](../guides/network.md) and [capture](../guides/capture.md) for other Effect dimensions.
