# AgentVisor contract

> **Status:** category definition and pVisor product contract. Sections marked
> **Current** describe implemented behavior. Sections marked **Product gate**
> describe requirements that must be completed before the corresponding claim
> is made. Executable `--help`, public types, and contract tests remain the
> source of truth for a particular release.

**AgentVisor** is the product category. **pVisor** is Persisting's AgentVisor
implementation.

An AgentVisor is a control and containment layer between an autonomous Agent
and the infrastructure that executes it. It governs the Agent's logical
lifecycle, requested capabilities, external effects, checkpoints, lineage, and
the promotion of selected results into the real environment.

```text
Agent / coding Agent / evaluation worker
                     │
                     │ Agent-facing Run contract
                     ▼
                AgentVisor
        lifecycle · capabilities · effects
        checkpoints · lineage · evidence
                     │
                     │ provider contract
                     ▼
 host process · sandbox · OCI · microVM · cluster provider
```

An AgentVisor is not an Agent framework, a model router, a container runtime,
or an operating system. It may use each of those systems. Its defining object
is an **Agent Run with governed effects**, not a process, container, VM, prompt,
or workflow node.

![pVisor AgentVisor architecture](../assets/diagrams/pvisor/agentvisor-architecture.svg)

## The contract at a glance

Every pVisor execution begins with a logical `RunSpec` and produces a terminal
`RunResult`, durable local state, and a versioned Run Bundle. Physical retries
or placements are represented as Attempts beneath the Run.

```text
RunSpec
  ├── Run identity and parent lineage
  ├── Agent identity and invocation
  ├── capability intent
  ├── runtime limits and policy mode
  └── optional fenced supervisor bootstrap
          │
          ▼
Run
  ├── Attempt 1 @ lease epoch N
  ├── Attempt 2 @ lease epoch N+1
  ├── Agent ABI observations and open effects
  ├── workspace stage and checkpoints
  └── lifecycle and trajectory events
          │
          ▼
RunResult + Run Bundle + promoted or discarded effects
```

The contract has six invariants:

1. **A Run is not a process.** One logical Run may have multiple physical
   Attempts, and process identity never becomes the portable product identity.
2. **A staged effect is not an enforced boundary.** Copy-on-write files make
   changes reviewable; an isolation backend determines whether the Agent can
   bypass that view.
3. **Requested capability is not proof of enforcement.** Enforcement is
   reported independently for each dimension with the mechanism that supplied
   it.
4. **Execution success is not terminal success.** Teardown, durable Run state,
   Run Bundle creation, and terminal event publication are part of completion.
5. **Checkpoint means what it records.** pVisor's current checkpoint is a
   logical Agent/workspace checkpoint; it does not claim VM-memory or arbitrary
   process continuation.
6. **Placement does not change Agent semantics.** Local, container, VM, and
   future cluster providers consume the same logical Run, capability, effect,
   and evidence model.

## What an AgentVisor owns

| Plane | Required responsibility | pVisor implementation |
| --- | --- | --- |
| Identity | Stable Run identity, distinct Attempt identity, parent lineage, and fenced ownership generation | `RunId`, `AttemptId`, `parent_run_id`, `lease_epoch` |
| Lifecycle | Admission, start, observation, cancellation, quiescence, checkpoint, terminal publication, and recovery semantics | `PVisor`, `RunHandle`, Agent ABI, RunRecord |
| Capability | Requested access plus explicit admission and enforcement outcome | `CapabilitySet`, `PolicyMode`, per-dimension evidence |
| Effect | Observe, stage, classify, approve, promote, reject, and audit externally visible changes | OverlayFS review/apply/drop today; Agent ABI effect registry and Gateway records |
| Placement | Select and bind one provider without rewriting the Agent-facing contract | host, Docker/Podman, libkrun VM |
| Evidence | Persist enough facts to explain what ran, what boundary applied, what changed, and how the Run ended | Run Bundle and pChronicle events |

An AgentVisor does not need to implement its own kernel, image format,
container lifecycle, scheduler, or analytical database. Those are provider or
adjacent-system responsibilities unless the Agent contract requires additional
semantics.

## Run and Attempt lifecycle

