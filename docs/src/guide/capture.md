# Capture Agent Trajectories

Use `persisting execute` for one managed Agent Run. Use `persisting gateway serve` only when
several independently started clients need to share a long-running Gateway.

References: [pVisor CLI](../design/cli-pvisor.md), [History / Eval / Gateway CLI](../design/cli-history.md),
[Gateway architecture](../design/gateway.md).

## Build

```bash
cargo build --release -p persisting-pvisor -p persisting-cli
export PATH="$(pwd)/target/release:$PATH"
```

## Local walkthrough

The repository includes a mock OpenAI-compatible model and a two-turn Agent.
The quantitative example verifies upstream requests, Gateway counters, and
AgenticMD blocks:

```bash
cd examples/pvisor/04-gateway-llm-control
./run.sh
```

The script prints the generated path. pVisor assigns the Run ID, so do not
hard-code a session directory or Markdown filename.

## Run a real Agent

Provide the upstream route and its API key directly:

```bash
export DEEPSEEK_API_KEY=sk-...
export PERSISTING_RUN_HOME=$HOME/.persisting/runs

pvisor run \
  --workspace . \
  --agent deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

Replace `claude` with `codex` or any program that uses an injected proxy/base
URL. pVisor starts the in-process Gateway, injects the child environment, waits
for the child, and stops the Gateway. The workspace is reusable; each execution
writes an independent `run-<uuid>` directory below `PERSISTING_RUN_HOME`.

### Storage mode

| Option | Writes | Use when |
|---|---|---|
| `--gateway-stream-markdown` | Live AgenticMD projection | You want a lightweight human-readable run |
| `--chronicle-mode lance` | Canonical structured events | You need complete events, judgment, or derived views |

Generate AgenticMD from a Lance run with `persisting history materialize`.

## Long-running Gateway

Start a foreground proxy:

```bash
persisting gateway serve -o ./store \
  -c examples/pvisor/04-gateway-llm-control/configs/deepseek.toml -f markdown
```

The startup banner prints the proxy address and environment variables. Export
them in the terminal that starts the Agent. For a background daemon:

```bash
persisting gateway start -o ./store \
  -c examples/pvisor/04-gateway-llm-control/configs/deepseek.toml -f markdown
persisting gateway status
persisting gateway list
persisting gateway stop
```

Do not run `pvisor run` and a standalone proxy against the same storage root at
the same time. The standalone proxy does not provide pVisor's process or
OverlayFS lifecycle.

## Inspect trajectories

```bash
# Discover every Story under one Agent directory.
persisting history stats ./store/<agent-id> --detail

# Replay a particular Run directory.
persisting history replay ./store/<agent-id>/<run-id>

# Build the human-readable view from canonical Lance events.
persisting history materialize ./store \
  --agent-id <agent-id> \
  --root-session-id <run-id> \
  --session-id <session-id>
```

`stats`, `replay`, `materialize`, and `truncate` may omit the storage argument
after `gateway start`, or when `PERSISTING_CAPTURE_STORAGE` is set.

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

Generated AgenticMD uses session-named files and Storyline-like block fields. The
reader also accepts legacy headers and plain Markdown because this is a debug
view, not a storage protocol.

## Troubleshooting

| Problem | Check |
|---|---|
| Agent cannot reach Gateway | Use the injected child via `pvisor run`, or export the standalone proxy banner variables |
| Codex bypasses the proxy | Pass the printed `-c openai_base_url=...` setting |
| No Markdown with Lance output | Enable `--gateway-stream-markdown` or run `history materialize` |
| Failed capture event | Inspect `.capture/dead_letter.jsonl`, then use `history replay-dead-letter` |
| `persisting execute` reports an active owner | Stop `gateway`, wait for the live Run, or use another storage root |

The independent dlcapt implementation has its own configuration and storage
model; see `crates/persisting-dlcapt/README.md` when working on that component.
