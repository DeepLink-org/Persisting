# pPilot

**Durable Run production at scale.**

pPilot extends the Run model from one execution to a bounded collection of
tasks. It owns planning, bounded concurrency, leases and fencing decisions,
infrastructure retry and recovery, reconciliation, durable result publication,
and task-to-Run mapping.

It does not redefine the Agent runtime: each task remains an independent
[pVisor Run](../pvisor/concepts/run-model.md), executed by the standalone
`pvisor` binary.

| Command | Owns |
| --- | --- |
| `ppilot run` | execute a `plan()` / `execute(item)` workload with durable recovery |
| `ppilot produce` | create independent pVisor Runs from a streaming planner |

## Where to start

- [Get Started](get-started.md) — run your first parallel plan in five minutes
- [Orchestrate many Agent Runs](guides/orchestrate.md) — planning, workers, resume, and sinks
- [Orchestration design](design/orchestration.md) — leases, fencing, and recovery guarantees
- [pPilot CLI reference](reference/cli.md) — exact flags and exit behavior
