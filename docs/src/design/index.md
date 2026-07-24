# Design Documents

Persisting's architecture, addressing model, and storage design.

---

## Architecture

| Document | Description |
|----------|-------------|
| [Architecture](architecture.md) | Queue persistence and system overview |
| [CLI Architecture](cli.md) | Thin CLI + dynamic engine loading (中文) |

## Tiered Storage

| Document | Description |
|----------|-------------|
| [TTAS Addressing](tensor-address-space.md) | Tiered Tensor Address Space — formal addressing model |
| [Distributed Tiered Storage](distributed-tiered-storage.md) | Block model, virtual address mapping, Pulsing integration |
| [Block Store Internals](block-store.md) | Block table, page fault handling, event loop (中文) |

## Capture & Trajectory

| Document | Description |
|----------|-------------|
| [Capture Architecture](capture.md) | LLM proxy, event model, dual storage (中文) |
| [Trajectory Storage](trajectory.md) | Vortex canonical + Markdown materialization (中文) |
| [Trajectory Markdown Format](trajectory-format.md) | TLV block model, frontmatter, live upsert (中文) |
| [Traj CLI](cli-traj.md) | `persisting traj` command reference (中文) |
| [Capture CLI](cli-capture.md) | `traj capture` / `traj proxy` subcommands (中文) |

## Compute

| Document | Description |
|----------|-------------|
| [Compute Architecture](compute.md) | Driver/Worker, scheduling, sink, checkpoint (中文) |

## Search

| Document | Description |
|----------|-------------|
| [Search CLI](cli-search.md) | `persisting search` command design (中文) |

---

## Reference & Analysis

| Document | Description |
|----------|-------------|
| [Similar Systems](references/similar-systems.md) | LMCache, vLLM, UMap, CUDA VMM comparison |
| [vs TransferQueue](references/transfer-queue-comparison.md) | Scoring and migration analysis |
| [TransferQueue Interface](references/transfer-queue-interface.md) | API comparison table |
| [LMCache KV Cache](references/lmcache.md) | LMCache analysis for KV Cache implementation (中文) |

---

## Implementation Tracking

| Document | Description |
|----------|-------------|
| [Tiered Storage Steps](../dev/tiered-storage-steps.md) | Step-by-step implementation with test checklists |

---

## Design Principles

1. **Lance is the baseline** — All caches and accelerations are built on top of "reads from file."
2. **One foundation, multiple patterns** — Trajectory, Search, KV, Queue share the Lance ecosystem.
3. **Trajectory dual-view** — Vortex (canonical) + Markdown (materialized); live upsert from capture.
4. **Capture is self-contained** — Embedded proxy captures LLM traffic; IDE import is supplementary.
5. **TTAS is internal** — Users see `kv[key].tensor()`, not raw address algebra.
6. **Performance is product** — P99 latency, GPU utilization, capture real-time fidelity.
