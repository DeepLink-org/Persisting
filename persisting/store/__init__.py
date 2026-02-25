"""Persisting Store — 单机分层存储，TAA 地址映射到本地 CPU tensor；元数据与 Backing 分离。"""

from .block import BlockId, block_to_region, region_to_blocks
from .block_store import BlockStore
from .block_store_actor import (
    ActorStore,
    get_block_store_actor_class,
    region_deserialize,
    region_serialize,
)
from .local_tensor import (
    Backing,
    BlockMappedBacking,
    Handler,
    LocalTensorStore,
    MmapBacking,
    NumpyBacking,
    RemoteBacking,
    SafetensorsBacking,
    Store,
    TensorLayout,
    TensorNamespace,
    region_to_index,
)

__all__ = [
    "ActorStore",
    "Backing",
    "BlockId",
    "BlockMappedBacking",
    "BlockStore",
    "get_block_store_actor_class",
    "region_deserialize",
    "region_serialize",
    "Handler",
    "LocalTensorStore",
    "MmapBacking",
    "NumpyBacking",
    "RemoteBacking",
    "SafetensorsBacking",
    "Store",
    "TensorLayout",
    "TensorNamespace",
    "block_to_region",
    "region_to_blocks",
    "region_to_index",
]
