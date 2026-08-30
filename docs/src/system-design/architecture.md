# End-to-end architecture

This document defines the contracts between Persisting products. Provider
mechanisms belong to pVisor Design; storage layouts belong to pChronicle Design;
commands belong to each product's Reference.

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## Product ownership

| Product or layer | Owns | Does not own |
| --- | --- | --- |
| `persisting-events` contract | storage-independent `EventRecord` identity/envelope and the optional versioned pChronicle control protocol | storage rows, storage engines, query, or projection |
| pVisor | one Run, its Attempts, execution environment, capability admission, effects, and runtime evidence | many-Run scheduling or durable history queries |
| pPilot | planning, bounded execution, leases, retry and recovery, reconciliation, result collection, and task-to-Run mapping for many Runs | Agent reasoning, provider enforcement, or trajectory formats and storage |
| pChronicle | Agent trajectory storage engine: path identity, Snapshot, canonical events, projections, query, and exchange | starting, scheduling, or controlling a Run |
| Runtime provider | one physical execution mechanism | logical Run identity or product policy |

Gateway, OverlayFS, and OverlayNet are pVisor runtime mechanisms. They do not
form independent control planes. pPilot scales the pVisor Run model and remains
inside the pVisor product boundary.

## Independent ingress paths

```text
Configured runtime capture
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> canonical event Source ──────────────┐
Pinned external Sources                                                │
  local/S3 ATIF, ACTF, OpenAI Messages files ──────────────────────────┼─> Snapshot
  local/S3 Storyline Sources ──────────────────────────────────────────┘
                                                                         └─> normalized Dataset views
```

pPilot is the optional many-Run orchestrator on the governed-execution path.
pVisor can complete its standalone loop with a terminal RunResult, staged
Effects, and a private, versioned Run Bundle. Configured capture is not a pVisor
runtime prerequisite. External file and Storyline Sources are pinned and
normalized directly; they neither pass through pVisor nor become canonical
runtime events, and they do not acquire pVisor execution guarantees.

## Stable objects

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      └── Attempt finalization
          ├── terminal RunResult
          ├── private versioned Run Bundle
          └── staged Effects → later review / apply / drop

Optional configured event handoff
  └── Gateway trajectory events + pVisor lifecycle records
```

The logical Run is portable. An Attempt is provider-specific. Infrastructure
retry creates another Attempt; a semantic retry creates a derived Run. A Run
may have multiple Attempts but only one visible terminal result.

Where a Source carries it, the stable cross-product identity is `run_id`.
Session, Step, call, event, and Artifact identities remain scoped and retain
their Source lineage. A process ID, container ID, VM ID, or worker lease is
never a substitute for Run identity.

## Single-Run path

```text
User or Agent framework
  → RunSpec
  → pVisor admission
  → capability-by-dimension provider selection
  → Attempt execution
  → terminal RunResult + private versioned Run Bundle + staged Effects
  → later review / apply / drop
```

Admission compares requested capability dimensions with evidence the selected
provider can produce. A required dimension that cannot be enforced fails before
workload execution. Optional degradation is recorded explicitly in the Run
Bundle.

Filesystem promotion is an Effect decision, not the Run terminal commit.
Selected paths can be applied more than once while the stage remains available.
Network requests and remote tool mutations are separate effect dimensions and
cannot be inferred from filesystem state.

When configured, pVisor publishes Gateway trajectory events plus `run.created`,
`run.state_changed`, and terminal lifecycle records to pChronicle. Those records
carry Run/Attempt identity, lifecycle facts, and available event-carried
Evidence. Artifact references, lineage, staged filesystem Effects,
AgentCtl/network/resource Evidence, and the full Run Bundle remain local unless
a separate adapter moves them.

## Many-Run path

```text
Manifest or task stream
  → pPilot planner
  → stable task_id and run_id
  → bounded RunFuture set
  → pVisor placement and Attempts
  → pPilot checkpoint and reconciliation
  → terminal results and task-to-Run mapping
```

pPilot schedules Run futures rather than Agent conversations. It persists the
relationship:

```text
job_id → task_id → run_id → attempt_id / lease_epoch → terminal result
```

The system does not promise exactly-once physical execution. Lease fencing,
stable identity, idempotent event ingestion, and terminal compare-and-swap aim
for at-least-once Attempts with one visible Run result.

With `run --sink`, the default path writes the configured result journal and
uses a pChronicle control child for selected coordination records. When built
with `traj-sink` and invoked with `--traj`, pPilot additionally emits only
terminal `ppilot.result` or `ppilot.failure` records; it does not capture a
general Run trajectory. Delegated pVisor Runs do not receive Chronicle capture
configuration. pPilot owns orchestration decisions and task-to-Run mapping in
all of these modes. Command flags and resume behavior belong to the
[pPilot CLI reference](../ppilot/reference/cli.md) and
[orchestration design](../ppilot/design/orchestration.md).

## Dataset path

Canonical runtime writers and pinned external Sources are independent Source
paths. They converge only at the Snapshot and normalized Dataset views:

```text
configured Gateway and pVisor lifecycle writers
  → canonical event Source ────────────────────────────────┐
