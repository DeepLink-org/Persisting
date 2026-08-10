# Persisting

**Agent execution, orchestration, and durable history.**

Persisting's Agent infrastructure has three peer components: **pVisor** manages
one Agent Run, **pPilot** orchestrates many Runs, and **pChronicle** stores
canonical Run history. Gateway, OverlayNet, Control, and OverlayFS are runtime
drivers assembled by pVisor.

```text
pPilot ── RunSpec ──► pVisor ── EventRecord ──► pChronicle
  │                    │                         ▲
  └──── history ───────┴─────────────────────────┘
                       ├─ Control
                       ├─ OverlayFS
                       └─ OverlayNet → Gateway sink
```

---

## Experimental Unified Data-Plane Vision

```
┌─────────────────────────────────────────────────────────────────┐
│                      Application                                 │
│                                                                  │
│   pvisor run            persisting.open()       Queue           │
│   (agent proxy)         (tensor subscript)      (event stream)  │
│                                                                  │
├─────────────────────────────────────────────────────────────────┤
│                    Persisting Data Plane                         │
│                                                                  │
│   ┌─────────────────────────────────────────────────────────┐   │
│   │                      TTAS                                │   │
│   │         Tiered Tensor Address Space                      │   │
│   │                                                          │   │
│   │   Trajectories:  (run_id, time)                          │   │
│   │   Parameters:    (param_id, shard)                       │   │
│   │   KV Cache:      (session, layer, head, time)            │   │
│   │                                                          │   │
│   │   All share the same address model, same routing,        │   │
│   │   same block-tiering across GPU / host / SSD.            │   │
│   └─────────────────────────────────────────────────────────┘   │
│                                                                  │
│   Tiering:  GPU (L0)  ↔  Host (L1)  ↔  SSD (L3)                │
│   Route:    Pulsing actor runtime                                │
├─────────────────────────────────────────────────────────────────┤
│                     pChronicle Store                            │
│                                                                  │
│   Lance (columnar)  ·  Lance (trajectory)  ·  Numpy (memory)  │
└─────────────────────────────────────────────────────────────────┘
```

The diagram above is a target for TTAS/tiered tensor storage, not the current
pChronicle trajectory architecture. Tensor Memory, Queue, and Search remain
additional capability-specific data systems.

---

## Quick Start

```bash
pip install persisting
```

The platform wheel installs the Python package and the matching `persisting`,
`pvisor`, and `ppilot` host binaries into the same Python scripts directory.
Trajectory and Search operations are provided directly by pChronicle; no
separate Engine library is installed. Source developers can still use
`just install-cli` to install the three binaries into a Cargo bin directory.

Run `just examples` to execute the small quantitative experiments under
[`examples/`](examples/). Each script runs the product commands in order and
prints the generated files, reports, and query results directly.
Use `just examples-ppilot` to run the focused `run`, `produce`, `process`, and
`analysis` demonstrations.

### Safe Agent Run

Start with a transactional workspace and a durable review bundle:

```bash
pvisor run --safe codex
pvisor review last
pvisor checkpoint last --name accepted-base
# choose apply, drop, or explore a branch
pvisor fork last --checkpoint accepted-base --workspace /tmp/codex-fork -- codex
```

### Batch Trajectory Workflows

```bash
# pPilot creates many independent, reviewable pVisor Runs.
ppilot produce production.py --output ./runs --parallelism 16 -- --count 100

# pPilot runs SQL over deterministic balanced ATIF shards.
ppilot analysis ./atif --output ./analysis --parallelism 8 \
  --sql 'SELECT session_id, COUNT(*) AS steps FROM steps GROUP BY session_id'

# Transfer a Python map/reduce job to multiple Pulsing mappers.
ppilot process ./atif --script metrics.py --mappers 8 --output ./processed

# Two-level pChronicle count: Pulsing workers compute partials, pPilot merges.
ppilot process ./atif --output ./counts --mappers 8 --count steps
```

Production writes one `run-bundle.json` per Run plus a batch report. Analysis
writes partition JSONL, a deterministic combined JSONL file, and a shard report;
processing writes typed partial aggregates and their checked global reduction.
See the [pPilot component guide](crates/persisting-ppilot/README.md).

The low-privilege `--safe` profile stages workspace writes and observes
cooperative proxy traffic. On Linux, the default host executor now enters an
unprivileged user/mount namespace, builds a minimal bind-projected root,
applies Landlock to the complete Agent process tree, drops capabilities, and
fails closed when that boundary cannot be installed. `--overlaynet-deny-all`
also places the process in a private network namespace; allowlist/public proxy
modes remain cooperative. macOS keeps the
review-only host path and labels it accordingly. Docker/Podman and libkrun/KVM
remain stronger placement options, and the Run Bundle records the effective
boundary rather than inferring it from the requested mode.

### Agent Trajectories

Execute an Agent through the unified CLI and record its LLM calls:

```bash
persisting execute --workspace ./run \
  --gateway-mode capture \
  --gateway-route 'name="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --gateway-stream-markdown \
  -- claude
```

