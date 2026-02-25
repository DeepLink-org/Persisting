"""Tiered Tensor Address Space (TTAS) — Rust implementation.

Re-exports from the native module ``persisting._core`` (built with maturin).
轻量访问：用 TensorView(dims) 得到 tensor，再 tensor[{SESSION: xx}, :, 0:100] 或 tensor["s1", :, 0:100] 得到 Region。
"""

from persisting._core import (
    Dimension,
    Point,
    Range,
    SetC,
    Address,
    Region,
    canonicalize,
    project_prefix,
    is_point_query,
    is_range_query,
    block_read,
    block_write,
    TieredLoop,
)

try:
    from persisting._core import MmapRegion, mmap_reserve
except ImportError:
    MmapRegion = None  # type: ignore[misc, assignment]
    mmap_reserve = None  # type: ignore[misc, assignment]

try:
    from persisting._core import start_uffd_handler
except ImportError:
    start_uffd_handler = None  # type: ignore[misc, assignment]  # Linux: uffd fd；macOS: Mach handler shutdown fd


def _item_to_constraint(item, dim):
    if item is None or item is Ellipsis:
        return None
    if isinstance(item, slice):
        if item.start is None and item.stop is None and (item.step is None or item.step == 1):
            return None  # : 表示未约束（与 llms.binding 一致）
        if item.step not in (None, 1):
            raise ValueError("slice step 必须为 1 或省略")
        if item.stop is None:
            raise ValueError("Range 需指定上界，不能用 slice(lo, None)")
        return Range(item.start if item.start is not None else 0, item.stop)
    return Point(item)


class TensorView:
    """
    按固定维度顺序的 tensor 式下标，得到 Region。

    用法::
        dims = (SESSION, LAYER, TIME)
        tensor = TensorView(dims)
        r = tensor["s1", :, 0:100]           # SESSION=Point("s1"), LAYER 不限, TIME=Range(0,100)
        r = tensor[{SESSION: "s1"}, :, 0:100]  # 等价
    """

    __slots__ = ("_dims",)

    def __init__(self, dims):
        self._dims = tuple(dims)

    def __getitem__(self, key):
        dims = self._dims
        if isinstance(key, dict):
            # 纯 dict：按维度名，缺失维为 :
            constraints = {}
            for d in dims:
                if d in key:
                    c = _item_to_constraint(key[d], d)
                    if c is not None:
                        constraints[d] = c
            return Region(constraints)

        if not isinstance(key, tuple):
            key = (key,)
        if len(key) != len(dims):
            raise ValueError(f"下标长度须为 {len(dims)}，得到 {len(key)}")
        constraints = {}
        for i, d in enumerate(dims):
            item = key[i]
            if isinstance(item, dict):
                item = item.get(d, Ellipsis)
            c = _item_to_constraint(item, d)
            if c is not None:
                constraints[d] = c
        return Region(constraints)


__all__ = [
    "Dimension",
    "Point",
    "Range",
    "SetC",
    "Address",
    "Region",
    "TensorView",
    "canonicalize",
    "project_prefix",
    "is_point_query",
    "is_range_query",
    "block_read",
    "block_write",
    "MmapRegion",
    "mmap_reserve",
]
