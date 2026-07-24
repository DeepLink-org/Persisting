# Design Documents

High-level design for Persisting — tiered storage and addressing (evolving), queue persistence, **trajectory capture & storage** (shipped), and CLI capability model.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│  Application                                                    │
│  Tensor-style KV  ·  Agent trajectory capture  ·  Doc search   │
├─────────────────────────────────────────────────────────────────┤
│  Access patterns: multi-dim lookup · streaming append · search │
├─────────────────────────────────────────────────────────────────┤
│  Core / Engine: TTAS (planned) · tiering · trajectory dual-view │
├─────────────────────────────────────────────────────────────────┤
│  Storage engine: Lance (columnar SSD baseline)                  │
└─────────────────────────────────────────────────────────────────┘
```

See [index.zh.md](index.zh.md) for the full Chinese index with document links and design principles.

---

## Core topics (中文)

| Topic | Document |
|-------|----------|
| Trajectory storage model | [trajectory_storage.zh.md](trajectory_storage.zh.md) (中文) |
| Trajectory Markdown format | [trajectory_tlv_format.zh.md](trajectory_tlv_format.zh.md) |
| Capture architecture | [capture_design.zh.md](capture_design.zh.md) (中文) |
| Compute architecture (Phase-1) | [compute_control_plane.zh.md](compute_control_plane.zh.md) (中文) · [Quick start](../guide/compute_quickstart.zh.md) |
| CLI architecture | [cli_architecture.zh.md](cli_architecture.zh.md) |
| Queue persistence | [architecture.md](architecture.md) |
| TTAS addressing | [tensor_address_algebra.md](tensor_address_algebra.md) |
