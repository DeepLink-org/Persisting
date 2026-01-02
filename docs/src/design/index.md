# Design Documents

This section contains design documents describing Persisting's architecture and implementation details.

## Overview

Persisting is designed as a modular persistent storage system that integrates with Pulsing's distributed queue infrastructure. Key design principles include:

- **Pluggable Architecture** - Storage backends can be swapped without changing application code
- **Pulsing Integration** - Seamless integration with Pulsing Actor framework
- **Production Ready** - Enterprise features like WAL, monitoring, and schema evolution

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Application Layer                        │
│                                                                 │
│    ┌─────────────┐     ┌─────────────┐     ┌─────────────┐    │
│    │ QueueWriter │     │ QueueReader │     │   Queue     │    │
│    └──────┬──────┘     └──────┬──────┘     └──────┬──────┘    │
│           │                   │                   │            │
│           └───────────────────┴───────────────────┘            │
│                               │                                 │
└───────────────────────────────┼─────────────────────────────────┘
                                │
┌───────────────────────────────┼─────────────────────────────────┐
│                         Pulsing Layer                           │
│                               │                                 │
│              ┌────────────────┴────────────────┐                │
│              │        StorageManager          │                │
│              │    (Actor per node)            │                │
│              └────────────────┬────────────────┘                │
│                               │                                 │
│         ┌─────────────────────┼─────────────────────┐          │
│         │                     │                     │          │
│    ┌────┴─────┐         ┌────┴─────┐         ┌────┴─────┐     │
│    │ Bucket 0 │         │ Bucket 1 │         │ Bucket N │     │
│    └────┬─────┘         └────┬─────┘         └────┬─────┘     │
│         │                    │                    │            │
└─────────┼────────────────────┼────────────────────┼────────────┘
          │                    │                    │
┌─────────┼────────────────────┼────────────────────┼────────────┐
│         │              Backend Layer              │            │
│         │                    │                    │            │
│    ┌────┴─────┐         ┌────┴─────┐         ┌────┴─────┐     │
│    │ Backend  │         │ Backend  │         │ Backend  │     │
│    │(Memory/  │         │(Lance/   │         │(Custom)  │     │
│    │ Lance)   │         │Persisting│         │          │     │
│    └────┬─────┘         └────┬─────┘         └────┬─────┘     │
│         │                    │                    │            │
└─────────┼────────────────────┼────────────────────┼────────────┘
          │                    │                    │
┌─────────┼────────────────────┼────────────────────┼────────────┐
│         │             Storage Layer               │            │
│         ▼                    ▼                    ▼            │
│    ┌─────────┐          ┌─────────┐          ┌─────────┐      │
│    │ Memory  │          │  Lance  │          │ Custom  │      │
│    │         │          │  Files  │          │ Storage │      │
│    └─────────┘          └─────────┘          └─────────┘      │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

## Documents

- [Architecture](architecture.md) - Overall system architecture
- [Write-Ahead Log](wal.md) - WAL design and implementation

## Design Principles

### 1. Separation of Concerns

Each layer has clear responsibilities:

- **Application Layer**: High-level queue APIs
- **Pulsing Layer**: Distributed actor management
- **Backend Layer**: Storage abstraction
- **Storage Layer**: Actual data persistence

### 2. Protocol-Based Abstraction

The `StorageBackend` protocol defines a clear contract:

```python
class StorageBackend(Protocol):
    async def put(self, record: dict[str, Any]) -> None: ...
    async def get(self, offset: int, limit: int) -> list[dict]: ...
    async def flush(self) -> None: ...
    # ...
```

### 3. Composition over Inheritance

Backends are composed into `BucketStorage` actors rather than inheriting from a base class.

### 4. Fail-Safe Defaults

- Memory backend for testing (no external dependencies)
- Sensible default configurations
- Graceful degradation

## Future Work

- [ ] Multi-region replication
- [ ] Tiered storage (hot/cold)
- [ ] Query optimization
- [ ] Backup and restore utilities

