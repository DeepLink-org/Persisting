# From model state to Agent history

**Persisting is persistent infrastructure for the Agent era.** It connects the
state an Agent needs to work with the history needed to understand what it did.

## One persistence continuum

| Layer | Examples | Why persistence matters |
| --- | --- | --- |
| Model state | model parameters and checkpoints | load, share, version, and recover model state |
| Inference state | KV caches and reusable intermediate state | avoid repeated work across requests and Runs |
| Agent history | trajectories, tool calls, execution records, and effects | review, query, compare, and reproduce behavior |

These layers do not have to use one physical format or one API. The shared
idea is durable identity and explicit lifecycle: reusable state should survive
the process that created it, and completed work should remain inspectable.

## Current user workflows

The current product starts with Agent execution and history. Choose the entry
point that matches the task in front of you; neither is a setup step for the
other.

| Command | Start here when you need to | Durable result |
| --- | --- | --- |
| `pvisor` | run one existing Agent with explicit workspace and runtime controls | a Run result, private Run Bundle, and staged filesystem changes |
| `pchronicle` | inspect or exchange Agent trajectory data | a browsable and queryable Dataset view |

pVisor works without pChronicle, and pChronicle can read external trajectories
that never passed through pVisor.

## Workflow 1: run and review one Agent

Use pVisor when the immediate question is: “How do I let this Agent work while
keeping its filesystem changes reviewable?”

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --all    # or: pvisor drop last
```

The Agent works in a Run-owned stage. `review` shows the staged changes;
`apply` accepts selected changes into the base workspace; `drop` discards them.
The exact filesystem and network boundary depends on the selected execution
provider and is recorded with the Run.

[Complete the first pVisor Run →](pvisor/get-started.md)

## Workflow 2: explore Agent history

Use pChronicle when you have trajectory data and need to browse, query, analyze,
import, export, or serve it.

```bash
pchronicle onboard
pchronicle onboard query
```

The walkthrough creates temporary example Datasets. A Dataset can be a local
path, an object-store URI prefix, or a configured alias. It may contain data
captured by Persisting or imported from supported external formats.

[Explore the first Dataset →](pchronicle/get-started.md)

## Optional integration

The execution and history paths can be connected through configured capture:

```text
pVisor Run ── configured Gateway/lifecycle capture ──> pChronicle Dataset

External trajectory files ───────────────────────────> pChronicle Dataset
```

The configured handoff publishes Gateway trajectory events and selected
lifecycle records. It does not automatically publish the full private Run
Bundle, all staged effects, or every piece of provider-specific evidence.
Likewise, importing an external trajectory does not retroactively add execution
controls that were never recorded.

![Current Persisting workflows and optional integration](assets/diagrams/persisting/system-products.svg)

## Where to go next

- [Install Persisting](installation.md)
- [Choose a pVisor execution environment](pvisor/guides/execution.md)
- [Review and selectively apply Agent changes](pvisor/guides/review-apply.md)
- [Read the pChronicle CLI guide](pchronicle/reference/cli.md)
- [Inspect architecture and delivery boundaries](system-design/index.md)
