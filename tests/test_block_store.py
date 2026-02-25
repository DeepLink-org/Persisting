"""Block 与 BlockStore：region_to_blocks、BlockStore get/put/prefetch/wait、open(backend="tiered")。"""

import pytest

try:
    from persisting.core import Dimension, Point, Range, Region, TensorView, mmap_reserve
except ImportError as e:
    pytest.skip(f"persisting._core 未安装: {e}", allow_module_level=True)

from persisting.store import (
    BlockId,
    BlockMappedBacking,
    BlockStore,
    block_to_region,
    region_to_blocks,
    RemoteBacking,
)
from persisting.store.local_tensor import MmapBacking, NumpyBacking


SESSION = Dimension("session", "str")
LAYER = Dimension("layer", "int")
HEAD = Dimension("head", "int")
TIME = Dimension("time", "int")
DIMS = (SESSION, LAYER, HEAD, TIME)
# shape: 2 session, 2 layer, 2 head, 256 time
SHAPE = (2, 2, 2, 256)
BLOCK_TOKENS = 64
CATALOG = {SESSION: {"s0": 0, "s1": 1}}


class TestBlockId:
    """BlockId 不可变、可哈希、相等性。"""

    def test_eq_and_hash(self):
        a = BlockId(("s0", 0, 0), 1)
        b = BlockId(("s0", 0, 0), 1)
        c = BlockId(("s0", 0, 0), 2)
        assert a == b
        assert a != c
        assert hash(a) == hash(b)
        assert hash(a) != hash(c)

    def test_in_set(self):
        s = {BlockId(("x",), 0), BlockId(("x",), 0)}
        assert len(s) == 1


class TestRegionToBlocks:
    def test_range_covers_multiple_blocks(self):
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:128]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert len(blocks) == 2
        assert blocks[0].partition_key == ("s0", 0, 0)
        assert blocks[0].block_id == 0
        assert blocks[1].block_id == 1

    def test_range_one_block(self):
        tv = TensorView(DIMS)
        r = tv["s1", 1, 1, 64:128]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert len(blocks) == 1
        assert blocks[0].partition_key == ("s1", 1, 1)
        assert blocks[0].block_id == 1

    def test_point_single_block(self):
        tv = TensorView(DIMS)
        r = tv["s0", 0, 1, 100]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert len(blocks) == 1
        assert blocks[0].block_id == 1  # 100 // 64

    def test_empty_range_returns_empty_list(self):
        """order_dim 范围超出 shape 被裁剪后 lo >= hi 时返回 []。"""
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 300:400]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert blocks == []

    def test_range_clamped_to_shape(self):
        """order_dim 范围超出 shape 时按 shape 裁剪块范围。"""
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 200:300]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        # TIME size=256, 200:256 只有 56 个，block_id 3 覆盖 192:256
        assert len(blocks) == 1
        assert blocks[0].block_id == 3

    def test_requires_point_on_prefix_dims(self):
        """prefix_dims 上有未约束维度时 project_prefix 会失败，应报错。"""
        tv = TensorView(DIMS)
        r_bad = tv["s0", slice(None), slice(None), 0:64]
        with pytest.raises((ValueError, KeyError, TypeError)):
            region_to_blocks(r_bad, DIMS, TIME, BLOCK_TOKENS, SHAPE)

    def test_dims_shape_length_mismatch_raises(self):
        with pytest.raises(ValueError, match="长度须一致"):
            tv = TensorView(DIMS)
            r = tv["s0", 0, 0, 0:64]
            region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, (2, 2, 256))


