# SandboxReplay

SandboxReplay is pVisor's Agent-trajectory replay capability. It normally runs
inside a fresh sandbox created by the caller, re-executes the complete tool
batches before a selected boundary, rebuilds the Agent-native context with the
fresh observations, and then continues the Agent directly after that boundary.
It creates a derived Run in the
[Run and Attempt model](../concepts/run-model.md).

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

An explicit boundary user prompt can instead make the first live request:

```text
system + tools + task + A1 -> O'1 -> ... -> AN -> O'N
    + boundary_user_prompt -> A'(N+1)
```

Use `--boundary-user-prompt TEXT` only when this changed input is intentional.
SandboxReplay injects it once, after `O'N` and before the first live model
inference. It never replaces the original task and is not injected in
prepare-only or replay-only mode. Without the option, the existing exact
boundary behavior is unchanged.

## Supported Agent profiles

Replay profiles are version-pinned and fail closed when the installed runtime
does not match. The Pi profile supports `@earendil-works/pi-coding-agent`
`0.83.0` and consumes the native RPC event JSONL produced by Pi. A replay step
is one complete `turn_end` tool batch. The profile reconstructs a fresh Pi v3
session with new observations, then continues through Pi's SDK. Its initial
tool surface is intentionally limited to Pi's `read`, `bash`, `edit`, and
`write` tools; trajectories containing another tool fail validation.

## Run replay

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
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
boundary_user_prompt = "Review the fresh observation before continuing."
```

For a Pi runtime installed at the SweEval default location, the CLI form is:

```bash
pvisor replay \
  --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
```

### Execution modes and results

- The default mode executes the selected prefix and continues with the live Agent.
- `--replay-only` executes the prefix and stops before the next model request.
- `--prepare-only` only validates and constructs the prefix. It executes no tools,
  starts no Agent, and does not require an Agent runtime.

`--max-steps` counts all Agent actions, including the selected prefix. For
example, `--after-step 30 --max-steps 50` leaves at most 20 live actions. A
replay-only budget must cover the prefix; a continuation budget must leave at
least one live action.

Results use `sandbox-playback.result/v3`. The `phase` is `prepared`, `replayed`,
or `continued`; `quality` is `verified` or `degraded`; and `agent_status`
distinguishes `not_started`, `completed`, `max_steps`, and `failed`. Failures
retain available logs and native trajectories. OpenHands controller fatal states
are failures even when its process exits with status zero.

Successful result metadata records whether the boundary prompt was requested
and injected, plus its character length and SHA-256 digest; replay journals do
not store the prompt text. Agent-native prepared or continued trajectories may
contain the user message. For Claude Code, the in-memory bridge adds the message
only to the first cleaned upstream request and leaves the reconstructed native
session unchanged. When it is injected,
`next-action-comparison.json` uses the input condition
`boundary_user_prompt_appended`. Its similarity and tool metrics remain
descriptive and must not be interpreted as same-input replay fidelity.

Migration: older non-Claude configurations sometimes used `replay_only = true`
to construct a prefix without executing it. Use `prepare_only = true` for that
behavior. In v3, replay-only always executes the selected prefix and therefore
requires an exact-version runtime. Claude observations that cannot be reproduced
fresh fail by default; `--allow-stale-observations` explicitly permits a
`degraded` result.

Runtime isolation is opt-in. Replay only creates an outer managed `pvisor run`
when the caller supplies runtime options such as `--executor`, `--stage`, or
`--overlayfs-path`/`--overlayfs-compose`, or their TOML equivalents. See the
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

For exact options, use the
[`pvisor replay` reference](../reference/cli.md#replay-an-agent-trajectory);
for execution boundaries, continue to the
[execution guide](execution.md).
