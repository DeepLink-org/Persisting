# `persisting execute` / `env`（pVisor）

面向用户的入口是 `persisting execute` 和 `persisting env`；它们分别转发到组件命令
`pvisor run` 和 `pvisor env`。直接使用 `pvisor` 适合组件部署与调试。

```text
pvisor
├── run       (default) execute one Agent Run
├── env                 manage durable reusable environments
├── status              aggregate Run, filesystem, and network state
├── inspect             open a read-only Run view
├── apply               commit a stopped Run's filesystem stage
└── drop                discard a stopped Run's filesystem stage
```

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
  --overlaynet-mode proxy \
  --overlaynet-policy allowlist \
  --overlaynet-allow api.openai.com \
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
command = ["codex"]

[overlayfs]
mode = "overlay"
target = "/path/to/project"
backend = "directory"
commit = "manual"

[overlaynet]
mode = "proxy"
policy = "allowlist"
allow = ["api.openai.com"]

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
`--overlaynet-allow`, or `--gateway-route`) replaces that complete TOML list.
The command after `--` replaces `run.command`.

The driver modes are explicit. Gateway capture requires OverlayNet proxy mode;
OverlayFS overlay mode requires `--workspace` and `--overlayfs-target`.
OverlayNet policy applies to traffic routed through the explicit proxy and does
not claim non-bypassable host network isolation.

## Run workspace and discovery

`--workspace` is the exact durable Run directory. pVisor never appends a hidden
Run-id child:

```text
workspace/
├── run.json
├── overlay.json       # when OverlayFS is enabled
├── upper/             # or a Jujutsu workspace upper
├── merged/
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
