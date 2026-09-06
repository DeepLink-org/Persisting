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
      │                                │
      │ lease / CAS                    │ EventRecord + Attempt state
      └──────────────┬─────────────────┘
                     ▼
             pChronicle control sidecar
                     │
                     ▼
              durable history store
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

The default pVisor build has no embedded Chronicle storage adapter and does not
link Lance or DataFusion. When durable publication is configured, pVisor starts
the Control component of `pchronicle serve` and publishes Attempt state plus
lifecycle/Gateway events through the shared `persisting-events` control contract. pPilot uses the same
contract for lease/CAS and result-journal coordination; each command owns the
sidecar process it starts. The executable is selected by the pVisor installation
or Run configuration; recording is selected with `--record-format` and
`--record-destination`.

The local control protocol is versioned, request-correlated, authenticated with
a per-process token, and bound to loopback. It is a process boundary rather
than a second storage implementation: pChronicle alone selects the physical
backend and sends the durable acknowledgement. See
[RFC-0007](../../rfcs/0007-events-contract-pchronicle-sidecar.md).

`run` executes a map-style `plan()` / `execute(item)` workload. `produce`
streams complete Run descriptions from a planner and creates one independent
pVisor workspace per item. Both start an in-process job Supervisor and publish
stable lineage (`parent_run_id`, `task_id`, and job metadata).

The durable path uses monotonically increasing lease epochs and terminal CAS
to reject stale workers. On restart, pPilot reconciles its journal, Attempt
records, and Run control records before deciding whether to defer, recover, or
redispatch work. External effects still require application-level idempotency;
the system does not promise exactly-once execution.

See the [`ppilot` command reference](../reference/cli.md) for the public
interface and [Run, Attempt, and Effect](../../pvisor/concepts/run-model.md) for the
identity and retry model used here.
