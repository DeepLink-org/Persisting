---
template: home.html
title: Persisting - Persistent Storage for Parameters, KV Cache, and Trajectories
description: Distributed tiered memory for AI — extends Pulsing with multi-dimensional addressing, GPU/host/SSD tiering, and distribution via actor runtime.
hide: toc
---

<!-- This content is hidden by the home.html template but indexed for search -->

# Persisting

**Persistent Storage for Parameters, KV Cache, and Trajectories.**

Extends [Pulsing](https://github.com/DeepLink-org/pulsing) (distributed actor runtime) with distributed tiered memory: GPU ↔ host ↔ SSD, multi-dimensional tensor addressing, Pulsing-powered distribution.

## Core Idea

- **Pulsing** = control plane (actor discovery, messaging, lifecycle)
- **Persisting** = data plane (multi-dimensional addresses, tiered memory, placement)

Together: a runtime where actors communicate via Pulsing and share tensor data via Persisting — data lives across memory tiers, addressed by tensor-style subscript, materialized on demand.

## Features

- **Tiered Tensor Address Space (TTAS)** — Multi-dimensional, tiered address space for AI data (KV Cache, parameters, trajectories); the counterpart to PGAS.
- **Tiered Memory** — GPU ↔ host ↔ SSD, transparent to application code.
- **Pulsing Distribution** — Cross-node data access via Pulsing's actor runtime.
- **Tensor-style API** — `kv["s1", 0, 2, 0:512].tensor()` — slice by subscript, materialize on demand.

## Quick Start

```bash
pip install persisting
```

```python
import persisting
from persisting.core import Dimension

SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME), order_dim=TIME)

arr = kv["s1", 0, 2, 0:512].tensor()
```

## Use Cases

| Use Case | Dimensions | Primary Access Pattern |
|----------|-----------|----------------------|
| **KV Cache Offloading** | (session, layer, head, time) | Point query + range scan + prefetch |
| **Parameter Serving** | (param_id, shard) | Batch point query |
| **Trajectory Storage** | (run_id, time) | Sequential range scan |

## Community

- [GitHub Repository](https://github.com/DeepLink-org/Persisting)
- [Issue Tracker](https://github.com/DeepLink-org/Persisting/issues)
- [Discussions](https://github.com/DeepLink-org/Persisting/discussions)
