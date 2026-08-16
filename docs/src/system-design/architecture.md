# End-to-end architecture

This document defines the contracts between Persisting products. Provider
mechanisms belong to pVisor Design; storage layouts belong to pChronicle Design;
commands belong to each product's Reference.

![Persisting from one Run to orchestration and history](../assets/diagrams/persisting/execution-story.svg)

## Product ownership

| Product or layer | Owns | Does not own |
| --- | --- | --- |
| `persisting-events` contract | storage-independent `EventRecord` identity/envelope and the optional versioned pChronicle control protocol | storage rows, storage engines, query, or projection |
| pVisor | one Run, its Attempts, execution environment, capability admission, effects, and runtime evidence | many-Run scheduling or durable history queries |
| pPilot | planning and reconciling many Runs, leases, bounded concurrency, infrastructure retry, and result collection | Agent reasoning, provider enforcement, or trajectory storage |
| pChronicle | durable canonical event log, physical schemas/backends, terminal facts, Dataset discovery, normalized projections, revisions, and read surfaces | starting, scheduling, or controlling a Run; defining a second event envelope |
| Runtime provider | one physical execution mechanism | logical Run identity or product policy |

Gateway, OverlayFS, and OverlayNet are pVisor runtime mechanisms. They do not
form independent control planes. pPilot scales the pVisor Run model and remains
inside the pVisor product boundary.

## Stable objects

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      ├── Artifact references
      ├── Effect decisions
      └── terminal RunResult
              └── canonical events and history projections
```

The logical Run is portable. An Attempt is provider-specific. Infrastructure
retry creates another Attempt; a semantic retry creates a derived Run. A Run
may have multiple Attempts but only one visible terminal result.

The stable cross-product identity is `run_id`. Session, Step, call, event, and
Artifact identities remain scoped and retain their Source lineage. A process
ID, container ID, VM ID, or worker lease is never a substitute for Run identity.

## Single-Run path

```text
User or Agent framework
  → RunSpec
  → pVisor admission
  → capability-by-dimension provider selection
  → Attempt execution
  → runtime events and Artifact references
  → persisting-events contract
  → pChronicle sidecar durable acknowledgement (when enabled)
  → Effect review or direct policy decision
  → terminal RunResult
  → pChronicle canonical history and projections
```

Admission compares requested capability dimensions with evidence the selected
provider can produce. A required dimension that cannot be enforced fails before
workload execution. Optional degradation is recorded explicitly in the Run
Bundle.

Filesystem promotion is an Effect decision, not the Run terminal commit.
Selected paths can be applied more than once while the stage remains available.
Network requests and remote tool mutations are separate effect dimensions and
cannot be inferred from filesystem state.

## Many-Run path

```text
Manifest or task stream
  → pPilot planner
  → stable task_id and run_id
  → bounded RunFuture set
  → pVisor placement and Attempts
  → pPilot checkpoint and reconciliation
  → terminal results
  → pChronicle history
```

pPilot schedules Run futures rather than Agent conversations. It persists the
relationship:

```text
job_id → task_id → run_id → attempt_id / lease_epoch → terminal result
```

The system does not promise exactly-once physical execution. Lease fencing,
stable identity, idempotent event ingestion, and terminal compare-and-swap aim
for at-least-once Attempts with one visible Run result.

## History path

pVisor, Gateway, providers, and importers emit facts using the shared
`persisting-events::EventRecord` contract. pChronicle owns durable ingestion,
the physical representation, and interpretation:

```text
producers
  → EventRecord contract
  → versioned pChronicle control protocol or in-process pChronicle API
  → durable canonical event log
  → terminal fact and Artifact manifest
  → normalized Run / Step / ToolCall projections
  → exchange formats and lineage-bearing revisions
```

Canonical facts are append-oriented. Storyline and other normalized views are
rebuildable projections. Exchange files are interoperability boundaries, not a
replacement source of truth. Each read operation fixes a Catalog Snapshot; it
does not invent a global transaction across unrelated Sources.

The default pVisor build does not link Lance or DataFusion. With Chronicle mode
`spawn`, pVisor starts `pchronicle control`, submits lifecycle and Gateway
events over authenticated loopback IPC, and treats only a successful sidecar
response as a durable acknowledgement. The legacy mode name `lance` is an
alias for `spawn`; pVisor no longer writes Lance itself.

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
placement. pChronicle stores the evidence references and immutable outcome facts.

This produces a chain rather than a boolean label:

```text
requested policy
  → admission decision
  → installed mechanism
  → provider-bound evidence
  → observed effects
  → terminal result
  → durable history
```

See [Security and evidence](security-evidence.md) for evidence levels and
[Local to fleet](local-to-fleet.md) for portability requirements.

## Public boundaries

| Boundary | Contract owner | Detailed document |
| --- | --- | --- |
| logical runtime event and local Chronicle control protocol | `persisting-events` | [RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md) |
| Agent execution and Effect review | pVisor | [pVisor concepts](../pvisor/concepts/index.md) and [guides](../pvisor/guides/index.md) |
| provider and runtime mechanisms | pVisor | [pVisor design](../pvisor/design/index.md) |
| many-Run orchestration | pPilot within pVisor | [pPilot design](../pvisor/design/orchestration.md) |
| Dataset, facts, and projections | pChronicle | [pChronicle concepts](../pchronicle/concepts/index.md) |
| storage and Catalog implementation | pChronicle | [pChronicle design](../pchronicle/design/index.md) |
| stable command syntax and formats | each product | [pVisor reference](../pvisor/reference/index.md) and [pChronicle reference](../pchronicle/reference/index.md) |
| normative ownership decisions | Project RFCs | [RFC index](../rfcs/index.md) |

This document changes only when a cross-product contract changes. Product
implementation status and roadmap details belong to their owning Design pages
or Project engineering notes.
