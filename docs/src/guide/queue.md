# Queue

Sometimes you don't need random access — you need a stream. Training data flowing from producers to consumers. Agent trajectory events appending in order. Log records you'll scan sequentially later.

Persisting Queue gives you persistent, append-only queues backed by the same Lance engine as everything else. Start locally with three lines, scale to distributed with zero code changes.

---

## The Simplest Thing That Works

```python
import asyncio
from persisting import Queue

async def main():
    q = Queue("events", storage_path="./data")

    await q.put({"step": 1, "reward": 0.5})
    await q.put({"step": 2, "reward": 0.8})
    await q.flush()

    records = await q.get(limit=100)
    print(f"Read {len(records)} records")  # Read 2 records

asyncio.run(main())
```

That's it — Lance-backed persistence, auto-flush at `batch_size`, recovery on restart. Records accumulate in a memory buffer, flush to disk when the buffer fills or you call `flush()` explicitly.

```python
q = Queue("events",
    storage_path="./data",
    batch_size=100,                # flush when 100 records accumulate
    auto_flush_interval_sec=1.0,   # or every second, whichever comes first
    enable_metrics=True,           # track put/get/flush counts
)
```

---

## From Local to Distributed — Zero Code Changes

When Pulsing is running, Queue automatically switches to distributed mode. The same `q.put()` and `q.get()` calls now route through Pulsing actors, with data sharded across nodes by consistent hashing:

```python
import pulsing

await pulsing.init()           # start Pulsing cluster
q = Queue("events")            # automatically distributed — same API

await q.put({"step": 1})       # hashed to a bucket, routed to a node
records = await q.get(limit=100)  # reads from all buckets
```

Under the hood, records are hashed by `bucket_column` (default `"id"`) into `num_buckets` shards. Each shard is owned by a Pulsing BucketStorage actor on some node. Readers can be assigned subsets:

```python
# Two consumers, each reading different buckets
reader_0 = q.reader()   # under Pulsing: rank=0, world_size=2
reader_1 = q.reader()   # under Pulsing: rank=1, world_size=2
```

For large tensor payloads, enable zero-copy transfer:

```python
q = Queue("events", zerocopy_mode="auto")   # avoids pickle + copy for tensor data
```

---

## Beyond Simple Messages: KV and Tensor APIs

Plain `put`/`get` works for records. But training pipelines often need more: keyed access, field-level reads, consumption tracking.

### Batch Read with Metadata

For tensor-heavy workloads, `get_meta` + `get_data` gives you a TransferQueue-compatible two-phase read:

```python
from persisting import Queue, SequentialSampler

q = Queue("training_data", storage_path="./data")
reader = q.reader()

# Phase 1: what's available?
meta = await reader.get_meta(
    fields=["input_ids", "attention_mask", "labels"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=SequentialSampler(),
)
# meta.size, meta.global_indexes, meta.field_names

# Phase 2: pull the actual tensors
batch = await reader.get_data(meta, partition_id="train_0")

# Or do both in one call
batch = await reader.get_batch(
    fields=["input_ids", "attention_mask"],
    batch_size=32,
    task_name="actor_train",
    partition_id="train_0",
    sampler=SequentialSampler(),
)
```

This decouples "deciding what to read" from "reading the data" — useful when different consumers need different subsets of fields.

### Key-Value Access

For workloads that need random access by key (not just sequential scan), wrap a Queue with `KVInterface`:

```python
from persisting import Queue, KVInterface

q = Queue("kv_store", storage_path="./data")
kv = KVInterface(q)

# Put by key
await kv.kv_put("sess-42", data=tensor_dict, partition_id="prod",
    tag={"status": "complete", "model": "llama-70b"})

# Batch put
await kv.kv_batch_put(
    ["sess-43", "sess-44"],
    data=two_session_batch,
    partition_id="prod",
)

# Retrieve by keys
data = await kv.kv_batch_get(["sess-42", "sess-43"], partition_id="prod")

# List keys and their tags
pairs = await kv.kv_list("prod")
# → [("sess-42", {"status": "complete", ...}), ("sess-43", {}), ...]

# Clean up
await kv.kv_clear(["sess-42"], partition_id="prod")
```

### Consumption Tracking

For training loops that need to know what's been consumed:

```python
# Mark records as processed by this task
await q.mark_consumed("actor_train", global_indexes=[1, 2, 3], partition_id="train_0")

# Reset consumption state for a fresh training run
await q.reset_consumption("actor_train", partition_id="train_0")
```

---

## Sampling Strategies

Samplers control which records your reader pulls. Pick the one that matches your training setup:

```python
from persisting import SequentialSampler, RankAwareSampler, GRPOGroupNSampler

# Simple: consume in order
seq = SequentialSampler()

# Distributed training: each rank gets independent shards
rank = RankAwareSampler()

# GRPO: sample N items from each prompt group
grpo = GRPOGroupNSampler(n_samples_per_prompt=4)
```

---

## Streaming for Long-Running Consumers

When your consumer outlives the data currently in the queue, block until new records arrive:

```python
async for batch in q.stream(limit=1000, wait=True, timeout=5.0):
    process(batch)
```

- `wait=True`: don't return empty — block until data arrives
- `timeout=5.0`: give up after 5 seconds of waiting

---

## Storage Layout

```
./data/events/
└── data.lance/         ← Lance columnar dataset
    ├── _versions/      ← versioned writes
    └── data/           ← column files
```

Records are stored in Lance columnar format. Schema is inferred from the first batch — `int` → `int64`, `float` → `float64`, `bool` → `bool`, `str` → `string`, everything else → pickled binary. The Lance engine handles consistency, recovery, and efficient columnar scans.

---

## Next Steps

- [API Reference — Queue](../api/queue.md) — all method signatures
- [Custom Backends](custom-backends.md) — replace Lance with your own storage engine