class TestBlockToRegion:
    def test_roundtrip(self):
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        blocks = region_to_blocks(r, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert len(blocks) == 1
        bid = blocks[0]
        back = block_to_region(bid, DIMS, TIME, BLOCK_TOKENS, SHAPE)
        assert back[TIME].lo == 0 and back[TIME].hi == 64
        assert back[SESSION].value == "s0"

    def test_block_id_out_of_range_raises(self):
        """block_id 过大导致 lo >= order_size 时应 ValueError。"""
        bid = BlockId(("s0", 0, 0), 100)
        with pytest.raises(ValueError, match="超出"):
            block_to_region(bid, DIMS, TIME, BLOCK_TOKENS, SHAPE)

    def test_partition_key_length_must_match_prefix_dims(self):
        """partition_key 长度须与 prefix_dims 一致。"""
        bid = BlockId(("s0",), 0)
        with pytest.raises(ValueError, match="长度"):
            block_to_region(bid, DIMS, TIME, BLOCK_TOKENS, SHAPE)


class TestBlockStore:
    def test_get_triggers_fetch_from_l3(self):
        import numpy as np
        l3 = NumpyBacking(SHAPE, dtype=np.float32)
        idx = (0, 0, 0, slice(0, 64))
        l3.write(idx, np.ones(64, dtype=np.float32) * 42)
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, 42.0)

    def test_put_then_get(self):
        import numpy as np
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        data = np.arange(64, dtype=np.float32)
        store.put(r, data)
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, data)

    def test_prefetch_and_wait(self):
        import numpy as np
        l3 = NumpyBacking(SHAPE, dtype=np.float32)
        l3.write((0, 0, 0, slice(0, 128)), np.ones(128, dtype=np.float32))
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:128]
        store.prefetch(r)
        store.wait(r)
        out = store.get(r)
        assert out.shape == (128,)

    def test_put_writes_through_to_l3(self):
        """put 写回 L3 后，另一 BlockStore 共享同一 L3 时能读到相同数据。"""
        import numpy as np
        import tempfile
        with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
            path = f.name
        try:
            l3 = MmapBacking(path, SHAPE, dtype=np.float32, mode="w+")
            store_a = BlockStore(
                DIMS,
                SHAPE,
                order_dim=TIME,
                block_tokens=BLOCK_TOKENS,
                catalog=CATALOG,
                l3_backing=l3,
            )
            tv = TensorView(DIMS)
            r = tv["s0", 0, 0, 0:64]
            data = np.arange(64, dtype=np.float32)
            store_a.put(r, data)
            store_b = BlockStore(
                DIMS,
                SHAPE,
                order_dim=TIME,
                block_tokens=BLOCK_TOKENS,
                catalog=CATALOG,
                l3_backing=l3,
            )
            out = store_b.get(r)
            np.testing.assert_array_almost_equal(out, data)
        finally:
            import os
            try:
                os.unlink(path)
            except OSError:
                pass


class TestBlockMappedBacking:
    """BlockMappedBacking 降级实现与接口。"""

    def test_block_table_not_implemented_raises(self):
        with pytest.raises(NotImplementedError, match="block_table"):
            BlockMappedBacking((2, 2, 64), block_tokens=64, block_table={})

    def test_degraded_read_write_same_as_numpy(self):
        import numpy as np
        backing = BlockMappedBacking((2, 2, 64), dtype=np.float32, block_tokens=64)
        idx = (0, 0, slice(0, 64))
        data = np.ones(64, dtype=np.float32) * 7
        backing.write(idx, data)
        out = backing.read(idx)
        np.testing.assert_array_almost_equal(out, data)

    @pytest.mark.skipif(mmap_reserve is None, reason="mmap_reserve 仅 Unix 可用")
    def test_use_mmap_read_write_same_as_numpy(self):
        """use_mmap=True 时 read/write 与降级实现结果一致。"""
        import numpy as np
        backing = BlockMappedBacking(
            (2, 2, 64), dtype=np.float32, block_tokens=64, use_mmap=True
        )
        idx = (0, 0, slice(0, 64))
        data = np.ones(64, dtype=np.float32) * 7
        backing.write(idx, data)
        out = backing.read(idx)
        np.testing.assert_array_almost_equal(out, data)


class TestBlockStoreWithBlockMappedBacking:
    """BlockStore 使用 BlockMappedBacking 作为 L1（降级实现）时，get/put 行为与 NumpyBacking 一致。"""

    def test_get_triggers_fetch_from_l3(self):
        import numpy as np
        l3 = NumpyBacking(SHAPE, dtype=np.float32)
        l3.write((0, 0, 0, slice(0, 64)), np.ones(64, dtype=np.float32) * 42)
        l1 = BlockMappedBacking(SHAPE, dtype=np.float32, block_tokens=BLOCK_TOKENS)
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l1_backing=l1,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, 42.0)

    def test_put_then_get(self):
        import numpy as np
        l1 = BlockMappedBacking(SHAPE, dtype=np.float32, block_tokens=BLOCK_TOKENS)
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l1_backing=l1,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        data = np.arange(64, dtype=np.float32)
        store.put(r, data)
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, data)

    def test_put_writes_through_to_l3(self):
        import numpy as np
        import os
        import tempfile
        with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
            path = f.name
        try:
            l3 = MmapBacking(path, SHAPE, dtype=np.float32, mode="w+")
            l1_a = BlockMappedBacking(SHAPE, dtype=np.float32, block_tokens=BLOCK_TOKENS)
            store_a = BlockStore(
                DIMS,
                SHAPE,
                order_dim=TIME,
                block_tokens=BLOCK_TOKENS,
                catalog=CATALOG,
                l1_backing=l1_a,
                l3_backing=l3,
            )
            tv = TensorView(DIMS)
            r = tv["s0", 0, 0, 0:64]
            data = np.arange(64, dtype=np.float32)
            store_a.put(r, data)
            store_b = BlockStore(
                DIMS,
                SHAPE,
                order_dim=TIME,
                block_tokens=BLOCK_TOKENS,
                catalog=CATALOG,
                l3_backing=l3,
            )
            out = store_b.get(r)
            np.testing.assert_array_almost_equal(out, data)
        finally:
            try:
                os.unlink(path)
            except OSError:
                pass

    @pytest.mark.skipif(mmap_reserve is None, reason="mmap_reserve 仅 Unix 可用")
    def test_store_with_mmap_l1_put_then_get(self):
        """L1=BlockMappedBacking(use_mmap=True) 时 put 再 get 与 NumpyBacking 一致。"""
        import numpy as np
        l1 = BlockMappedBacking(
            SHAPE, dtype=np.float32, block_tokens=BLOCK_TOKENS, use_mmap=True
        )
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l1_backing=l1,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        data = np.arange(64, dtype=np.float32)
        store.put(r, data)
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, data)


