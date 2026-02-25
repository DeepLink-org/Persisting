"""Persisting — Persistent Storage for Parameters, KV Cache, and Trajectories.

Distributed tiered memory for AI workloads:
- Multi-dimensional address space (TAA) for KV Cache, parameters, trajectories
- GPU / host / SSD tiering, transparent to application code
- Lance storage engine as SSD-layer baseline

API 与 llms.binding.md 一致：open → kv[key] → h.tensor() / h.put(data)。
"""

__version__ = "0.1.0"

from persisting.store import (
    BlockId,
    BlockStore,
    Handler,
    LocalTensorStore,
    TensorNamespace,
    region_to_index,
)
from persisting.queue import (
    BatchMeta,
    KVInterface,
    LanceBackend,
    PersistingBackend,
    Queue,
    QueueReader,
    QueueWriter,
)
from persisting.sampler import (
    BaseSampler,
    GRPOGroupNSampler,
    RankAwareSampler,
    SequentialSampler,
    get_sampled_batch,
)

__all__ = [
    "Queue",
    "QueueWriter",
    "QueueReader",
    "KVInterface",
    "BatchMeta",
    "LanceBackend",
    "PersistingBackend",
    "BaseSampler",
    "SequentialSampler",
    "RankAwareSampler",
    "GRPOGroupNSampler",
    "get_sampled_batch",
    "open",
    "Handler",
    "LocalTensorStore",
    "BlockStore",
    "BlockId",
    "TensorNamespace",
    "region_to_index",
]


def open(
    namespace: str,
    dims: tuple,
    order_dim=None,
    partition_dims=None,
    *,
    backend: str = "local",
    shape: tuple | None = None,
    dtype=None,
    catalog: dict | None = None,
    block_tokens: int = 64,
):
    """打开一个 tensor 命名空间（与 llms.binding.md 一致）。

    - namespace: 命名空间名（如 "kvcache/v1"）
    - dims: 维度元组 (Dimension, ...)
    - order_dim: 有序维度，支持 range scan（可选；tiered 必填）
    - partition_dims: 分区维度，决定跨节点放置（可选）
    - backend: "local" 单机单层；"tiered" 单机 L1+L3 按 Block 分层
    - shape: 必填，与 dims 一一对应的各维大小
    - dtype: 数组类型，默认 numpy.float32
    - catalog: str/bytes 维度的坐标→整数下标映射
    - block_tokens: tiered 时每块在 order_dim 上的元素个数，默认 64

    返回支持 kv[key] 的句柄，kv[key] 返回 Handler，h.tensor() 物化、h.put(data) 写入。
    """
    if dtype is None:
        import numpy as np
        dtype = np.float32
    if shape is None:
        raise ValueError("open() 须提供 shape")
    if backend == "local":
        store = LocalTensorStore(dims, shape, dtype=dtype, catalog=catalog)
        return TensorNamespace(namespace, dims, store)
    if backend == "tiered":
        if order_dim is None:
            raise ValueError("backend='tiered' 须提供 order_dim")
        store = BlockStore(
            dims,
            shape,
            dtype=dtype,
            order_dim=order_dim,
            block_tokens=block_tokens,
            catalog=catalog,
        )
        return TensorNamespace(namespace, dims, store)
    raise ValueError("backend 须为 'local' 或 'tiered'")
