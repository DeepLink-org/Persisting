"""Persisting Queue — append-only persistent queue on Lance storage engine.

Usage:
    from persisting import Queue

    queue = Queue("my_topic", storage_path="./data")
    await queue.put({"id": "1", "value": 42})
    await queue.flush()
    records = await queue.get(limit=100)
"""

from .backend import LanceBackend, PersistingBackend
from .kv_interface import KVInterface
from .metadata import BatchMeta, FieldMeta, SampleMeta
from .queue import Queue, QueueReader, QueueWriter

__all__ = [
    "Queue",
    "QueueWriter",
    "QueueReader",
    "KVInterface",
    "FieldMeta",
    "SampleMeta",
    "BatchMeta",
    "LanceBackend",
    "PersistingBackend",
]
