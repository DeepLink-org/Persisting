# Design Documents

Design documents for Persisting — addressing model, tiered storage, queue persistence, and CLI command design.

---

## Architecture

```
┌────────────────────────────────────────────────────────┐
│  Application                                           │
│  kv["s1", 0, 2, 0:512].tensor()                       │
├────────────────────────────────────────────────────────┤
│  Access Patterns                                       │
│  ┌──────────────┐ ┌──────────────┐ ┌────────────────┐ │
│  │ Multi-dim    │ │  Streaming   │ │ Batch point    │ │
│  │ lookup (KV)  │ │ append(traj) │ │ query (PS)     │ │
│  └──────────────┘ └──────────────┘ └────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Persisting Core                                       │
│  ┌────────────┐  ┌────────────┐  ┌──────────────────┐ │
│  │    TTAS    │  │  Tiering   │  │   Placement /    │ │
│  │ (Tiered    │  │  GPU/Host  │  │   Route          │ │
│  │  Tensor AS)│  │  /SSD      │  │                  │ │
│  └────────────┘  └────────────┘  └──────────────────┘ │
├────────────────────────────────────────────────────────┤
│  Storage Engine: Lance                                 │
│  columnar format · SSD persist · baseline read path    │
└────────────────────────────────────────────────────────┘
```

- **Storage Engine (Lance)**: SSD-layer file format providing the persistence baseline. All upper-layer optimizations accelerate the "read from file" path.
- **Persisting Core**: TTAS addressing, tiered memory (GPU ↔ host ↔ SSD), placement and routing.
- **Access Patterns**: Different access patterns on the same storage engine — streaming append (trajectories), multi-dim lookup (KV Cache), batch point query (PS).

---

## Core Design

- [Tiered Tensor Address Space (TTAS)](tensor_address_algebra.md) — Addressing model (counterpart to PGAS): multi-dimensional addresses, constraint algebra, canonicalization and lowering
- [Architecture](architecture.md) — Queue persistence architecture: LanceBackend, concurrency model, Pulsing integration
- [Distributed Tiered Storage](distributed_tiered_storage.md) — Four-tier storage (GPU / CPU / Remote / SSD), Block model, mmap + UFFD virtual address mapping

## CLI

- [CLI Architecture](cli_architecture.zh.md) — Engine discovery, lazy loading, async Job ABI, wire format, version constants (中文)
- [`persisting search` command design](cli_search_command.zh.md) — Command tree, parameters, interaction design (中文)
- [`persisting trajectory` command design](cli_trajectory_command.zh.md) — Command tree, parameters, interaction design (中文)

## Reference & Analysis

- [LMCache KV Cache Reference](lmcache_kvcache_reference.zh.md) — LMCache analysis for KV implementation (中文)
- [Persisting vs TransferQueue Scoring](persisting_vs_transferqueue_scoring.md) — Dimensional comparison of Queue vs TransferQueue (中文)
- [TransferQueue vs Pulsing Queue Interface Comparison](transfer_queue_interface_comparison.md) — Side-by-side API comparison and migration notes (中文)
- [Similar Systems Reference](similar_systems_reference.md) — LMCache, UMap, CUDA UVM and other related systems

## Implementation Tracking

- [Tiered Storage Implementation Steps](../dev/tiered_storage_implementation_steps.md) — Step 1–11 implementation log and test checklist

## Design Principles

1. **Lance is the floor** — Lance provides the SSD persistence baseline. GPU cache, host memory pools, and cross-node sharing accelerate the "read from file" path.
2. **One engine, multiple access patterns** — Streaming append, multi-dim lookup, and batch get share the same Lance storage engine.
3. **TTAS is internal** — Users see `kv[key].tensor()`. TTAS powers addressing, routing, and batch optimization under the hood.
4. **Performance is the product** — Persisting's value is measured by KV Cache P99, GPU utilization, and memory efficiency.
