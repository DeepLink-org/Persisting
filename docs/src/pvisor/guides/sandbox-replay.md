# SandboxReplay

SandboxReplay is pVisor's Agent-trajectory replay capability. It normally runs
inside a fresh sandbox created by the caller, re-executes the complete tool
batches before a selected boundary, rebuilds the Agent-native context with the
fresh observations, and then continues the Agent directly after that boundary.


## Replay boundary

A trajectory can be written as:

```text
task -> A1 -> O1 -> ... -> AN -> ON -> A(N+1) -> ...
```

SandboxReplay executes `A1...AN` in the fresh sandbox to produce
`O'1...O'N`, retains the Agent-native system prompt, tools, task, and actions,
and issues the first continued model request immediately after `O'N`. No
"continue" message is appended. This preserves the replay boundary but does
not guarantee that `A'(N+1)` is byte-identical to `A(N+1)`.

## Run replay

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude
```

The equivalent TOML is:

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
```

Runtime isolation is opt-in. Replay only creates an outer managed `pvisor run`
when the caller supplies runtime options such as `--safe`, `--executor`, or
`--overlayfs-base`, or their TOML equivalents. See the
[`pvisor replay` reference](../reference/cli.md#replay-an-agent-trajectory) for
the complete surface.

## Qwen3.6 evaluation

The evaluation used Qwen3.6-35B-A3B with thinking disabled. Each original run
and continuation used a newly created sandbox with 2 CPUs, 7 GiB memory, and
70 GiB storage. Step counts are native tool batches recognized by the Rust
parser. Text similarity compares normalized visible text and excludes
reasoning. Exact tools require equal counts, order, names, and JSON arguments.

| Agent | Task | N | Original steps | Continued steps | Original Reward | Continued Reward | Exact A'(N+1) tools | Text similarity |
|---|---|---:|---:|---:|---:|---:|---|---:|
| Claude Code | NodeBB (291) | 1 | 69 | 101 | 1 | 1 | yes | 0.85 |
| Claude Code | Vuls (666) | 28 | 76 | 200 | 0 | 0 | no | 0.45 |
| Claude Code | qutebrowser (667) | 36 | 46 | 46 | 1 | 1 | no | 0.48 |
| OpenHands | NodeBB (291) | 28 | 62 | 68 | 1 | 0 | yes | 1.00 |
| OpenHands | Vuls (666) | 25 | 43 | 43 | 1 | 1 | yes | 1.00 |
| OpenHands | qutebrowser (667) | 17 | 36 | 87 | 1 | 1 | no | 0.13 |
| mini-swe-agent | NodeBB (291) | 48 | 104 | 115 | 1 | 1 | no | 1.00 |
| mini-swe-agent | Vuls (666) | 39 | 94 | 91 | 1 | 1 | yes | 1.00 |
| mini-swe-agent | qutebrowser (667) | 31 | 115 | 65 | 1 | 1 | no | 0.77 |

## Boundary-response examples

The Chinese localization of this page contains all nine paired `A(N+1)` and
`A'(N+1)` responses, including visible text, tool names, and arguments. Use the
language switcher to open that detailed report. Reasoning is removed, and long
file replacements are reduced to their distinguishing targets.