pinned local/S3 external Sources                           │
  → ATIF / ACTF / OpenAI Messages files ───────────────────┼─> Snapshot
  → Storyline Sources ─────────────────────────────────────┘     ├─> normalized Run / Step / ToolCall views
                                                                 └─> query / export / revision lineage
```

Canonical facts are append-oriented. Storyline and other normalized views are
rebuildable projections. Exchange files are interoperability boundaries, not a
replacement source of truth. Each read operation fixes a Snapshot; it
does not invent a global transaction across unrelated Sources. Pinning an
external file does not convert it into a canonical runtime event Source.

## Source-specific guarantees

| Source path | Supported claim | Explicit non-claim |
| --- | --- | --- |
| External file or imported Source | discovered content, pinned Source version, normalized representation, and recorded conversion lineage where implemented | completeness of an external task manifest or absence of unreported trajectories |
| Gateway capture | requests and responses observed and durably published through the configured Gateway path | absence of traffic that bypassed Gateway |
| pVisor Run | Run/Attempt identity, recorded terminal facts, installed mechanisms, observed Effects, and provider-specific Evidence | enforcement a selected provider did not supply |
| pPilot job | persisted task/Run mapping, retry and lease history, and terminal result behavior supported by its selected mode | physical exactly-once execution |

Ingestion preserves these boundaries. A normalized representation or Catalog
Snapshot does not upgrade the evidence supplied by its Source.

The default pVisor build does not link Lance or DataFusion. Configured
Chronicle publication starts a pChronicle sidecar over authenticated loopback
IPC and treats only a successful sidecar acknowledgement as durable. The
legacy mode name `lance` is an alias for `spawn`; pVisor no longer writes Lance
itself. Sidecar flags and mode names belong to the
[pVisor CLI reference](../pvisor/reference/cli.md) and
[RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md).

## Failure and recovery

| Failure | Owner | Required behavior |
| --- | --- | --- |
| Attempt exits or provider disappears | pVisor | finalize evidence; expose failure or create a fenced replacement Attempt |
| Worker lease expires | pPilot | prevent stale terminal publication; reconcile expected and observed state |
| sidecar append queue is saturated or closed | pVisor/Gateway producer | reject before submission and report the failure; do not claim durability |
| append connection or acknowledgement is lost | producer and pChronicle writer | preserve the write as unknown because it may have committed; do not reuse its sequence as if definitely rejected |
| history publication conflicts | pChronicle writer | preserve the previously published Snapshot; surface or retry according to the writer contract |
| control plane restarts | pPilot | reconcile checkpoint, active Attempts, and terminal history facts |
| view generation fails | pChronicle | keep canonical facts readable; rebuild the derived view |

Recovery never upgrades uncertainty into success. A missing terminal fact, a
lost callback, and an unenforced capability remain visible states.

## Security and evidence chain

Security is reported per capability dimension. pVisor records requested policy,
installed mechanism, provider identity, enforcement result, and observed
effects. pPilot preserves authority generation and lease history across
placement. Configured pChronicle capture stores lifecycle facts and only the
Evidence carried by Gateway or lifecycle event records; the broader Run Bundle
evidence inventory remains local unless moved separately.

This produces a chain rather than a boolean label. The local Run evidence
chain does not mean every layer is automatically published into durable
history:

```text
requested policy
  → admission decision
  → installed mechanism
  → provider-bound evidence
  → observed effects
  → terminal result

Optional configured persistence
  Gateway trajectory events + pVisor lifecycle records
    → event-carried Evidence only
    → pChronicle durable history
```

See [Security and evidence](security-evidence.md) for evidence levels and
[Local to fleet](local-to-fleet.md) for portability requirements.

## Public boundaries

| Boundary | Contract owner | Detailed document |
| --- | --- | --- |
| logical runtime event and local Chronicle control protocol | `persisting-events` | [RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md) |
| Agent execution and Effect review | pVisor | [pVisor concepts](../pvisor/concepts/index.md) and [guides](../pvisor/guides/index.md) |
| provider and runtime mechanisms | pVisor | [pVisor design](../pvisor/design/index.md) |
| many-Run orchestration | pPilot within pVisor | [pPilot design](../ppilot/design/orchestration.md) |
| Dataset, facts, and projections | pChronicle | [pChronicle concepts](../pchronicle/concepts/index.md) |
| storage and Snapshot implementation | pChronicle | [pChronicle design](../pchronicle/design/index.md) |
| stable command syntax and formats | each product | [pVisor reference](../pvisor/reference/index.md) and [pChronicle reference](../pchronicle/reference/index.md) |
| normative ownership decisions | Project RFCs | [RFC index](../rfcs/index.md) |

This document changes only when a cross-product contract changes. Product
implementation status and roadmap details belong to their owning Design pages
or Project engineering notes.
