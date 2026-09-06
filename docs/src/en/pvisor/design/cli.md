# pVisor command model

The `pvisor` command is the public entry point for one governed Agent Run. Its
interface is organized around four responsibilities: start a Run, inspect its
record, decide what happens to staged changes, and manage reusable environments.
The command line and `RunConfig` describe the same model; a configuration file
is an explicit input, never an implicit project policy.

## Start with `run`

The short form is intentionally equivalent to the explicit form:

```bash
pvisor -- codex
pvisor run --stage ./runs/task-001 -- codex
```

Use `--stage` when filesystem changes must remain available for review. Without
a stage, pVisor still records a Run, but there is no durable workspace change set
to apply. The selected host, container, or VM provider records its effective
capabilities and limitations in the Run Bundle.

Common controls are grouped by purpose:

| Purpose | Options | Result |
| --- | --- | --- |
| Workspace | `--stage`, `--overlayfs-path`, `--overlayfs-compose` | create a copy-on-write view and retain a changeset |
| Runtime | `--executor host\|container\|vm`, `--rootfs`, `--container-image` | select the execution provider and root filesystem |
| Network | `--overlaynet-deny-all`, `--overlaynet-allow`, `--overlaynet-limit` | request deny, allowlist, or rate-limit policy |
| Gateway | `--gateway-mode`, `--gateway-route`, `--gateway-level` | route and optionally capture model traffic |
| Limits | `--timeout`, `--memory`, `--max-processes`, `--max-open-files` | constrain the Attempt where the provider supports it |
| Configuration | `--spec`, `--name`, `--pass-env` | provide a prepared RunSpec, identity, and explicit environment |

Provider selection does not change the Run contract. It changes the mechanism
used to enforce each capability dimension, and the resulting evidence is
reported separately.

## Inspect and decide

A completed Run remains a record until its staged effects are explicitly
accepted or discarded:

```bash
pvisor review last
pvisor inspect last -- git status --short
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
# or: pvisor drop last
```

`review` explains the Run Bundle and staged changes. `inspect` executes a
read-only command against the Run view. `apply` commits a selected path set and
keeps the remainder staged; `drop` discards the stage. Neither operation
rewrites a live Run. A reset creates a new stage generation so stale metadata
cannot replace a newer decision.

## Checkpoints and forks

Checkpoints are stopped-consistent filesystem and AgentCtl safe points. They do
not claim to capture process memory:

```bash
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
```

Use checkpoints to preserve a known workspace state before a new attempt. The
checkpoint protocol waits for participating AgentCtl sessions to quiesce,
records the upper layer and lineage, then resumes the Run.

## Reusable environments

`env` gives a named stage a stable lifecycle across commands:

```bash
pvisor env create dev --target ./project
pvisor env exec dev -- make test
pvisor env shell dev
pvisor env inspect dev -- git status --short
pvisor env apply dev --path src
pvisor env drop dev
pvisor env delete dev --force
```

An environment is a persistent stage, not a resident VM. `start` and `stop`
control whether new sessions are accepted. `apply` and `drop` advance the stage
generation after a decision.

## Configuration precedence

`--spec` accepts TOML `RunConfig` or prepared JSON `RunSpec`. Explicit scalar
options override file values. Repeated list options replace the complete list,
and the command after `--` replaces `run.command`. `--container-image` and
`--rootfs` may infer the matching executor; an explicit `--executor` remains
clearer in automation.

Keep the public workflow small: start a Run, inspect its evidence, then make an
explicit decision about staged effects. Detailed provider behavior belongs to
[execution environments](../guides/execution.md), while the complete option
surface belongs to the [CLI reference](../reference/cli.md).
