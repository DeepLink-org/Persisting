# User Guide

Welcome to the Persisting User Guide. This guide covers the core concepts and usage patterns.

## Overview

Persisting provides persistent storage backends for Pulsing's distributed queue system. It enables reliable data storage with features like:

- **Lance-based persistence** - High-performance columnar storage
- **Write-Ahead Log (WAL)** - Data durability guarantees
- **Schema evolution** - Flexible data structure changes
- **Monitoring** - Built-in metrics and observability

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Pulsing Queue                        │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐     │
│  │ QueueWriter │  │ QueueReader │  │   Queue     │     │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘     │
│         │                │                │             │
│         └────────────────┼────────────────┘             │
│                          ▼                              │
│              ┌───────────────────────┐                  │
│              │   StorageManager      │                  │
│              └───────────┬───────────┘                  │
│                          ▼                              │
│              ┌───────────────────────┐                  │
│              │   BucketStorage       │                  │
│              └───────────┬───────────┘                  │
│                          │                              │
└──────────────────────────┼──────────────────────────────┘
                           ▼
┌──────────────────────────────────────────────────────────┐
│                  StorageBackend Protocol                 │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │MemoryBackend│  │ LanceBackend│  │PersistingBackend│  │
│  └─────────────┘  └─────────────┘  └─────────────────┘  │
│       (Pulsing)        (Persisting)     (Persisting)     │
└──────────────────────────────────────────────────────────┘
```

## Core Concepts

### Storage Backends

Storage backends implement the `StorageBackend` protocol and provide different storage strategies:

| Backend | Location | Persistence | Use Case |
|---------|----------|-------------|----------|
| `MemoryBackend` | Pulsing | No | Testing, temporary data |
| `LanceBackend` | Persisting | Yes | Production workloads |
| `PersistingBackend` | Persisting | Yes | Advanced features (WAL, metrics) |

### Backend Registration

Backends must be registered before use:

```python
from pulsing.queue import register_backend
from persisting.queue import LanceBackend

register_backend("lance", LanceBackend)
```

### Backend Selection

Specify the backend when creating a queue:

```python
from pulsing.queue import write_queue

# By name (must be registered)
writer = await write_queue(system, "topic", backend="lance")

# By class directly
writer = await write_queue(system, "topic", backend=LanceBackend)
```

## Topics in This Guide

- [Queue Backends](backends.md) - Overview of storage backends
- [Lance Backend](lance.md) - Using Lance for persistence
- [Persisting Backend](persisting.md) - Enhanced backend with WAL
- [Custom Backends](custom.md) - Implementing custom backends

## Next Steps

Choose the topic that best matches your needs, or continue with the [Queue Backends](backends.md) overview.

