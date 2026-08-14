# Architecture & Internals

These documents explain how Persisting is built. They are not the primary
starting point for using a capability; begin with [Choose a Capability](../guide/index.md)
for supported workflows.

## Read by subsystem

| Subsystem | Start here | Then read |
|---|---|---|
| pVisor | [Agent infrastructure](agent-infrastructure.md) | [Isolation backends](pvisor-isolation.md) → standalone `pvisor` CLI → [Gateway driver](gateway.md) → [OverlayNet interception](overlaynet.md) |
| pChronicle | [`pchronicle` command reference](cli-pchronicle.md) | [Dataset Catalog](dataset-catalog.md) → [Trajectory storage](trajectory.md) → [Storyline three-table Lance](storyline-lance.md) → [RFC-0003 Ownership](../rfcs/0003-pchronicle-ownership.md) |
| pPilot | [pPilot control plane](ppilot.md) | standalone `ppilot` CLI → run orchestration and pChronicle SQL analysis |
| Queue | [Queue persistence](architecture.md) | [Custom backend guide](../guide/custom-backends.md) |
| Gateway capture driver | [Gateway pipeline](gateway.md) | [Markdown format](trajectory-format.md) → [RFC-0001 Storyline](../rfcs/0001-storyline-format.md) / [RFC-0002 Events](../rfcs/0002-events-format.md) |
| Tensor Memory (experimental) | [TTAS model](tensor-address-space.md) | [Tiered storage](distributed-tiered-storage.md) → [BlockStore](block-store.md) |
| CLI boundary | [pVisor](cli-pvisor.md), [pPilot](cli-ppilot.md), [pChronicle](cli-pchronicle.md) | command references under **Reference** |

## Maturity and scope

| Area | Status | Notes |
|---|---|---|
| pVisor, pPilot, pChronicle | Implemented core | Peer Agent execution, orchestration, and history components; pChronicle `search` and `maintain` remain reserved |
| Gateway, OverlayNet, OverlayFS | Implemented | pVisor runtime drivers; Gateway supplies capture semantics |
| pVisor enforced isolation | Implemented / partial | Linux uses FUSE + synthetic root + rootless namespaces + Landlock; macOS uses Seatbelt-enforced staged writes and deny-all socket confinement while reads remain ambient; Docker and libkrun/KVM transports exist; seccomp, resource controls, LiteBox VFS, and Firecracker remain on the [isolation roadmap](pvisor-isolation.md) |
| OverlayNet transparent interception | VM implemented / host planned | libkrun virtio-net + smoltcp is non-bypassable for IPv4 TCP/DNS on Linux and Apple Silicon macOS; Linux host netns/seccomp drivers remain planned; see [design](overlaynet.md) |
| TTAS / tiered tensor memory | Experimental | Host/SSD work exists; GPU and cross-node data paths remain planned |
| Research comparisons | Reference | Input to future design, not a product commitment |

## Design principles

1. Keep user programming models small and capability-specific.
2. Use Lance as a durable baseline where a subsystem needs columnar storage.
3. Keep control-plane concerns separate from data movement and user execution.
4. Treat TTAS as an experimental internal substrate until its end-to-end data path is complete.
5. Prefer explicit failure and recovery semantics over implied exactly-once guarantees.