class TestPrefetchAsync:
    """预取异步化：prefetch 后台执行，wait 等待完成，get 未预取时同步拉取。"""

    def test_wait_waits_for_background_prefetch(self):
        """prefetch(region) 后 wait(region) 阻塞直到后台预取完成，再 get 得到 L3 数据。"""
        import numpy as np
        l3 = NumpyBacking(SHAPE, dtype=np.float32)
        l3.write((0, 0, 0, slice(0, 128)), np.ones(128, dtype=np.float32) * 99)
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:128]
        store.prefetch(r)
        store.wait(r)
        out = store.get(r)
        assert out.shape == (128,)
        np.testing.assert_array_almost_equal(out, 99.0)

    def test_get_without_prefetch_sync_fetches(self):
        """不调用 prefetch，直接 get(region) 仍能同步从 L3 拉取并返回正确数据。"""
        import numpy as np
        l3 = NumpyBacking(SHAPE, dtype=np.float32)
        l3.write((0, 0, 0, slice(0, 64)), np.arange(64, dtype=np.float32))
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, np.arange(64, dtype=np.float32))


class TestRustTieredLoop:
    """Rust 侧主事件循环（persisting.core.TieredLoop）：仅测 start / submit_prefetch / stop，填页为占位。"""

    def test_tiered_loop_start_submit_stop(self):
        """TieredLoop.start() 后 submit_prefetch(block_list) 再 stop() 不报错。"""
        from persisting.core import TieredLoop

        loop = TieredLoop()
        loop.start()
        # BlockId 形式：(partition_key, block_id)；partition_key 可为 tuple of int/str
        loop.submit_prefetch([((0, 0, 0), 0)])
        loop.submit_prefetch([(("s0", 0, 0), 1)])
        loop.stop()

    def test_tiered_loop_submit_without_start_raises(self):
        """未 start 时 submit_prefetch 应报错。"""
        from persisting.core import TieredLoop

        loop = TieredLoop()
        with pytest.raises(RuntimeError, match="未 start"):
            loop.submit_prefetch([((0, 0), 0)])


class TestBlockStoreWithRemoteBacking:
    """BlockStore L3=RemoteBacking（内存 stub）时 get/put 与本地 L3 行为一致。"""

    def test_get_triggers_fetch_from_remote_stub(self):
        """L3 为 RemoteBacking(stub) 时，get 从 stub 拉块到 L1。"""
        import numpy as np
        stub = {}
        idx_key = (0, 0, 0, (0, 64))
        stub[idx_key] = np.ones(64, dtype=np.float32) * 42
        l3 = RemoteBacking(
            SHAPE,
            dtype=np.float32,
            get_block=stub.__getitem__,
            put_block=stub.__setitem__,
        )
        store = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        out = store.get(r)
        assert out.shape == (64,)
        np.testing.assert_array_almost_equal(out, 42.0)

    def test_put_writes_through_to_remote_stub(self):
        """put 写回 RemoteBacking(stub)，另一 BlockStore 共享同一 stub 时能读到。"""
        import numpy as np
        stub = {}
        l3 = RemoteBacking(
            SHAPE,
            dtype=np.float32,
            get_block=stub.__getitem__,
            put_block=stub.__setitem__,
        )
        store_a = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        tv = TensorView(DIMS)
        r = tv["s0", 0, 0, 0:64]
        data = np.arange(64, dtype=np.float32)
        store_a.put(r, data)
        store_b = BlockStore(
            DIMS,
            SHAPE,
            order_dim=TIME,
            block_tokens=BLOCK_TOKENS,
            catalog=CATALOG,
            l3_backing=l3,
        )
        out = store_b.get(r)
        np.testing.assert_array_almost_equal(out, data)


class TestOpenTiered:
    def test_open_tiered_returns_namespace(self):
        import numpy as np
        import persisting
        from persisting.core import Dimension
        S = Dimension("session", "int")
        L = Dimension("layer", "int")
        T = Dimension("time", "int")
        dims = (S, L, T)
        shape = (2, 2, 128)
        kv = persisting.open(
            "test",
            dims,
            order_dim=T,
            backend="tiered",
            shape=shape,
            block_tokens=32,
        )
        h = kv[0, 0, 0:32]
        arr = h.tensor()
        assert arr.shape == (32,)
        h.put(np.zeros(32, dtype=np.float32))
