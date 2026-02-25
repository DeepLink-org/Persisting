"""BlockStore 作为 Pulsing Actor：暴露 get/put/prefetch/wait，供远程或本地代理调用。"""

from __future__ import annotations

from typing import Any

import numpy as np

from persisting.core import Dimension, Region, TensorView

from .block_store import BlockStore


def region_serialize(region: Region, dims: tuple[Dimension, ...]) -> list[tuple[str, str, Any]]:
    """将 Region 序列化为可跨进程/网络传递的列表：(dim_name, "point"|"range", value)。"""
    out = []
    for dim in dims:
        try:
            c = region[dim]
        except KeyError:
            out.append((dim.name, "all", None))
            continue
        if hasattr(c, "value") and not hasattr(c, "lo"):
            out.append((dim.name, "point", c.value))
        elif hasattr(c, "lo") and hasattr(c, "hi"):
            out.append((dim.name, "range", (int(c.lo), int(c.hi))))
        else:
            out.append((dim.name, "all", None))
    return out


def region_deserialize(
    region_ser: list[tuple[str, str, Any]],
    dims: tuple[Dimension, ...],
) -> Region:
    """从 region_ser 反序列化为 Region。"""
    view = TensorView(dims)
    key = []
    for (dim_name, typ, val), dim in zip(region_ser, dims):
        if typ == "point":
            key.append(val)
        elif typ == "range":
            key.append(slice(val[0], val[1]))
        else:
            key.append(Ellipsis)
    return view[tuple(key)]


def _dims_from_ser(dims_ser: list[tuple[str, str]]) -> tuple[Dimension, ...]:
    return tuple(Dimension(name, kind) for name, kind in dims_ser)


def _ensure_pulsing():
    try:
        import pulsing.core
        if not pulsing.core.is_initialized():
            raise RuntimeError("Pulsing 未初始化，请先 await pulsing.core.init()")
    except ImportError as e:
        raise ImportError("BlockStoreActor 需要安装 pulsing") from e


def _make_block_store_actor_class():
    _ensure_pulsing()
    from pulsing.core import remote

    @remote
    class BlockStoreActor:
        """Pulsing Actor：内部持有一个 BlockStore，暴露 get_region/put_region/prefetch_region/wait_region。"""

        def __init__(
            self,
            dims_ser: list[tuple[str, str]],
            shape: tuple[int, ...],
            order_dim_name: str,
            block_tokens: int,
            catalog_ser: dict[str, dict[Any, int]] | None = None,
        ):
            dims = _dims_from_ser(dims_ser)
            order_dim = next(d for d in dims if d.name == order_dim_name)
            self._dims = dims
            catalog = None
            if catalog_ser:
                catalog = {d: catalog_ser.get(d.name, {}) for d in dims}
            self._store = BlockStore(
                dims,
                shape,
                order_dim=order_dim,
                block_tokens=block_tokens,
                catalog=catalog,
            )

        def get_region(self, region_ser: list[tuple[str, str, Any]]):
            region = region_deserialize(region_ser, self._dims)
            return self._store.get(region)

        def put_region(self, region_ser: list[tuple[str, str, Any]], data: np.ndarray) -> None:
            region = region_deserialize(region_ser, self._dims)
            self._store.put(region, data)

        def prefetch_region(self, region_ser: list[tuple[str, str, Any]]) -> None:
            region = region_deserialize(region_ser, self._dims)
            self._store.prefetch(region)

        def wait_region(self, region_ser: list[tuple[str, str, Any]]) -> None:
            region = region_deserialize(region_ser, self._dims)
            self._store.wait(region)

    return BlockStoreActor


# 延迟绑定，避免 import 时未 init pulsing
BlockStoreActor = None

def get_block_store_actor_class():
    global BlockStoreActor
    if BlockStoreActor is None:
        BlockStoreActor = _make_block_store_actor_class()
    return BlockStoreActor


class ActorStore:
    """通过 Pulsing Actor 代理实现 Store：get/put 转发到远程 BlockStoreActor。"""

    __slots__ = ("_proxy", "_dims", "_view")

    def __init__(self, actor_proxy: Any, dims: tuple[Dimension, ...]):
        self._proxy = actor_proxy
        self._dims = tuple(dims)
        self._view = TensorView(self._dims)

    async def get_async(self, region: Region) -> np.ndarray:
        """异步 get，在 async 上下文中使用。"""
        region_ser = region_serialize(region, self._dims)
        return await self._proxy.get_region(region_ser)

    async def put_async(self, region: Region, value: np.ndarray) -> None:
        """异步 put，在 async 上下文中使用。"""
        region_ser = region_serialize(region, self._dims)
        await self._proxy.put_region(region_ser, value)

    def get(self, region: Region) -> np.ndarray:
        """同步 get；无运行中 event loop 时可用。"""
        import asyncio
        region_ser = region_serialize(region, self._dims)
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return asyncio.run(self._proxy.get_region(region_ser))
        raise RuntimeError("ActorStore.get 不能在已运行的 event loop 内调用，请使用 await store.get_async(region)")

    def put(self, region: Region, value: np.ndarray) -> None:
        """同步 put；无运行中 event loop 时可用。"""
        import asyncio
        region_ser = region_serialize(region, self._dims)
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            asyncio.run(self._proxy.put_region(region_ser, value))
            return
        raise RuntimeError("ActorStore.put 不能在已运行的 event loop 内调用，请使用 await store.put_async(region, value)")
