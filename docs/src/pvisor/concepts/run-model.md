# Run, Attempt, and Effect

pVisor manages an **Agent Run**. A Run is not the process that happens to
execute it, and it is not the container or virtual machine selected for one
attempt.

## Run

A Run is the stable, user-meaningful identity of one Agent task. Its identity,
requested capabilities, parent/child lineage, accepted effects, artifacts, and
terminal result survive executor changes and process exits.

## Attempt

An Attempt is one physical realization of a Run on a provider. Infrastructure
failure may create another Attempt without changing the Run. A semantic retry
is different: it represents a new decision and therefore creates a derived
Run.

```text
Run
├── Attempt 1 → infrastructure failure
└── Attempt 2 → terminal result
```

This distinction allows infrastructure retries while keeping the history
understandable.

## Effect

An Effect is a consequence that matters outside the Agent's reasoning loop.
Filesystem changes, network requests, tool calls, credential use, and external
API mutations are separate effect dimensions. Capturing an effect is not the
same as preventing it, and staging one dimension does not isolate another.

For a staged workspace, the lifecycle is:

```text
execute → inspect stage → apply selected paths zero or more times → drop stage
```

`apply` promotes selected filesystem changes. It does not imply that network or
remote-service effects were rolled back.

## Checkpoint and terminal result

A Checkpoint records a declared consistency frontier for the state a provider
can preserve. The terminal Run result records the final status and references
to evidence and artifacts. Neither should be inferred from a process exit code
alone.

Next, read [Capabilities and evidence](capabilities-and-evidence.md), then use
the [execution guide](../guides/execution.md) to choose a provider.
