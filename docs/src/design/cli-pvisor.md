# `pvisor` command

`pvisor` is the foreground command for one Agent Run:

```text
pvisor
├── run       (default) execute one Agent Run
├── status              aggregate Run, filesystem, and network state
├── inspect             open a read-only Run view
├── apply               commit a stopped Run's filesystem stage
└── drop                discard a stopped Run's filesystem stage
```

## One configuration model

`pvisor run` has one canonical `RunConfig`. TOML and command-line options are
two representations of the same fields. `--config` is optional and explicit;
pVisor does not discover a hidden project configuration file.

```bash
pvisor run \
  --workspace /tmp/my-run \
  --agent codex \
  --overlayfs-mode overlay \
  --overlayfs-target /path/to/project \
  --overlayfs-backend redb \
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
backend = "redb"
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
├── upper.redb        # or upper/
├── merged/
├── lease.lock
├── control.sock       # while a live OverlayFS Run is available
├── .capture/          # when OverlayNet/Gateway is enabled
└── chronicle/         # default pChronicle location
```

Lifecycle commands accept a workspace, `run.json`, `upper.redb`, upper or
merged path:

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
