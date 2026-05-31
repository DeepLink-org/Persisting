# Capture Quick Start

Get **`persisting traj`** running in **5–10 minutes**: use `traj capture` or `traj proxy` to record agent sessions, then `traj stats` / `traj replay` to inspect Markdown trajectories.

> Architecture: [Capture Architecture](../design/capture_design.zh.md) (中文). CLI reference: [Traj command](../design/cli_trajectory_command.zh.md) and [Capture subcommands](../design/cli_capture_command.zh.md).

---

## What you get

`traj capture`:

1. Starts a local **LLM reverse proxy** (forwards to your upstream model).
2. Appends every request/response to a **Vortex event log** when using `-f vortex` (machine-readable, replayable).
3. By default **live-updates Markdown** (`-f md`, human-readable, `tail -f` friendly).
4. Prints a summary when the child exits and writes `.capture/reconcile.json`.

Supported live clients: **Claude Code**, **OpenAI Codex** (via proxy). Cursor is not supported yet.

---

## 0. Build the CLI

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
cargo build -p persisting-cli -p persisting-engine
```

```bash
export PERSISTING="$(pwd)/target/debug/persisting"
export PERSISTING_ENGINE_LIB="$(pwd)/target/debug/libpersisting_engine.dylib"  # .so on Linux
"$PERSISTING" traj --help
```

---

## 1. Fastest path: no API key

```bash
cd examples/capture-walkthrough
./run.sh
```

Output: `store/demo-agent/walkthrough-001/0001.md`

Manual two-terminal flow: see [examples/capture-walkthrough/README.md](../../examples/capture-walkthrough/README.md).

---

## 2. Real models: Claude Code / Codex

```bash
export DEEPSEEK_API_KEY=sk-...
```

```bash
persisting traj capture \
  -o ./store \
  -c examples/llm-proxy/deepseek.toml \
  -f md \
  -- claude
```

Codex: replace `claude` with `codex`. Custom agent: `... -- python3 your_agent.py`.

| Flag | Meaning |
|------|---------|
| `-o DIR` | Store root (default `.persisting/capture`) |
| `-c FILE` | Proxy TOML (`listen`, `models`, upstream) |
| `-f md` | Markdown only (default) |
| `-f vortex` | Vortex only (`events.vortex`); run `traj materialize` when you need `.md` |
| `--debug` | Log proxied requests to stderr + `.capture/debug.log` |

`traj capture` is **in-process** — no `traj proxy stop` needed when the child exits.

---

## 3. Long-running: `traj proxy` / `traj proxy start`

Use **`traj proxy`** (foreground) or **`traj proxy start`** (background) when multiple terminals share one proxy and store — not `traj capture` per session.

### 3.1 Which mode?

| Mode | Command | Proxy lifetime | Typical use |
|------|---------|----------------|-------------|
| One-shot | `traj capture` | Ends with child process | Scripts, CI |
| Foreground | `traj proxy` | Blocks until `Ctrl+C` | Local dev, debugging |
| Background | `traj proxy start` | Until `traj proxy stop` | Long-lived shared proxy |

Same `-c` / `-o` / `-f` for all three. **Do not** mix `traj proxy` (or `traj proxy start`) with `traj capture` on the same `-o`.

### 3.2 `traj proxy`

```bash
persisting traj proxy -o ./store -c examples/llm-proxy/deepseek.toml -f md
```

On startup the CLI prints **usage hints**: `proxy`, `admin`, `store`, `agent_id`, and the `export` lines below.

In **another terminal** (example `listen = 127.0.0.1:19081`):

```bash
export HTTP_PROXY=http://127.0.0.1:19081 HTTPS_PROXY=http://127.0.0.1:19081
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
export OPENAI_BASE_URL=http://127.0.0.1:19081/v1
export ANTHROPIC_BASE_URL=http://127.0.0.1:19081
claude
# Codex: codex -c 'openai_base_url="http://127.0.0.1:19081/v1"'
```

Sessions use a **flat** `{agent_id}/{session_id}/` layout (unlike `run-{timestamp}/` from `traj capture`).

Stop with **`Ctrl+C`** in the `traj proxy` terminal.

### 3.3 `traj proxy start` (daemon)

```bash
persisting traj proxy start -o ./store -c examples/llm-proxy/deepseek.toml -f md
persisting traj proxy status   # live connections
persisting traj proxy list     # history, tokens, cost
persisting traj proxy stop
```

Omit `-o` on `list` / `status` / `stop` when using the last `start` dir or `PERSISTING_CAPTURE_STORAGE`.

`--debug` → logs to `{store}/.capture/daemon.log`.

### 3.4 vs `traj capture`

| | `traj capture` | `traj proxy` / `traj proxy start` |
|--|----------------|-----------------------------------|
| Env injection | Automatic for child | Manual `export` in new terminal |
| Codex | Auto `-c openai_base_url=…` | You must pass `-c` |
| Layout | `run-{timestamp}/` | Flat `{session_id}/` |
| Stop | Child exits | `Ctrl+C` or `traj proxy stop` |

Prefer **`traj capture`** for one session; **`traj proxy`** for multi-terminal dev.

---

## 4. Output layout

```text
store/
├── .capture/          # index, reconcile, dead letters
└── {agent_id}/
    └── run-{timestamp}-{nanos}/
        ├── run-*.md           # main agent dialogue
        ├── agent-*.md         # subagents (if any)
        └── events.vortex        # Vortex event log
