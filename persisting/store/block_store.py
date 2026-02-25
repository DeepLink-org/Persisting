"""BlockStore：单节点分层存储（Phase 1）。

- 以 Block 为粒度管理 L1 / L3，显式 miss 时从 L3 拉取到 L1。
- 提供 get/put/prefetch/wait，与 LocalTensorStore 同属「Store」语义，供 Handler 使用。
- 主事件循环必须在 Rust 侧（见设计 6.2.2，避免 GIL 死锁）；当前预取由 ThreadPoolExecutor 执行，后续可接入 persisting.core.TieredLoop。
"""

from __future__ import annotations

import threading
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import numpy as np

from persisting.core import Dimension, Region

from .block import BlockId, block_to_region, region_to_blocks
from .local_tensor import Backing, NumpyBacking, TensorLayout


class BlockStore:
    """单节点两层存储（L1 + L3）：按 Block 粒度跟踪，get 时缺块则从 L3 拷入 L1。"""

    __slots__ = (
        "_layout",
        "_l1",
        "_l3",
        "_dims",
        "_shape",
        "_order_dim",
        "_block_tokens",
        "_blocks_in_l1",
        "_lock",
        "_prefetch_executor",
        "_prefetch_pending",
        "_prefetch_counter",
    )

    def __init__(
        self,
        dims: tuple[Dimension, ...],
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        *,
        order_dim: Dimension,
        block_tokens: int,
        catalog: dict[Dimension, dict[Any, int]] | None = None,
        l1_backing: Backing | None = None,
        l3_backing: Backing | None = None,
    ):
        if len(dims) != len(shape) or order_dim not in dims:
            raise ValueError("dims/shape 须一致且 order_dim 在 dims 中")
        self._dims = tuple(dims)
        self._shape = tuple(shape)
        self._order_dim = order_dim
        self._block_tokens = int(block_tokens)
        self._layout = TensorLayout(self._dims, self._shape, dtype=dtype, catalog=catalog)
        self._l1 = l1_backing if l1_backing is not None else NumpyBacking(self._shape, dtype=dtype)
        self._l3 = l3_backing if l3_backing is not None else NumpyBacking(self._shape, dtype=dtype)
        if self._l1.shape != self._shape or self._l3.shape != self._shape:
            raise ValueError("L1/L3 backing.shape 须与 shape 一致")
        self._blocks_in_l1: set[BlockId] = set()
        self._lock = threading.Lock()
        self._prefetch_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="blockstore-prefetch")
        self._prefetch_pending: dict[int, tuple[frozenset[BlockId], threading.Event]] = {}
        self._prefetch_counter = 0

    @property
    def dims(self) -> tuple[Dimension, ...]:
        return self._dims

    @property
    def shape(self) -> tuple[int, ...]:
        return self._shape

    @property
    def layout(self) -> TensorLayout:
        return self._layout

    def _blocks_for_region(self, region: Region) -> list[BlockId]:
        return region_to_blocks(
            region,
            self._dims,
            self._order_dim,
            self._block_tokens,
            self._shape,
        )

    def _ensure_block_in_l1(self, block_id: BlockId) -> None:
        with self._lock:
            if block_id in self._blocks_in_l1:
                return
            block_region = block_to_region(
                block_id,
                self._dims,
                self._order_dim,
                self._block_tokens,
                self._shape,
            )
            idx = self._layout.region_to_index(block_region)
            data = self._l3.read(idx)
            self._l1.write(idx, data)
            self._blocks_in_l1.add(block_id)

    def get(self, region: Region) -> np.ndarray:
        """按 Region 读出：先保证覆盖该 Region 的 Block 均在 L1，再从 L1 读。未预取时同步拉取。"""
        for bid in self._blocks_for_region(region):
            self._ensure_block_in_l1(bid)
        idx = self._layout.region_to_index(region)
        return self._l1.read(idx)

    def put(self, region: Region, value: np.ndarray) -> None:
        """写入 Region：写入 L1 并写回 L3（write-through），标记对应 Block 已在 L1。"""
        idx = self._layout.region_to_index(region)
        self._l1.write(idx, value)
        self._l3.write(idx, value)
        with self._lock:
            for bid in self._blocks_for_region(region):
                self._blocks_in_l1.add(bid)

    def _prefetch_task(self, key: int, blocks: list[BlockId]) -> None:
        for bid in blocks:
            self._ensure_block_in_l1(bid)
        with self._lock:
            entry = self._prefetch_pending.pop(key, None)
        if entry is not None:
            entry[1].set()

    def prefetch(self, region: Region) -> None:
        """预取：异步将覆盖该 Region 的 Block 从 L3 拉取到 L1。wait(region) 可等待完成。"""
        blocks = self._blocks_for_region(region)
        with self._lock:
            blocks = [b for b in blocks if b not in self._blocks_in_l1]
            if not blocks:
                return
            self._prefetch_counter += 1
            key = self._prefetch_counter
            ev = threading.Event()
            self._prefetch_pending[key] = (frozenset(blocks), ev)
        self._prefetch_executor.submit(self._prefetch_task, key, blocks)

    def wait(self, region: Region) -> None:
        """等待该 Region 所需 Block 均在 L1（若已有后台预取则等待其完成，否则同步拉取）。"""
        blocks = set(self._blocks_for_region(region))
        while True:
            with self._lock:
                need = blocks - self._blocks_in_l1
                if not need:
                    return
                ev = None
                for _key, (pending_blocks, e) in list(self._prefetch_pending.items()):
                    if need <= pending_blocks:
                        ev = e
                        break
                if ev is None:
                    break
            ev.wait()
        for bid in need:
            self._ensure_block_in_l1(bid)
