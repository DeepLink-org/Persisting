"""BlockStore 作为 Pulsing Actor：spawn、get_region/put_region、ActorStore。"""

import pytest

try:
    from persisting.core import Dimension, TensorView
except ImportError as e:
    pytest.skip(f"persisting._core 未安装: {e}", allow_module_level=True)

from persisting.store import (
    get_block_store_actor_class,
    region_serialize,
    region_deserialize,
    ActorStore,
)

SESSION = Dimension("session", "str")
LAYER = Dimension("layer", "int")
HEAD = Dimension("head", "int")
TIME = Dimension("time", "int")
DIMS = (SESSION, LAYER, HEAD, TIME)
SHAPE = (2, 2, 2, 256)
BLOCK_TOKENS = 64
CATALOG = {SESSION: {"s0": 0, "s1": 1}}
CATALOG_SER = {"session": {"s0": 0, "s1": 1}}
DIMS_SER = [(d.name, d.kind) for d in DIMS]


def _pulsing_available():
    try:
        import pulsing.core
        return True
    except ImportError:
        return False


@pytest.mark.asyncio
@pytest.mark.skipif(not _pulsing_available(), reason="pulsing 未安装")
async def test_block_store_actor_put_then_get():
    """spawn BlockStoreActor，put_region 后 get_region 返回相同数据。"""
    import pulsing.core
    await pulsing.core.init()
    try:
        BlockStoreActor = get_block_store_actor_class()
        proxy = await BlockStoreActor.spawn(
            dims_ser=DIMS_SER,
            shape=SHAPE,
            order_dim_name="time",
            block_tokens=BLOCK_TOKENS,
            catalog_ser=CATALOG_SER,
        )
        try:
            import numpy as np
            view = TensorView(DIMS)
            r = view["s0", 0, 0, 0:64]
            region_ser = region_serialize(r, DIMS)
            data = np.arange(64, dtype=np.float32)
            await proxy.put_region(region_ser, data)
            out = await proxy.get_region(region_ser)
            assert out.shape == (64,)
            np.testing.assert_array_almost_equal(out, data)
        finally:
            pass  # actor 随进程退出
    finally:
        await pulsing.core.shutdown()


@pytest.mark.asyncio
@pytest.mark.skipif(not _pulsing_available(), reason="pulsing 未安装")
async def test_actor_store_get_async_put_async():
    """ActorStore(proxy, dims) 的 get_async/put_async 与直接调 actor 一致。"""
    import pulsing.core
    await pulsing.core.init()
    try:
        BlockStoreActor = get_block_store_actor_class()
        proxy = await BlockStoreActor.spawn(
            dims_ser=DIMS_SER,
            shape=SHAPE,
            order_dim_name="time",
            block_tokens=BLOCK_TOKENS,
            catalog_ser=CATALOG_SER,
        )
        store = ActorStore(proxy, DIMS)
        view = TensorView(DIMS)
        r = view["s0", 0, 0, 0:64]
        import numpy as np
        data = np.ones(64, dtype=np.float32) * 7
        await store.put_async(r, data)
        out = await store.get_async(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, data)
    finally:
        await pulsing.core.shutdown()


def test_region_serialize_deserialize_roundtrip():
    """region_serialize 再 region_deserialize 与原始 Region 等价（同一下标）。"""
    view = TensorView(DIMS)
    r = view["s0", 0, 0, 0:64]
    ser = region_serialize(r, DIMS)
    r2 = region_deserialize(ser, DIMS)
    # 用 BlockStore 验证：同一 region 应得到同一 block 列表
    from persisting.store import region_to_blocks
    blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
    blocks2 = region_to_blocks(r2, DIMS, TIME, BLOCK_TOKENS, SHAPE)
    assert blocks == blocks2
    assert len(ser) == len(DIMS)
    assert ser[0] == ("session", "point", "s0")
    assert ser[3] == ("time", "range", (0, 64))
