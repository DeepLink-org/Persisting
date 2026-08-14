# Python API Reference

The Python wheel provides the persistent Queue and sampling APIs. Agent-runtime
capabilities such as pVisor, pPilot, and pChronicle are exposed by the native
CLI programs bundled in the same platform wheel.

| Module | Use for | Status |
|--------|---------|--------|
| [Queue](queue.md) | `persisting.Queue` — event streaming, KV interface, samplers | Stable |

## Queue and sampling

```python
from persisting import Queue, SequentialSampler

queue = Queue("events", storage_path="./data")
sampler = SequentialSampler()
```

See the [Queue API](queue.md) for the complete interface.
