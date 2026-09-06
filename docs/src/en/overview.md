# Start with the question you have

Persisting has two independent entry points. Start with the one that matches
the work in front of you, then follow the short path to a useful result.

## I want to run an Agent safely and review its changes

Start with **pVisor**. It gives one Agent a Run-owned workspace, records the
execution boundary, and leaves filesystem changes staged until you decide what
enters the real project.

1. [Install the command line tools](installation.md).
2. [Run your first Agent](pvisor/get-started.md).
3. [Review and selectively apply changes](pvisor/guides/review-apply.md).
4. [Choose a host, OCI, or VM environment](pvisor/guides/execution.md).

You should finish with a completed Run, a readable Run Bundle, and either an
applied or discarded stage.

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

## I already have Agent trajectories

Start with **pChronicle**. It can inspect local or object-store data and can
also import supported external formats. The first walkthrough creates temporary
example data, so you can learn the query flow before preparing a Dataset.

1. [Explore a first Dataset](pchronicle/get-started.md).
2. [Discover and query your own data](pchronicle/guides/discover-and-query.md).
3. [Import or export a supported format](pchronicle/guides/exchange.md).
4. [Serve a Dataset locally](pchronicle/guides/serve.md).

You should finish with a read-only query, a normalized view, and a clear Source
lineage for the data you inspected.

```bash
pchronicle onboard query
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source'
```

## I want execution and history together

Connect the two products only after each standalone workflow works. Configure
pVisor capture to publish selected Gateway trajectory events and lifecycle
records into pChronicle. The handoff is explicit and narrow: it does not move
the private Run Bundle or invent evidence that the original Source did not
provide.

1. [Capture Agent trajectories](pvisor/guides/capture.md).
2. [Understand the event and sidecar contract](rfcs/0007-events-contract-pchronicle-sidecar.md).
3. [Read the execution-to-history architecture](system-design/architecture.md).

```text
pVisor Run ── configured capture ──> canonical event Source ──> Dataset views
external trajectory Source ──────────────────────────────────> Dataset views
```

## I need to understand the boundary before I run anything

Read the concepts in this order:

1. [Run, Attempt, and Effect](pvisor/concepts/run-model.md) — the stable objects.
2. [Capabilities and evidence](pvisor/concepts/capabilities-and-evidence.md) — what a Run can claim.
3. [Execution environments](pvisor/guides/execution.md) — how provider choice changes the boundary.
4. [Security and evidence](system-design/security-evidence.md) — what persists and what stays local.

The documentation uses one rule throughout: a successful command does not imply
that every requested capability was enforced. The Run Bundle records the
mechanisms and limitations that actually applied.

## Keep going

- [pVisor command model](pvisor/design/cli.md)
- [pVisor case catalog](pvisor/reference/cases.md)
- [pChronicle concepts](pchronicle/concepts/index.md)
- [System design](system-design/index.md)
