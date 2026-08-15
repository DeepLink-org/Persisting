# Persisting overview

Persisting turns an Agent command into a durable **Run** with an isolated
execution environment, reviewable effects, a stable identity, and a history
that survives the process.

That same Run model is used on a developer laptop and when many Runs are
orchestrated together.

![Persisting execution architecture](assets/diagrams/persisting/execution-story.svg)

## 1. Start with one Agent Run

An Agent needs more than a process. It needs a workspace, tools, network
access, credentials, state, and a boundary around the changes it can make.

`pvisor` creates that boundary. It is Persisting's implementation of the
[AgentVisor](pvisor/concepts/agentvisor.md) category: a hypervisor for Agent execution.
Each Run receives an Agent virtual execution environment while the underlying
host, container, VM, or fleet resources remain shareable.

```bash
pvisor run --safe codex
```

The command runs with a staged workspace and records the controls that were
actually installed. The Run identity does not depend on its process ID or
execution provider.

## 2. Separate execution from acceptance

Letting an Agent work and accepting its effects are different decisions.
Persisting keeps filesystem changes in a Run-owned stage so the Agent can work
without requesting approval for every edit.

After the Run, inspect the result and accept only what should enter the base
workspace:

```bash
pvisor review last
pvisor apply last --path src
pvisor apply last --include 'tests/**'
pvisor apply last --all
```

`apply` is intentionally repeatable. Every successful call consumes only the
selected dependency-closed batch; unselected changes remain staged. Effects
that cannot be represented as staged files—network calls, messages, database
writes, deployments—need their own admission and evidence mechanisms.

## 3. Scale the Run, not the shell command

Once one Run has stable identity, inputs, lifecycle, results, and evidence, it
can be orchestrated without changing its meaning. `pPilot` plans many tasks,
bounds concurrency, fences leases, records durable results, and reconciles
supported crash windows.

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

The local workflow and the fleet workflow share the same unit: a Run. Physical
placement may change; Run identity, authority, checkpoint lineage, and result
ownership must not.

## 4. Keep the history after execution ends

Processes disappear. The facts needed to understand a Run should not.
Gateway capture emits canonical events while `pChronicle` discovers trajectory
sources, normalizes their views, and makes them queryable.

```bash
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

History is not the scheduler and it is not the execution boundary. It is the
durable record used for inspection, replay, evaluation, exchange, and analysis.

## One model from local to fleet

| Concern | One local Run | Many Runs or a fleet |
| --- | --- | --- |
| Execution | One Agent virtual execution environment | A pool of environments placed across providers |
| Identity | Stable Run ID and one active Attempt | Stable Run IDs with lease-fenced ownership |
| Effects | Review, selective apply, drop | Policy-driven promotion and reconciliation |
| Continuity | Stage, checkpoint, fork | Recovery, migration, and lineage across placement |
| Evidence | Run Bundle and captured events | Durable history and cross-Run analysis |

The component boundary is direct: **pVisor runs and contains one Run, pPilot
orchestrates many Runs, and pChronicle preserves what happened.** Gateway,
OverlayFS, and OverlayNet are runtime mechanisms inside pVisor.

## Continue by task

- [Run the first Agent](pvisor/get-started.md)
- [Review and selectively apply changes](pvisor/guides/review-apply.md)
- [Orchestrate many Runs](pvisor/guides/orchestrate.md)
- [Explore durable history](pchronicle/get-started.md)
- [Read the system architecture](system-design/index.md)
