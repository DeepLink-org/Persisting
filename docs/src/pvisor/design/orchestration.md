# pPilot architecture

pPilot is the durable Run orchestrator. Its product surface is deliberately
limited to `ppilot run` and `ppilot produce`.

This page owns the many-Run control algorithm. The user workflow belongs to
[Orchestrate many Agent Runs](../guides/orchestrate.md), and the cross-product
commit boundary belongs to [System Design](../../system-design/architecture.md).

```text
planner / plan()
      │ stable task identity + backpressure
      ▼
pPilot ── RunSpec/file + process ──► pVisor ── RunResult ──► durable result journal
      │                   │
      └── lease / CAS ────┴───────────────► pChronicle Run control
```

pPilot owns planning, bounded concurrency, leases, infrastructure retries,
resume/reconciliation, and result collection. pVisor owns each Run/Attempt and
its runtime drivers. pChronicle owns trajectory Dataset catalog, SQL, analysis,
find, import/export, and serving.

pPilot does not link the pVisor implementation crate. It launches one
foreground `pvisor` binary per Run, submits a versioned `RunSpec`, and reads an
atomic `RunResult`. The job Supervisor's registration, heartbeat, quota, and
cancel messages are shared agentctl contracts. Process exit remains the
lifecycle boundary; a Supervisor connection supplies live control without a
resident pVisor daemon.

The product build enables pVisor's `local-lance-chronicle` adapter so the
standalone child can publish attempt state into pPilot's durable registry.
This is a runtime-binary capability: it does not add pVisor or Gateway to
pPilot's Cargo dependency graph, and it excludes cloud object-store SDKs.

`run` executes a map-style `plan()` / `execute(item)` workload. `produce`
streams complete Run descriptions from a planner and creates one independent
pVisor workspace per item. Both start an in-process job Supervisor and publish
stable lineage (`parent_run_id`, `task_id`, and job metadata).

The durable path uses monotonically increasing lease epochs and terminal CAS
to reject stale workers. On restart, pPilot reconciles its journal, Attempt
records, and Run control records before deciding whether to defer, recover, or
redispatch work. External effects still require application-level idempotency;
the system does not promise exactly-once execution.

See the [`ppilot` command reference](../reference/ppilot-cli.md) for the public
interface and [Run, Attempt, and Effect](../concepts/run-model.md) for the
identity and retry model used here.
