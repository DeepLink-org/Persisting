---
template: home.html
title: Persisting - Persistent Storage for AI Systems
description: Persistent storage solution for Parameters, KV Cache, and Trajectories. Built on Lance columnar format with Pulsing Actor framework integration.
hide: toc
---

<!-- This content is hidden by the home.html template but indexed for search -->

# Persisting

**Persisting** is a persistent storage solution for AI systems, providing efficient storage for Parameters, KV Cache, and Trajectories.

## Key Features

- **Pluggable Backends** - Support for multiple storage backends including Memory, Lance, and custom implementations.
- **Lance Integration** - Built on Lance columnar format for high-performance random access and zero-copy versioning.
- **Write-Ahead Log** - Built-in WAL support ensures data durability and crash recovery.
- **Pulsing Integration** - Seamless integration with Pulsing Actor framework for distributed queue persistence.
- **Schema Evolution** - Support for dynamic schema evolution without downtime.
- **Monitoring Metrics** - Built-in Prometheus metrics export for real-time monitoring.

## Quick Start

```bash
# Install
pip install persisting

# Or install with Pulsing
pip install persisting[pulsing]
```

```python
from pulsing.queue import register_backend, write_queue
from persisting.queue import LanceBackend

# Register Lance backend
register_backend("lance", LanceBackend)

# Use persistent queue
writer = await write_queue(system, "my_topic",
    backend="lance",
    storage_path="/data/queues")

await writer.put({"id": "1", "value": 42})
await writer.flush()
```

## Use Cases

- **KV Cache Storage** - Store and retrieve LLM KV Cache for cross-session reuse.
- **Trajectory Storage** - Persistent storage for RL and training trajectories.
- **Parameter Checkpoints** - Efficient model parameter checkpoint storage with versioning.

## Community

- [GitHub Repository](https://github.com/reiase/Persisting)
- [Issue Tracker](https://github.com/reiase/Persisting/issues)
- [Discussions](https://github.com/reiase/Persisting/discussions)

