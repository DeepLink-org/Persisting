# pPilot architecture

pPilot is the durable Run orchestrator. Its product surface is deliberately
limited to `ppilot run` and `ppilot produce`.

```text
planner / plan()
      │ stable task identity + backpressure
      ▼
pPilot ── RunSpec ──► pVisor ── RunResult ──► durable result journal
      │                   │
      └── lease / CAS ────┴───────────────► pChronicle Run control
```

pPilot owns planning, bounded concurrency, leases, infrastructure retries,
resume/reconciliation, and result collection. pVisor owns each Run/Attempt and
its runtime drivers. pChronicle owns trajectory Dataset catalog, SQL, analysis,
find, import/export, and serving.

`run` executes a map-style `plan()` / `execute(item)` workload. `produce`
streams complete Run descriptions from a planner and creates one independent
pVisor workspace per item. Both start an in-process job Supervisor and publish
stable lineage (`parent_run_id`, `task_id`, and job metadata).

The durable path uses monotonically increasing lease epochs and terminal CAS
to reject stale workers. On restart, pPilot reconciles its journal, Attempt
records, and Run control records before deciding whether to defer, recover, or
redispatch work. External effects still require application-level idempotency;
the system does not promise exactly-once execution.

See the [`ppilot` command reference](cli-ppilot.md) for the public interface.
