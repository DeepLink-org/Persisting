# pVisor guides

Follow the lifecycle of one Run, then connect execution to durable history when
needed.

:::note Start with the outcome
Each guide below is task-shaped. Begin with the first outcome you need and
keep the Run Bundle open while you work; it is the authoritative record of
which controls were actually installed.
:::

1. [Choose an execution environment](execution.md).
2. [Review and selectively apply filesystem effects](review-apply.md).
3. [Control network access](network.md).
4. [Capture model traffic and trajectory evidence](capture.md).
5. [Replay and continue an Agent trajectory in a fresh sandbox](sandbox-replay.md).

Filesystem, network, capture, and execution-provider guarantees are separate.
Always inspect the Run Bundle for the mechanisms installed on the current
platform.