Trajectories are stored as `(agent_id, run_id, time)` — the same TTAS model used for KV cache and parameters.

### Model Parameters

Address weights by name and shard:

```python
import persisting
from persisting.core import Dimension

PARAM_ID = Dimension("param_id", "str")
SHARD    = Dimension("shard", "int")

ps = persisting.open("params/llama-70b",
    dims=(PARAM_ID, SHARD),
    backend="tiered",
    shape=(100, 8),
)

weights = ps["embed.weight", 0].tensor()
ps["lm_head.weight", 0].put(updated_tensor)
```

### KV Cache

Cross-session, multi-layer KV cache with block-tiered storage:

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
    backend="tiered",
    shape=(100, 32, 8, 4096),
    block_tokens=64,
)

# Write: GPU → tiered storage
kv["s1", 0, 2, 100].put(kv_tensor)

# Read: from fastest available tier (CPU / SSD, GPU planned)
arr = kv["s1", 0, 2, 0:512].tensor()
```

---

## What's Available

| Capability | Status | Description |
|------------|--------|-------------|
| **pVisor agent runtime** | ✅ Implemented | One Run: execution, control, Agent ABI, transactional workspace |
| **pPilot orchestration** | ✅ Implemented | `plan()` + `execute()` with lease fencing and durable recovery (Phase-1) |
| **pChronicle history** | ✅ Implemented | Canonical events, Storyline/ATIF views, local + S3, read-only SQL |
| **Gateway capture** | ✅ Implemented | LLM/Agent HTTP capture as Lance + Markdown trajectories |
| **Container / KVM executors** | 🧪 Nightly runtimes | Docker/Podman delegation or a libkrun guest rooted at pVisor OverlayFS |
| **Streaming Queue** | ✅ Stable | Lance-backed append/consume, KV API, samplers |
| **Agent Search** | ✅ Stable | Document indexing, IVF-PQ, hybrid search |
| **Tensor Memory (TTAS)** | 🧪 Experimental | Multi-dim tensor subscript, tiered backends |
| **Transparent network enforcement** | 📋 Planned | Linux netns / seccomp interception |
| **Cross-node KV Cache** | 📋 Planned | Pulsing + RDMA data plane |

---

## Why Persisting?

### One addressing model for all AI data

Trajectories, parameters, and KV cache aren't separate silos — they're all multi-dimensional data. Persisting uses **TTAS** (Tiered Tensor Address Space) as a single addressing model:

| Workload | Dimensions | Access Pattern |
|----------|-----------|----------------|
| KV Cache | `(session, layer, head, time)` | Point query + range scan + prefetch |
| Parameters | `(param_id, shard)` | Batch point query |
| Trajectories | `(run_id, time)` | Sequential range scan |

### Lance as the common baseline

All data — whether it's a trajectory event log, a parameter shard, or a KV cache block — lands on Lance columnar storage. Upper tiers (host memory, GPU) are accelerations built on top of that baseline, driven by the TTAS address structure.

### Pulsing-powered distribution

Cross-node routing and placement via [Pulsing](https://github.com/DeepLink-org/pulsing)'s actor runtime. Pulsing handles the control plane (discovery, messaging, lifecycle); Persisting handles the data plane.

---

## Storage Tiers

| Tier | Latency | Role | Status |
|------|---------|------|--------|
| GPU (L0) | ~μs | Hot cache | Planned |
| Host (L1) | ~100ns / ~10μs | Warm buffer | Available |
| Remote (L2) | ~2μs (RDMA) | Cross-node | Planned |
| SSD (L3) | ~10μs | Cold, durable baseline | Available |

---

## Installation

```bash
pip install persisting[lance]        # Full
pip install persisting               # Minimal
```

The same wheel installs the unified CLI (`execute/env/batch/query/history/eval/gateway`):

```bash
persisting --version
pvisor --version
ppilot --help
```

The unified command deliberately ships as a matched component set. `persisting`
dispatches execution/environment commands to the sibling `pvisor` binary and
batch/query commands to the sibling `ppilot` binary. `PERSISTING_PVISOR_BIN`
and `PERSISTING_PPILOT_BIN` remain explicit overrides.

### Nightly wheel

The nightly wheel includes the matching `persisting`, `pvisor`, and `ppilot`
commands and does not require a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

Container/KVM executors require a compatible pVisor runtime supplied explicitly
through their `pvisor_binary` setting; nightly releases do not publish a separate
guest runtime.

---

## Documentation

| Document | Description |
|----------|-------------|
| [Quick Start](https://deepLink-org.github.io/Persisting/quickstart/) | Get started in 5 minutes |
| [User Guide](https://deepLink-org.github.io/Persisting/guide/) | Capture, tensor memory, queue, search |
| [API Reference](https://deepLink-org.github.io/Persisting/api/) | Full API documentation |
| [Design Docs](https://deepLink-org.github.io/Persisting/design/) | Architecture, TTAS, tiered storage |

---

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and components distributed under their own licenses.
