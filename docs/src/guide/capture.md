# Capture Agent Trajectories

Use `pvisor run` for one managed Agent Run. Use `persisting traj proxy` only when
several independently started clients need to share a long-running Gateway.

References: [pVisor CLI](../design/cli-pvisor.md), [Traj CLI](../design/cli-traj.md),
[Gateway architecture](../design/gateway.md).

## Build

```bash
cargo build --release -p persisting-pvisor -p persisting-cli -p persisting-engine
export PATH="$(pwd)/target/release:$PATH"
export PERSISTING_ENGINE_LIB="$(pwd)/target/release/libpersisting_engine.dylib" # .so on Linux
```

## Local walkthrough

The repository includes a mock OpenAI-compatible model, a two-turn Agent, and
an AgenticMD validator:

```bash
cd examples/capture-walkthrough
./run.sh
```

The script prints the generated path. pVisor assigns the Run ID, so do not
hard-code a session directory or `0001.md` filename.

## Run a real Agent

Provide the upstream route and its API key directly:

```bash
export DEEPSEEK_API_KEY=sk-...

pvisor run \
  --workspace ./store/run \
  --agent deepseek \
  --overlaynet-mode proxy \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

Replace `claude` with `codex` or any program that uses an injected proxy/base
URL. pVisor starts the in-process Gateway, injects the child environment, waits
for the child, and stops the Gateway.

### Storage mode

| Option | Writes | Use when |
|---|---|---|
| `--gateway-stream-markdown` | Live AgenticMD projection | You want a lightweight human-readable run |
| `--chronicle-mode lance` | Canonical structured events | You need complete events, judgment, or derived views |

Generate AgenticMD from a Lance run with `persisting traj materialize`.

## Long-running Gateway

Start a foreground proxy:

```bash
persisting traj proxy -o ./store -c examples/llm-proxy/deepseek.toml -f markdown
```

The startup banner prints the proxy address and environment variables. Export
them in the terminal that starts the Agent. For a background daemon:

```bash
persisting traj proxy start -o ./store -c examples/llm-proxy/deepseek.toml -f markdown
persisting traj proxy status
persisting traj proxy list
persisting traj proxy stop
```

Do not run `pvisor run` and a standalone proxy against the same storage root at
the same time. The standalone proxy does not provide pVisor's process or
OverlayFS lifecycle.

## Inspect trajectories

```bash
# Discover every Story under one Agent directory.
persisting traj stats ./store/<agent-id> --detail

# Replay a particular Run directory.
persisting traj replay ./store/<agent-id>/<run-id>
persisting traj replay ./store/<agent-id>/<run-id> --storage-format markdown

# Build the human-readable view from canonical Lance events.
persisting traj materialize ./store \
  --agent-id <agent-id> \
  --root-session-id <run-id> \
  --session-id <session-id>
```

`stats`, `replay`, `materialize`, and `truncate` may omit the storage argument
after `traj proxy start`, or when `PERSISTING_CAPTURE_STORAGE` is set.

## Layout

```text
store/
├── .capture/                 # Gateway runtime metadata and failure records
└── agent-id/
    └── run-id/
        ├── events.lance/     # with --chronicle-mode lance
        ├── run-id.md         # with --gateway-stream-markdown or after materialize
        └── agent-<id>.md     # optional subagent Story
```

Legacy `0001.md` and `.tlv.md` files remain readable, but new examples and new
writes use session-named AgenticMD files.

## Troubleshooting

| Problem | Check |
|---|---|
| Agent cannot reach Gateway | Use the injected child via `pvisor run`, or export the standalone proxy banner variables |
| Codex bypasses the proxy | Pass the printed `-c openai_base_url=...` setting |
| No Markdown with Lance output | Enable `--gateway-stream-markdown` or run `traj materialize` |
| Failed capture event | Inspect `.capture/dead_letter.jsonl`, then use `traj replay-dead-letter` |
| `pvisor run` reports an active owner | Stop `traj proxy`, wait for the live Run, or use another storage root |

The independent dlcapt implementation has its own configuration and storage
model; see `crates/persisting-dlcapt/README.md` when working on that component.
