# Architecture

## Overview

This document describes Persisting's **queue persistence** architecture.

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
