# End-to-end architecture

This document defines the contracts between Persisting products. Provider
mechanisms belong to pVisor Design; storage layouts belong to pChronicle Design;
commands belong to each product's Reference.

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## Product ownership

| Product or layer | Owns | Does not own |
| --- | --- | --- |
| pVisor | one Run, its Attempts, execution environment, capability admission, effects, and runtime evidence | many-Run scheduling or durable history queries |
| pPilot | planning, bounded execution, leases, retry and recovery, reconciliation, result collection, and task-to-Run mapping for many Runs | Agent reasoning, provider enforcement, or trajectory formats and storage |
| pChronicle | Dataset and Source discovery, canonical events and terminal facts, normalized projections, revision lineage, query, and exchange | starting, scheduling, or controlling a Run |
| Runtime provider | one physical execution mechanism | logical Run identity or product policy |

Gateway, OverlayFS, and OverlayNet are pVisor runtime mechanisms. They do not
form independent control planes. pPilot scales the pVisor Run model and remains
inside the pVisor product boundary.

## Independent ingress paths

```text
Agent goal -> pVisor / pPilot -> events + artifacts + terminal facts + Evidence
                                                   |
External Sources -> importer / adapter ------------+-> pChronicle Dataset
```

pPilot is the optional many-Run orchestrator on the governed-execution path.
pVisor can complete its standalone loop with staged Effects and a private,
versioned Run Bundle. The handoff to pChronicle is the standard durable Dataset
and history path, not a pVisor runtime prerequisite. External Sources enter
pChronicle through supported importers or adapters without acquiring pVisor
execution guarantees.

## Stable objects

```text
RunSpec
  └── Run
      ├── Attempt 1
      ├── Attempt 2
      ├── Artifact references
      ├── Effect decisions
      └── terminal RunResult + private versioned Run Bundle

Optional durable handoff
  └── pChronicle canonical events and Dataset projections
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
  → staged Effect review / apply / drop
  → terminal RunResult + private versioned Run Bundle
```

Admission compares requested capability dimensions with evidence the selected
provider can produce. A required dimension that cannot be enforced fails before
workload execution. Optional degradation is recorded explicitly in the Run
Bundle.

Filesystem promotion is an Effect decision, not the Run terminal commit.
Selected paths can be applied more than once while the stage remains available.
Network requests and remote tool mutations are separate effect dimensions and
cannot be inferred from filesystem state.

When configured, pVisor hands events, artifacts, terminal facts, and Evidence
to pChronicle for durable Dataset history. Failure to configure that handoff
does not make the local Run Bundle incomplete for the pVisor contract.

## Many-Run path

```text
Manifest or task stream
  → pPilot planner
  → stable task_id and run_id
  → bounded RunFuture set
  → pVisor placement and Attempts
  → pPilot checkpoint and reconciliation
  → terminal results and task-to-Run mapping
  → events, artifacts, terminal facts, and Evidence
```

pPilot schedules Run futures rather than Agent conversations. It persists the
relationship:

```text
job_id → task_id → run_id → attempt_id / lease_epoch → terminal result
```

The system does not promise exactly-once physical execution. Lease fencing,
stable identity, idempotent event ingestion, and terminal compare-and-swap aim
for at-least-once Attempts with one visible Run result.

The resulting facts can use the same optional durable pChronicle handoff as one
Run. pPilot owns the orchestration decisions and mapping even when a pChronicle
control process stores the selected coordination records.

## Dataset path

pVisor, Gateway, providers, and external importers emit facts from independent
Source paths. pChronicle owns their durable interpretation after ingestion:

```text
producers
  → canonical events
  → terminal fact and Artifact manifest
  → normalized Run / Step / ToolCall projections
  → exchange formats and lineage-bearing revisions
```

Canonical facts are append-oriented. Storyline and other normalized views are
rebuildable projections. Exchange files are interoperability boundaries, not a
replacement source of truth. Each read operation fixes a Catalog Snapshot; it
does not invent a global transaction across unrelated Sources.

## Source-specific guarantees

| Source path | Supported claim | Explicit non-claim |
| --- | --- | --- |
| External file or imported Source | discovered content, pinned Source version, normalized representation, and recorded conversion lineage where implemented | completeness of an external task manifest or absence of unreported trajectories |
| Gateway capture | requests and responses observed and durably published through the configured Gateway path | absence of traffic that bypassed Gateway |
| pVisor Run | Run/Attempt identity, recorded terminal facts, installed mechanisms, observed Effects, and provider-specific Evidence | enforcement a selected provider did not supply |
| pPilot job | persisted task/Run mapping, retry and lease history, and terminal result behavior supported by its selected mode | physical exactly-once execution |

Ingestion preserves these boundaries. A normalized representation or Catalog
Snapshot does not upgrade the evidence supplied by its Source.

## Failure and recovery

| Failure | Owner | Required behavior |
| --- | --- | --- |
| Attempt exits or provider disappears | pVisor | finalize evidence; expose failure or create a fenced replacement Attempt |
| Worker lease expires | pPilot | prevent stale terminal publication; reconcile expected and observed state |
| capture queue is saturated | producer/Gateway | never block the request callback; report loss or preserve through the configured durable path |
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
