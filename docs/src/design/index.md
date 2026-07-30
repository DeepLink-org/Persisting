# Architecture & Internals

These documents explain how Persisting is built. They are not the primary
starting point for using a capability; begin with [Choose a Capability](../guide/index.md)
for supported workflows.

## Read by subsystem

| Subsystem | Start here | Then read |
|---|---|---|
| Queue | [Queue persistence](architecture.md) | [Custom backend guide](../guide/custom-backends.md) |
| Capture and trajectories | [Capture pipeline](capture.md) | [Trajectory storage](trajectory.md) → [Markdown format](trajectory-format.md) → [RFC-0001 Storyline](../rfcs/0001-storyline-format.md) / [RFC-0002 Events](../rfcs/0002-events-format.md) |
| pPilot | [pPilot control plane](ppilot.md) | [pPilot guide](../guide/ppilot.md) |
| Tensor Memory (experimental) | [TTAS model](tensor-address-space.md) | [Tiered storage](distributed-tiered-storage.md) → [BlockStore](block-store.md) |
| CLI boundary | [CLI architecture](cli.md) | command references under **Reference** |

## Maturity and scope

| Area | Status | Notes |
|---|---|---|
| Capture, Queue, Search, pPilot | Implemented | Each has an independent product path and storage model |
| TTAS / tiered tensor memory | Experimental | Host/SSD work exists; GPU and cross-node data paths remain planned |
| Research comparisons | Reference | Input to future design, not a product commitment |

## Design principles

1. Keep user programming models small and capability-specific.
2. Use Lance as a durable baseline where a subsystem needs columnar storage.
3. Keep control-plane concerns separate from data movement and user execution.
4. Treat TTAS as an experimental internal substrate until its end-to-end data path is complete.
5. Prefer explicit failure and recovery semantics over implied exactly-once guarantees.
