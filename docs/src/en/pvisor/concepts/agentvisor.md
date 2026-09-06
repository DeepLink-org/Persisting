# What is an AgentVisor?

**An AgentVisor is the hypervisor for Agent execution.**

It organizes compute, filesystems, networks, models, tools, credentials, and
durable state on a personal computer, workstation, or cluster into a shared
resource pool. It then gives every Agent Run an isolated **Agent virtual
execution environment**. Multiple Agents can reuse the same underlying
environment resources without sharing identity, workspace, authority, state,
external effects, or failure domains.

![The AgentVisor category in the Agent infrastructure stack](../../assets/diagrams/agentvisor/agentvisor-stack.svg)

## Definition

> An **AgentVisor** is the virtualization layer for Agents. It maps shared
> environment resources into isolated, governable, suspendable, and portable
> Agent virtual execution environments, and manages resource multiplexing,
> isolation, lifecycle, authority, state, and external effects across Agents.

Traditional hypervisors present virtual machines to operating systems. An
AgentVisor presents virtual execution environments to Agents. This environment
is not a new machine-image format. It is the complete execution boundary seen
by an Agent, including:

- schedulable compute, memory, and accelerators;
- isolated workspace, process, network, and tool spaces;
- delegated and bounded access to models, data, secrets, and external services;
- execution state that can pause, recover, fork, and migrate;
- external effects that can be contained, reviewed, committed, or compensated;
- identity, lineage, and evidence that remain attached to the Run.

An AgentVisor can map that virtual environment onto a local process, operating-
system sandbox, container, microVM, confidential environment, or remote fleet.
The kernel, node, and scheduler can change while the Agent continues to see a
stable Run identity, capabilities, checkpoints, effect semantics, and
accountability boundary.

## Why the category is needed

Traditional software normally crosses a human-controlled boundary before it
acts: a user clicks, an operator deploys, or an API caller supplies a narrow
request. An autonomous Agent can instead form a chain of decisions over
minutes, hours, or days. During that time it may:

- read private context and acquire temporary credentials;
- write code, documents, infrastructure, or business records;
- call models and tools chosen at runtime;
- spawn subprocesses or delegate to other Agents;
- communicate with people or external services;
- pause, recover, branch, and continue from accumulated state;
- leave effects that survive every process involved in the Run.

What is missing is a unified virtualization layer for Agents. A sandbox can isolate a process but does
not decide which results should become real. A workflow engine can sequence
steps but does not prove that direct network or filesystem paths were confined.
A model gateway can mediate inference but does not own subprocesses, tools, or
workspace state. An observability platform can record events after the fact
but does not govern them before they occur.

The AgentVisor composes these separate capabilities into an Agent virtual
execution environment that can be created, scheduled, suspended, migrated,
and reclaimed, while allowing multiple Agents to share the underlying pool.

## Where an AgentVisor sits

An AgentVisor is a narrow, compositional infrastructure layer. It works with
the systems around it rather than replacing them.

| Neighboring category | What it primarily owns | What the AgentVisor adds |
| --- | --- | --- |
| Agent framework | Reasoning loop, prompts, tool adapters, application logic | Provider-independent Run identity, authority, effects, continuity, and evidence |
| Model gateway | Model routing, authentication, quotas, inference telemetry | One policy context spanning models, tools, files, processes, and network access |
| Workflow engine | Dependency graph, retries, scheduled steps | Autonomous Run semantics, effect boundaries, checkpoints, and causal lineage |
| Sandbox / container / VM runtime | Process and kernel isolation | Agent-aware capability admission, result promotion, and cross-substrate identity |
| Policy engine | Decision evaluation | Binding policy decisions to a concrete Run, enforcement mechanism, and observed outcome |
| Observability platform | Logs, traces, metrics, analytics | Durable action/effect identity and evidence about what was enforced, not only what was observed |
| Secrets manager | Credential storage and issuance | Run-scoped delegation, delivery, expiry, and evidence of use |

An Agent operating system may describe an entire developer or enterprise
platform. AgentVisor is the more precise category inside that larger vision:
the layer that supervises autonomous execution and its consequences.

## The six responsibilities

Every credible AgentVisor must address six connected responsibilities.

### 1. Identity and lifecycle

The durable unit is an **Agent Run**, not a process or container. One Run may
have multiple physical Attempts because of retry, recovery, migration, or
placement changes. The Run retains one identity and a causal relationship to
its parents, children, and checkpoints.

Lifecycle includes admission, start, observation, quiescence, cancellation,
recovery, terminal publication, and retention. A successful process exit is
not sufficient if effects, evidence, or durable state remain ambiguous.

### 2. Delegated authority

An Agent should receive explicit, bounded authority rather than ambient user
power. Authority can cover models, tools, filesystem regions, network
destinations, secrets, subprocesses, financial limits, communication channels,
or compute budgets.

The AgentVisor binds that authority to the Run and its active Attempt. It
decides whether a request is admissible, selects mechanisms capable of
enforcing it, and prevents silent fallback to a weaker boundary.

