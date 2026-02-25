# Architecture

## Overview

Persisting extends Pulsing with distributed tiered memory: **Pulsing handles the control plane** (actor runtime), **Persisting handles the data plane** (multi-dimensional addressing, GPU/host/SSD tiering, placement).

This document describes the **queue persistence** architecture — the currently available capability. For the tensor memory architecture (TTAS addressing, tiered storage, KV Cache), see [TTAS](tensor_address_algebra.md) and the [design index](index.md).

### Queue Persistence

Persisting provides pluggable storage backends for Pulsing's distributed streaming queue system. Data flows through Pulsing's actor network and lands in Lance columnar datasets on disk.

```
Application
    │
    ▼ put(record)
QueueWriter (Pulsing)
    │
    ▼ route by bucket_column
StorageManager Actor (Pulsing, per-node)
    │
    ▼ consistent hashing → bucket owner
BucketStorage Actor (Pulsing)
    │
    ▼ delegate to backend
StorageBackend (Persisting)
    │
    ▼ buffer → flush
Lance Dataset (disk)
```

## LanceBackend

Core storage with memory buffering and Lance persistence:

```
┌─────────────────────────────────────────┐
│             LanceBackend                │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │         Memory Buffer           │   │
│  │  (records waiting for flush)    │   │
│  └──────────────┬──────────────────┘   │
│                 │ flush()               │
│                 ▼                       │
│  ┌─────────────────────────────────┐   │
│  │        Lance Dataset            │   │
│  │  (columnar storage on disk)     │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
```

- Records accumulate in a memory buffer.
- When buffer reaches `batch_size` or `flush()` is called, data is written to a Lance dataset.
- Reads merge persisted records and buffered records transparently.
- On startup, the persisted record count is recovered from the existing dataset.

## PersistingBackend

Extends LanceBackend with operation metrics:

```
┌─────────────────────────────────────────┐
│           PersistingBackend             │
│     (inherits from LanceBackend)       │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │       Metrics Collector         │   │
│  │  put_count, get_count,          │   │
│  │  flush_count, last_flush_time   │   │
│  └─────────────────────────────────┘   │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │    LanceBackend (inherited)     │   │
│  │    buffer → flush → Lance       │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
```

## Concurrency Model

All backends use `asyncio.Condition` for thread-safe concurrent access:

- Writers: acquire lock, append to buffer, notify waiting readers.
- Readers: acquire lock, read from persisted + buffer; if `wait=True`, block on condition until new data arrives.
- Flush: acquire lock, swap buffer, release lock, write to Lance.

## Bucket Distribution

Pulsing distributes records across buckets using consistent hashing. Each bucket is owned by a node, and Persisting's backend runs inside each `BucketStorage` actor:

```
Record → hash(record[bucket_column]) % num_buckets → bucket_id
bucket_id → owner_node (consistent hashing over cluster members)
owner_node → BucketStorage actor → StorageBackend instance
```

## Tensor Memory (next phase)

Queue persistence is one layer of Persisting's data plane. The next phase adds **tensor memory** — the core distributed tiered memory capability:

- **TTAS addressing**: Multi-dimensional tensor addressing (`kv["s1", 0, 2, 0:512]`) with canonicalization, routing, and batch optimization. See [TTAS](tensor_address_algebra.md).
- **Tiered storage**: GPU ↔ host ↔ SSD, transparent to application code. Data placement driven by TTAS partition keys.
- **Use cases**: KV Cache offloading, parameter serving, trajectory storage — all as views over the same distributed tiered memory.

The queue architecture above continues to serve as the streaming/event backbone, while tensor memory handles the high-bandwidth tensor data path.
