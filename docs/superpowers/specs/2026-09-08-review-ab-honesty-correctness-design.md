# Review batches A+B: honesty alignment and data correctness

## Goal

Address the highest-ROI items from
`docs/reviews/2026-09-08-persisting-overall-review.zh.md` §6 items 1–2, under the
**honest alignment** policy: change docs/UX to match current behavior. Do not
make staging default. Do not implement Subprocess enforcement for `--strict`.

## Decisions

| Topic | Choice |
| --- | --- |
| Staging default | Keep opt-in via `--stage`; fix README and run banner |
| `--strict` | Document fail-closed / currently unreachable on host/container/VM; no capability matrix change |
| `just test` mode | Align `AGENTS.md` to justfile (debug nextest), not the reverse |
| Cases document locale | `docs/src/zh/pchronicle/reference/cases-{self,platform}.md` |
| Anthropic tool args | Read `input` after `arguments` in `tool_call_part` |
| OpenAI unknowns | Story-local `/session_steps/{i}` after per-session `step_id` sort; tighten relocate |
| Control JSON append | File flock around local `events.jsonl` write+flush |

## Out of scope

- `capture_level` privacy gating (batch C)
- Default staging / reachable `--strict` enforce
- ppilot defer / container shared image cache / apply mid-drop
- DecodeContext byte limits across codecs
- English cases path duplication (zh only for justfile)

## A — Honesty alignment

### A1 Root README

Replace `pvisor run --safe` and “`--safe` stages workspace” with the same
contract as zh/en get-started:

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --all
```

State explicitly that without `--stage`, the agent may write the real
workspace; staged review requires `--stage`.

### A2 Run banner (`persisting-pvisor` `cli/run.rs`)

Keep `safe = true` (always-on best-effort profile). Change the
`eprintln!("… staged workspace + …")` path so that:

- **Overlay/stage active** → may claim staged workspace + network boundary text
- **No overlay** → claim best-effort isolation only; tell the user workspace
  writes are not staged and to pass `--stage` for COW/review

Rootless / Seatbelt boundary lines must not imply staged writes when overlay is
absent.

### A3 `--strict` documentation

Update en/zh `docs/src/{en,zh}/pvisor/reference/cli.md` (and tighten zh
`reference/cases.md` A05 wording if it still softens “not unreachable on all
executors”): current executors request Network + Subprocess enforcement;
none claim Subprocess → `--strict` fails closed with `UnsupportedPolicy` before
Agent start. Flag is for verifying fail-closed, not “stronger sandbox ready”.

### A4 Engineering drift

- `justfile` `test-pchronicle-cases*` / `cases pchronicle*`: document paths →
  `docs/src/zh/pchronicle/reference/cases-self.md` and `cases-platform.md`
- `AGENTS.md` Test command: say Rust tests use **debug** `cargo nextest` (as
  justfile already documents), not release mode

## B — Data correctness

### B1 Anthropic `tool_use.input`

In `persisting-gateway` `understanding.rs` `tool_call_part`:

1. Resolve arguments from `function.arguments` / `value.arguments`
2. Else `value.input` (Anthropic Messages `tool_use`)
3. Keep existing empty-string → `{}` and JSON-string parse behavior

Add a unit test that Messages `tool_use.input: {cmd: pwd}` yields
`ToolCall.arguments` with that object (existing parse test does not assert
arguments today).

### B2 OpenAI unknown-field pointer binding

Root cause: decode attaches unknowns/carriers with **global input ordinal**
under `/session_steps/{ordinal}`, while encode rebuilds per-session rows by
`step_id` order; `relocate_openai_unknown_pointer` keeps a pointer if any row
exists at that index.

Fix:

1. After grouping rows by `session_id`, sort that session’s rows by `step_id`
2. Capture unknowns and carrier bindings with **story-local** indices
   `0..n-1` matching encode order
3. Relocate: only rewrite when the source index is missing; do not treat
   “some row at index i” as identity of the original record. Prefer identity via
   local carrier list for that story; fail closed on ambiguous multi-match if
   needed rather than silent cross-session attach

Add a multi-session, out-of-order `step_id` round-trip test that vendor unknown
fields stay on the correct session row.

### B3 Control JSON trajectory append

In `persisting-pchronicle-cli` `control.rs` `append_json_trajectory` local
branch: hold an exclusive flock on `events.jsonl` for the duration of
serialize + write lines + flush (reuse `fs2` if already a dependency; otherwise
the crate’s existing file-lock helper). Object-store branch unchanged
(per-event puts; already documented as worker-serialized).

Add a concurrent append test (two tasks / threads writing distinct records)
asserting every line is valid JSON and record count matches.

## Testing

| Area | Command / check |
| --- | --- |
| Gateway tool input | `just test persisting-gateway` (or targeted lib test for `understanding`) |
| OpenAI unknowns | `just test persisting-pchronicle` targeted openai format tests |
| Control append | `just test persisting-pchronicle-cli` control/concurrency test |
| Banner / README | Manual or existing CLI help tests if they snapshot banner; README review |
| Cases path | `test -f` paths used by justfile; optional short `just test-pchronicle-cases` smoke |

## Success criteria

- Root README no longer mentions `--safe` as the default staging story
- Unstaged `pvisor run` does not print “staged workspace”
- `--strict` docs state current universal fail-closed for Subprocess gap
- justfile case docs resolve; AGENTS.md matches `just test` mode
- Anthropic Messages tool args preserved in semantic IR
- OpenAI multi-session unknown round-trip does not cross-attach
- Concurrent local JSON append does not interleave lines