```

```bash
tail -f store/deepseek-proxy/run-*/run-*.md
```

---

## 5. Inspect & repair

If you ran `traj proxy start` or set `PERSISTING_CAPTURE_STORAGE`, you can omit `<STORAGE>` on `stats`, `replay`, `materialize`, and `truncate`.

```bash
# Omit --session-id to scan all runs under agent/ and expand Vortex session_id partitions
persisting traj stats ./store/deepseek-proxy/ --detail
persisting traj replay ./store --agent-id deepseek-proxy --session-id run-...
persisting traj materialize ./store --agent-id ... --root-session-id ... --session-id ...
```

**Multimodal (screenshots / image generation):** with default `capture_level = dialogue`, images appear as **`[image: …]` / `[image_generated: …]` placeholders** in Markdown and stats—not embedded pixels. Set `capture_level = "full"` in TOML for raw JSON in Vortex. See [trajectory TLV §2.7 (zh)](../design/trajectory_tlv_format.zh.md#27-多模态对话正文phase-0).

---

## 6. Post-hoc import (optional)

```bash
persisting traj import ./store --provider ide --since-days 7 --project "$(pwd)"
```

---

## 7. Command cheat sheet

| Command | Purpose |
|---------|---------|
| `traj capture` | One-shot: proxy + child command |
| `traj proxy` | Foreground long-running proxy |
| `traj proxy start` / `stop` | Background daemon |
| `traj proxy list` / `status` | Sessions and live connections |
| `traj import` | Post-hoc IDE / gateway import |
| `traj replay-dead-letter` | Replay failed capture events |
| `traj stats` | Stats; `--detail` per-turn tree |
| `traj replay` | Event JSON replay |
| `traj materialize` | Vortex → Markdown rebuild |

---

## 8. Troubleshooting

| Issue | Action |
|-------|--------|
| Agent cannot reach proxy | `export` env vars from startup banner in a **new** terminal; Codex needs `-c openai_base_url=…` |
| md vs Vortex mismatch | `traj materialize` after checking `.capture/reconcile.json` |
| Failed capture events | `traj replay-dead-letter -o ./store` |
| `traj capture` conflicts with daemon | `traj proxy stop` or use another `-o` |
| `stats` shows 0 turns | Scan **`./store/{agent_id}/`** without `--session-id` so CLI expands all Vortex `session_id` partitions; or pass the specific header UUID with `--session-id` |

---

## Next steps

- [Capture architecture](../design/capture_design.zh.md)
- [Trajectory Markdown format](../design/trajectory_tlv_format.zh.md)
- [Traj CLI](../design/cli_trajectory_command.zh.md)
- [Walkthrough example](../../examples/capture-walkthrough/README.md)
