"""Block：存储与传输的基本单元，TAA Region 与分层存储的桥接。

- BlockId = (partition_key, block_id)：逻辑块标识。
- region_to_blocks：Region → BlockId 列表。
- block_to_region：BlockId → Region（用于从 Backing 读写该块对应的切片）。
"""

from __future__ import annotations

from typing import Any

from persisting.core import Dimension, Point, Range, Region, project_prefix


def _has_lo_hi(c: Any) -> bool:
    return hasattr(c, "lo") and hasattr(c, "hi")


def _has_value(c: Any) -> bool:
    return hasattr(c, "value") and not _has_lo_hi(c)


class BlockId:
    """逻辑块标识：(partition_key, block_id)。

    - partition_key: prefix_dims 各维的 Point 值（与 dims 中除 order_dim 外的顺序一致）。
    - block_id: order_dim 上的块下标，块覆盖 [block_id*block_tokens, (block_id+1)*block_tokens)。
    """

    __slots__ = ("partition_key", "block_id")

    def __init__(self, partition_key: tuple[Any, ...], block_id: int):
        self.partition_key = tuple(partition_key)
        self.block_id = int(block_id)

    def __hash__(self) -> int:
        return hash((self.partition_key, self.block_id))

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, BlockId):
            return False
        return self.partition_key == other.partition_key and self.block_id == other.block_id

    def __repr__(self) -> str:
        return f"BlockId({self.partition_key!r}, {self.block_id})"


def region_to_blocks(
    region: Region,
    dims: tuple[Dimension, ...],
    order_dim: Dimension,
    block_tokens: int,
    shape: tuple[int, ...],
) -> list[BlockId]:
    """将 TAA Region 转为覆盖该 Region 的 BlockId 列表。

    要求：region 在 prefix_dims（= dims 除 order_dim）上均为 Point，以便得到 partition_key；
    order_dim 上为 Range 或 Point。

    Args:
        region: TAA Region
        dims: 维度顺序，与 shape 一致
        order_dim: 有序维度（块在该维上切分）
        block_tokens: 每块在 order_dim 上的元素个数
        shape: 各维大小，用于边界裁剪

    Returns:
        BlockId 列表，覆盖 region 在 order_dim 上的范围
    """
    if len(dims) != len(shape):
        raise ValueError("dims 与 shape 长度须一致")
    prefix_dims = tuple(d for d in dims if d is not order_dim)
    if len(prefix_dims) != len(dims) - 1:
        raise ValueError("order_dim 须在 dims 中")
    try:
        partition_key = tuple(project_prefix(region, prefix_dims))
    except Exception as e:
        raise ValueError(
            "region_to_blocks 要求 region 在 prefix_dims 上均为 Point，以便得到 partition_key"
        ) from e

    order_idx = dims.index(order_dim)
    order_size = shape[order_idx]
    try:
        c = region[order_dim]
    except (KeyError, TypeError):
        c = None

    if c is None:
        # 未约束：整维 → 所有块（0 到 ceil(order_size/block_tokens)）
        block_lo = 0
        block_hi = (order_size + block_tokens - 1) // block_tokens
    elif _has_value(c):
        # Point(v)
        v = int(c.value) if getattr(c, "value", None) is not None else 0
        block_lo = v // block_tokens
        block_hi = block_lo + 1
    elif _has_lo_hi(c):
        lo = int(c.lo)
        hi = int(c.hi)
        if hi > order_size:
            hi = order_size
        if lo >= hi:
            return []
        block_lo = lo // block_tokens
        block_hi = (hi + block_tokens - 1) // block_tokens
    else:
        return []

    return [
        BlockId(partition_key, k)
        for k in range(block_lo, block_hi)
    ]


def block_to_region(
    block_id: BlockId,
    dims: tuple[Dimension, ...],
    order_dim: Dimension,
    block_tokens: int,
    shape: tuple[int, ...],
) -> Region:
    """将 BlockId 转为对应的 TAA Region（该块在 order_dim 上的区间 + prefix 为 Point）。"""
    if len(dims) != len(shape):
        raise ValueError("dims 与 shape 长度须一致")
    prefix_dims = tuple(d for d in dims if d is not order_dim)
    if len(prefix_dims) != len(dims) - 1:
        raise ValueError("order_dim 须在 dims 中")
    if len(block_id.partition_key) != len(prefix_dims):
        raise ValueError(
            f"partition_key 长度 {len(block_id.partition_key)} 与 prefix_dims {len(prefix_dims)} 不一致"
        )

    order_idx = dims.index(order_dim)
    order_size = shape[order_idx]
    lo = block_id.block_id * block_tokens
    hi = min(lo + block_tokens, order_size)
    if lo >= hi:
        raise ValueError(f"block_id {block_id.block_id} 超出 order_dim 范围")

    constraints: dict[Dimension, Any] = {}
    for i, d in enumerate(prefix_dims):
        constraints[d] = Point(block_id.partition_key[i])
    constraints[order_dim] = Range(lo, hi)
    return Region(constraints)