**Current.** pVisor owns the semantic Run. An executor owns one physical
Attempt. pPilot may own a monotonically increasing lease epoch when durable
orchestration is enabled.

```text
created → starting → running ──────────────→ completed
                        │                         ▲
                        ├→ checkpointing → running
                        └→ cancelling ────→ cancelled

startup, execution, teardown, or publication ───→ failed
```

Terminal process exit is necessary but not sufficient. pVisor tears down Run
drivers, persists the RunRecord, writes the Run Bundle, and publishes the
terminal event before returning a completed result. An ambiguous or failed
terminal publication is an infrastructure failure, not a warning attached to
success.

The current local registry and Agent ABI are Run-scoped. The ABI authenticates
clients, records processes and open effects, reports desired state, and fences
quiescence acknowledgements by directive generation.

## Capability intent and enforcement evidence

**Current.** pVisor separates capability dimensions instead of promoting one
executor label into a blanket security claim:

- model access;
- tool access;
- filesystem read;
- filesystem write;
- network;
- secrets;
- subprocess;
- resources.

Each dimension has one of three levels:

| Level | Meaning |
| --- | --- |
| `unenforced` | No boundary is claimed for the dimension |
| `cooperative` | The normal Agent path is mediated, but another path may bypass it |
| `enforced` | The selected mechanism is intended to be non-bypassable for the scoped process tree |

Evidence names mechanisms such as a Linux network namespace, Landlock,
Seatbelt, or the VM smoltcp boundary. Gateway injection and an explicit proxy
are cooperative network evidence; they do not prove direct sockets are
confined. `PolicyMode::Enforce` fails admission when any requested dimension
lacks enforced evidence.

**Product gate.** Current mechanism values are structured runtime records, not
cryptographic attestation. Cluster-grade evidence must be bound to the exact
RunSpec digest, provider build, host or guest identity, and Attempt generation,
and must be signed or anchored in provider attestation when the threat model
requires it.

## Effects and promotion

An AgentVisor distinguishes execution permission from effect promotion:

```text
Agent action
    │
    ├── denied by capability policy
    │
    └── allowed inside the Run
             │
             ├── observed only
             ├── staged for review
             ├── promoted by policy or user decision
             └── discarded
```

![AgentVisor effect promotion flow](../assets/diagrams/pvisor/effect-promotion.svg)

### Filesystem effects

**Current.** pVisor gives the Agent a copy-on-write workspace. The base remains
unchanged during execution. `review` classifies additions, modifications,
deletions, type changes, opaque directories, links, and metadata. `apply`
accepts exact paths and git-style include/exclude globs.

A partial apply computes a dependency-closed batch:

- required parent directories are included;
- hard-link siblings stay together;
- opaque directories remain atomic;
- explicitly excluded dependencies cause an error rather than a partial write;
- unselected changes remain staged for another apply or drop;
- every successful batch is appended to `apply-ledger.json`.

Applying all remaining changes or dropping the stage is terminal for that
stage. A drop cannot undo an already promoted batch.

**Product gate.** The current apply path is not a crash-atomic transaction
across all selected files and does not yet reject target changes by comparing a
planned preimage digest with the live destination. Those guarantees are
required before unattended promotion should be described as conflict-safe.

### Network, model, tool, and external effects

**Current.** Gateway and OverlayNet record mediated traffic; the Agent ABI
tracks registered open effects and prevents a logical checkpoint while a
participating client reports unresolved effects. VM TCP/DNS enforcement and
supported host deny-all paths can block access at the runtime boundary.

**Product gate.** A generalized external Effect Broker must give irreversible
tool and service mutations durable identities, prepare/commit or compensation
semantics where available, and a promotion policy equivalent to filesystem
apply. Capturing a request is evidence that it occurred, not proof that its
external consequences can be rolled back.

## Checkpoint, fork, and replay

**Current.** A live logical checkpoint asks every connected Agent ABI client to
quiesce at one directive generation and requires the open-effect set to be
empty. pVisor then snapshots the workspace upper and resumes the clients. A
stopped Run can be checkpointed directly. `fork` restores the selected
checkpoint into a new Run and records the parent Run and checkpoint.

This supports workspace and Agent-coordinated experimentation. It does not
preserve arbitrary process memory, TCP connections, kernel state, or an
uncooperative tool's hidden state.