### 3. Effect governance

Execution permission and effect promotion are separate decisions. An Agent may
be allowed to explore, generate, and mutate inside a contained Run without
receiving automatic authority to change the real environment.

The AgentVisor observes and classifies effects, stages them when possible, and
applies policy to promotion, rejection, or compensation. Filesystem changes
are one example. Messages, payments, deployments, tickets, database writes,
and tool mutations belong to the same conceptual plane even when their
reversibility differs.

### 4. Continuity and branching

Agents accumulate more than memory pages. They accumulate conversation state,
workspace changes, tool state, unresolved effects, credentials, artifacts, and
causal history. An AgentVisor defines a **semantic checkpoint** that records
which of those elements are present, absent, open, or externally committed.

That checkpoint can support pause/resume, recovery, fork, replay, evaluation,
or migration without pretending that every external system can be rewound.

### 5. Evidence and accountability

Requested policy is not evidence of enforcement. An AgentVisor records the
actual mechanism and outcome for each relevant dimension. It preserves enough
information to answer:

- Which Agent, Run, Attempt, and authority generation acted?
- What code, model, tool, artifact, and environment were involved?
- Which access was requested, allowed, denied, or bypassable?
- Which effects were observed, staged, promoted, rejected, or compensated?
- Where did execution occur, and what boundary was actually installed?
- Why was the Run considered complete, failed, or cancelled?

### 6. Placement portability

Execution providers should be replaceable without rewriting Agent semantics.
Local processes, operating-system sandboxes, containers, microVMs, confidential
environments, and remote fleets can offer different isolation and performance
profiles while consuming the same logical Run and authority model.

Portability does not mean every provider is equivalent. It means differences
are explicit, admission is capability-aware, and evidence follows the Run.

## The AgentVisor object model

A shared vocabulary is necessary before implementations can interoperate.

| Object | Industry-level meaning |
| --- | --- |
| **Agent Run** | One durable, user-meaningful execution with stable identity and intent |
| **Attempt** | One physical realization of a Run on a particular provider and ownership generation |
| **Capability Grant** | Delegated authority scoped by resource, action, conditions, limits, and lifetime |
| **Effect** | An externally meaningful observation or mutation with stable identity and lifecycle |
| **Checkpoint** | A declared consistency frontier across Agent state, workspace, effects, and artifacts |
| **Lineage** | Causal relationships among Runs, checkpoints, delegations, artifacts, and derived outcomes |
| **Evidence Bundle** | Durable facts describing execution, enforcement, effects, and terminal outcome |
| **Execution Provider** | A substrate that realizes an Attempt while reporting its capabilities and evidence |

This model is intentionally independent of a specific wire protocol, database,
container format, cloud, or Agent framework.

## Governing the effect loop

![The AgentVisor effect governance loop](../../assets/diagrams/agentvisor/effect-governance.svg)

The effect loop begins before execution and ends after the consequence is
known. A useful effect lifecycle includes:

1. **Intent** — the Agent or its tool describes a requested action.
2. **Admission** — policy evaluates the Run, capability, resource, context,
   and budget.
3. **Execution** — an enforcement point allows, denies, or transforms the
   action.
4. **Observation** — the actual result is captured with stable identity.
5. **Containment** — the result remains isolated or pending when the medium
   permits it.
6. **Promotion** — policy or a person accepts the effect into the real system.
7. **Compensation** — a committed effect is counteracted when true rollback is
   unavailable.
8. **Evidence** — the complete decision and outcome become part of the Run's
   accountable history.

Effects differ by reversibility:

| Effect class | Examples | Appropriate control |
| --- | --- | --- |
| Reversible | Workspace file, generated artifact, isolated branch | Stage, review, promote, discard |
| Transactional | Database transaction, deployment plan, API with prepare/commit | Reserve, validate, commit atomically |
| Compensatable | Ticket creation, cloud resource, reversible business operation | Commit with durable compensation plan |
| Irreversible | External message, published secret, physical action, settled payment | Strong admission, explicit authority, minimal scope, complete evidence |

Calling every action “sandboxed” obscures these differences. AgentVisor makes
them part of the product model.

## Authority is multidimensional

Security cannot be reduced to one label such as *sandboxed*, *containerized*,
or *running in a VM*. A single Run may have different guarantees for
filesystem reads, filesystem writes, network egress, secret access,
subprocesses, devices, tools, models, and resource budgets.

The industry needs to distinguish four evidence levels:

| Level | Meaning |
| --- | --- |
| **Declared** | Policy intent exists, but no mediation or enforcement is demonstrated |
| **Mediated** | The normal integration path passes through a control point, but bypass may exist |
| **Enforced** | The scoped actor cannot bypass the mechanism within the stated threat model |
| **Attested** | Enforcement evidence is cryptographically bound to the exact Run, provider, software identity, and authority generation |

An AgentVisor should report evidence independently per dimension and refuse a
Run when the requested guarantee cannot be met. Silent downgrade destroys the
meaning of delegated authority.

