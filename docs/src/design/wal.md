# Write-Ahead Log (WAL)

> **Status: Planned** — WAL is not yet implemented. This document describes the design for future implementation.

## Overview

Write-Ahead Log (WAL) will ensure data durability by recording changes before they are applied to the main storage. This allows recovery of uncommitted data after crashes.

## Design Goals

1. **Crash recovery** — No data loss for acknowledged writes.
2. **Low overhead** — Minimal impact on write throughput.
3. **Simple implementation** — Append-only log with periodic truncation.

## Planned Architecture

```
put(record)
    │
    ├──► WAL append (sequential write)
    │
    └──► Memory buffer
              │
              ▼ flush()
         Lance dataset
              │
              ▼
         WAL truncate
```

## Planned API

```python
from persisting.queue import PersistingBackend

# WAL will be enabled via backend options
backend = PersistingBackend(
    bucket_id=0,
    storage_path="./data",
    enable_wal=True,
)
```

On startup, the backend will:
1. Check for existing WAL files.
2. Replay any uncommitted records into the buffer.
3. Continue normal operation.

## Implementation Plan

- [ ] WAL file format (append-only, with checksums)
- [ ] Write path: append to WAL before buffering
- [ ] Recovery path: replay WAL on startup
- [ ] Truncation: clear WAL after successful flush
- [ ] Configuration: sync interval, max WAL size