**Product gate.** Portable semantic replay additionally needs model/tool input
bindings, nondeterminism records, artifact digests, and explicit handling for
effects that cannot be replayed or compensated.

## Execution providers

The provider boundary answers *where and how* an Attempt runs. The AgentVisor
contract answers *which Agent Run it belongs to and how its effects are
governed*.

| Provider | Current status | Enforcement boundary |
| --- | --- | --- |
| Linux safe host | Implemented | Rootless namespaces, synthetic root, Landlock, dropped capabilities; private network namespace for deny-all |
| macOS safe host | Implemented | Seatbelt write confinement and optional deny-all sockets; reads remain ambient |
| Docker/Podman | Implemented | Container placement; complete capability-to-OCI compilation remains open |
| libkrun KVM/HVF | Implemented | Guest-kernel boundary and non-bypassable supported VM network path; host-side VMM threat model remains platform-specific |
| WASM | Type reserved | No production executor |
| Remote fleet | Contract direction | pPilot can coordinate durable Runs, but no long-lived general fleet provider is currently claimed |

Provider selection must be persisted for an Attempt. A later operation must not
silently reroute an existing Attempt to a weaker provider. Unsupported enforced
capabilities fail before Agent execution.

## Local-to-cluster continuity

The AgentVisor product promise is semantic portability, not live migration of
an arbitrary machine:

![pVisor local-to-cluster semantic continuity](../assets/diagrams/pvisor/local-to-cluster.svg)

| Contract | Personal machine | Cluster target |
| --- | --- | --- |
| Run identity | Local durable RunRecord | Durable control store and fenced lease |
| Agent ABI | Run-local authenticated endpoint | Same logical ABI through the node boundary |
| Workspace effects | Local stage and selective apply | Artifact-backed stage and policy-controlled promotion |
| Capability policy | Local profile and per-dimension evidence | Admission plus provider/node evidence |
| History | Local or object-store pChronicle | Shared canonical pChronicle root |
| Execution | Host, container, or local VM | Scheduled pVisor node/provider |

**Current.** Standalone pVisor needs no controller. pPilot supplies job-scoped
workers, least-loaded scheduling, durable results, lease epochs, CAS terminal
publication, and local or object-store control roots. Its worker labels are
informational and its normal workers are process-local or torchrun-created.

**Product gate.** The cluster claim requires a long-lived controller, durable
node registration, heartbeat and loss detection, placement constraints,
reconciliation after controller and node failure, artifact transfer, tenant or
trust-domain isolation, and a qualified real-backend test profile.

## pVisor architecture

```text
CLI / pPilot / embedding host
          │ RunSpec
          ▼
       pVisor
          ├── admission and per-dimension evidence
          ├── Run/Attempt state and Agent ABI
          ├── WorkspaceOverlay and checkpoint lineage
          ├── Gateway / OverlayNet / Control drivers
          ├── executor: host / container / libkrun VM
          └── Run Bundle and pChronicle event sink
```

pVisor owns one Run. pPilot owns planning and orchestration across Runs.
pChronicle owns canonical history and derived views. Gateway, OverlayNet,
Control, and OverlayFS are pVisor drivers rather than independent product
control planes.

## Deliberate boundaries

- **Not an OCI replacement.** OCI is one possible provider boundary. pVisor
  adds Agent lifecycle, effect, checkpoint, and evidence semantics above it.
- **Not a promise of universal rollback.** Filesystem promotion is controlled;
  arbitrary external service mutations may be irreversible.
- **Not one isolation label.** Read, write, network, secret, subprocess, and
  resource enforcement may have different strengths in the same Run.
- **Not live VM migration.** Local-to-cluster initially means compatible
  RunSpec, artifacts, checkpoints, lineage, and policy—not transparent memory
  transfer.
- **Not a mandatory daemon.** The local product remains direct and foreground.
  A cluster deployment may add durable controllers and node agents without
  changing the Agent-facing contract.

## Read next

- [Run workloads with pVisor](../guide/pvisor-execution.md)
- [pVisor command reference](cli-pvisor.md)
- [pVisor isolation architecture](pvisor-isolation.md)
- [Agent infrastructure](agent-infrastructure.md)
- [pPilot architecture](ppilot.md)
