# pVisor

**pVisor is the AgentVisor implementation in Persisting.** It virtualizes Agent
execution: shared host, container, VM, and future fleet resources are presented
to each Run as an isolated Agent virtual execution environment.

![pVisor architecture](../assets/diagrams/pvisor/agentvisor-architecture.svg)

## What pVisor owns

- a stable Run identity independent of the physical process or provider;
- creation, admission, cancellation, recovery, checkpoint, and terminal state;
- workspace, network, tool, model, credential, and compute capabilities;
- containment and review of effects where the medium supports it;
- evidence describing the controls that were actually installed.

pVisor does not define the Agent reasoning loop. It runs existing Agent CLIs,
scripts, and frameworks inside a governed execution boundary.

## Begin with one Run

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --path src
```

The Agent can edit freely inside its stage. The user decides which filesystem
effects enter the base project, and can apply independent batches more than
once.

The standalone pVisor product loop is:

```text
RunSpec -> admission -> Attempt -> Effect review/apply/drop -> Run Bundle
```

A pVisor Run completes with reviewable staged Effects and a private, versioned
Run Bundle. pChronicle is the standard durable Dataset and history handoff, not
a runtime prerequisite for this loop.

## Read pVisor by purpose

| Goal | Section |
| --- | --- |
| Complete the first local Run | [Get Started](get-started.md) |
| Understand the category and object model | [Concepts](concepts/index.md) |
| Choose an executor or govern effects | [Guides](guides/index.md) |
| Inspect isolation and runtime mechanisms | [Design](design/index.md) |
| Look up exact command syntax | [Reference](reference/index.md) |

To make a pVisor Run's events, artifacts, terminal facts, and Evidence part of
a durable, queryable Dataset, continue to [pChronicle](../pchronicle/index.md).