## From personal device to fleet

![The AgentVisor execution continuum](../../assets/diagrams/agentvisor/execution-continuum.svg)

The same category matters on both a personal computer and a multi-tenant
cluster.

On a personal device, an AgentVisor can remove constant approval prompts by
giving the Agent broad freedom inside a contained workspace while retaining
control over promotion into the user's real environment.

In a team or fleet, the same Run identity and effect semantics can be combined
with scheduling, leases, node attestation, tenant isolation, organization
policy, shared artifacts, and durable reconciliation.

The portable unit is not necessarily a live VM. It is the combination of:

- Run identity and intent;
- delegated authority;
- semantic checkpoint and lineage;
- effect frontier;
- content-addressed artifacts;
- enforcement and outcome evidence.

This makes local-to-fleet migration a semantic problem first and a machine
transport problem second.

## Design principles of the category

1. **Autonomy inside, control at the boundary.** High-frequency Agent decisions
   should not require high-frequency human approvals.
2. **Authority follows the Run.** Permissions must not depend on an accidental
   process, shell, node, or container identity.
3. **Effects are first-class.** External consequences need identity, state,
   policy, and evidence—not only logs.
4. **Evidence beats labels.** A mechanism and threat model are more meaningful
   than a generic “secure” or “sandboxed” badge.
5. **No silent weakening.** Placement or recovery must never reinterpret an
   existing Run under a weaker boundary.
6. **Checkpoint is semantic.** It declares consistency across Agent state and
   effects, not merely a memory snapshot.
7. **Providers remain replaceable.** The category sits above kernels,
   containers, VMs, clouds, and schedulers.
8. **Terminal means accountable.** A Run is not complete while its effects or
   terminal evidence remain ambiguous.

## A maturity model

AgentVisor products can be evaluated by capability rather than marketing
language.

| Level | Name | Minimum characteristics |
| ---: | --- | --- |
| 0 | Observed Agent | Stable Run identity and correlated logs, but no governed authority or effects |
| 1 | Supervised Agent | Isolated virtual execution environment, lifecycle control, cancellation, bounded resources, and explicit execution placement |
| 2 | Governed Agent | Multidimensional capability enforcement and first-class effect lifecycle |
| 3 | Portable Agent | Semantic checkpoints, lineage, provider-independent Attempts, and local-to-fleet continuity |
| 4 | Accountable Agent | Attested enforcement, durable effect reconciliation, multi-tenant isolation, and verifiable terminal evidence |

Level 0 is useful infrastructure, but it is not sufficient to claim the full
AgentVisor category. The category becomes distinctive at Level 2, where the
system controls both delegated authority and external effects.

## What qualifies as an AgentVisor?

A product belongs to this category when it can answer all of the following:

- Does the Agent have a stable Run identity independent of process placement?
- Can multiple Agents share underlying environment resources while preserving
  isolation of identity, state, authority, and failure domains?
- Is delegated authority explicit, bounded, and tied to that Run?
- Are enforcement claims separated by capability dimension and backed by
  concrete evidence?
- Are externally meaningful effects represented before and after commitment?
- Can the Run pause, recover, or fork at a declared semantic frontier?
- Do lineage and evidence survive movement across execution providers?
- Can terminal state be reconciled after failures without silently duplicating
  irreversible work?

A sandbox alone is not an AgentVisor. Neither is a model proxy, workflow
engine, tracing product, permission prompt, or container scheduler. Any of them
can become an essential provider within an AgentVisor architecture.

## Where industry standardization can emerge

The category does not require one implementation, but it benefits from open
interfaces around:

- a portable Agent Run envelope and identity model;
- a vocabulary for capability dimensions and constraints;
- provider capability discovery and per-dimension enforcement evidence;
- effect identity, lifecycle, promotion, and compensation records;
- semantic checkpoint manifests and effect frontiers;
- causal lineage across parent Runs, delegated Agents, artifacts, and tools;
- evidence bundles that can be verified outside the producing platform;
- conformance profiles for personal, enterprise, and multi-tenant operation.

Standardization at this layer would let Agent frameworks remain creative,
execution runtimes remain specialized, and organizations choose infrastructure
without giving up authority, continuity, or accountability.

## Category definition

A hypervisor lets multiple operating systems share a machine safely. An
AgentVisor lets multiple Agents share execution environments safely. Downward,
it unifies heterogeneous compute and isolation substrates. Upward, it exposes
a stable Agent virtual execution environment and places authority, state,
effects, and evidence inside the same virtualization boundary.

**AgentVisor is the virtualization infrastructure for Agent execution.**

## Continue from the category to the product

- [Run, Attempt, and Effect](run-model.md) defines the portable execution object.
- [Capabilities and evidence](capabilities-and-evidence.md) defines authority and enforcement reporting.
- [pVisor Overview](../index.md) explains Persisting's implementation of the category.
- [Local to fleet](../../system-design/local-to-fleet.md) explains which contracts survive placement changes.
