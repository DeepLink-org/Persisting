"""单机存储：TTAS Region → CPU tensor；API 与 llms.binding.md 一致（open → kv[key] → h.tensor()/h.put）。"""

import pytest

np = pytest.importorskip("numpy")

try:
    import persisting
    from persisting.core import Dimension, TensorView
    from persisting.store import (
    Handler,
    LocalTensorStore,
    MmapBacking,
    SafetensorsBacking,
    TensorNamespace,
    region_to_index,
)
except ImportError as e:
    pytest.skip(f"persisting._core 或 store 不可用: {e}", allow_module_level=True)


LAYER = Dimension("layer", "int")
HEAD = Dimension("head", "int")
TIME = Dimension("time", "int")
DIMS = (LAYER, HEAD, TIME)
SHAPE = (2, 4, 512)


class TestRegionToIndex:
    """region_to_index 与 LocalTensorStore get/put。"""

    def test_point_slice(self):
        view = TensorView(DIMS)
        store = LocalTensorStore(DIMS, SHAPE, dtype=np.float32)
        r = view[0, 1, 10:20]
        idx = region_to_index(r, DIMS, SHAPE, None)
        assert idx[0] == 0
        assert idx[1] == 1
        assert idx[2] == slice(10, 20)

    def test_put_get_slice(self):
        view = TensorView(DIMS)
        store = LocalTensorStore(DIMS, SHAPE, dtype=np.float32)
        r = view[0, 1, 10:20]
        data = np.ones((10,), dtype=np.float32) * 42
        store.put(r, data)
        out = store.get(r)
        assert out.shape == (10,)
        np.testing.assert_array_almost_equal(out, 42)

    def test_put_get_point(self):
        view = TensorView(DIMS)
        store = LocalTensorStore(DIMS, SHAPE, dtype=np.float32)
        r = view[0, 1, 100]
        store.put(r, np.float32(7))
        out = store.get(r)
        assert out.shape == ()
        assert float(out) == 7.0

    def test_full_slice_one_dim(self):
        view = TensorView(DIMS)
        store = LocalTensorStore(DIMS, SHAPE, dtype=np.float32)
        # 仅指定 TIME 范围，LAYER/HEAD 不写表示全选
        r = view[{TIME: slice(0, 8)}]
        idx = region_to_index(r, DIMS, SHAPE, None)
        assert idx[0] == slice(None)
        assert idx[1] == slice(None)
        assert idx[2] == slice(0, 8)
        data = np.ones((2, 4, 8), dtype=np.float32)
        store.put(r, data)
        out = store.get(r)
        assert out.shape == (2, 4, 8)

    def test_str_dim_catalog(self):
        SESSION = Dimension("session", "str")
        TIME = Dimension("time", "int")
        dims = (SESSION, TIME)
        shape = (3, 100)
        catalog = {
            SESSION: {"s1": 0, "s2": 1, "s3": 2},
        }
        view = TensorView(dims)
        store = LocalTensorStore(dims, shape, dtype=np.float32, catalog=catalog)
        r = view["s1", 10:20]
        store.put(r, np.arange(10, dtype=np.float32))
        out = store.get(r)
        assert out.shape == (10,)
        np.testing.assert_array_almost_equal(out, np.arange(10, dtype=np.float32))


class TestBindingAPI:
    """与 llms.binding.md 一致：open → subscript → h.tensor() / h.put(data)。"""

    def test_open_kv_subscript_tensor_put(self):
        kv = persisting.open(
            "kvcache/v1",
            dims=DIMS,
            order_dim=TIME,
            partition_dims=(LAYER,),
            shape=SHAPE,
            dtype=np.float32,
        )
        h = kv[0, 1, 10:20]
        assert isinstance(h, Handler)
        h.put(np.ones((10,), dtype=np.float32) * 42)
        arr = h.tensor()
        assert arr.shape == (10,)
        np.testing.assert_array_almost_equal(arr, 42)

    def test_open_dict_style_subscript(self):
        kv = persisting.open("ns", dims=DIMS, shape=SHAPE)
        h = kv[{TIME: slice(0, 8)}]
        h.put(np.ones((2, 4, 8), dtype=np.float32))
        arr = h.tensor()
        assert arr.shape == (2, 4, 8)

    def test_open_colon_unconstrained(self):
        """kv["s1", :, :, 0:8] 中 : 表示未约束（llms.binding）。"""
        SESSION = Dimension("session", "str")
        dims = (SESSION, LAYER, HEAD, TIME)
        shape = (1, 2, 4, 512)
        catalog = {SESSION: {"s1": 0}}
        kv = persisting.open("ns", dims=dims, shape=shape, catalog=catalog)
        h = kv["s1", :, :, 0:8]
        h.put(np.ones((2, 4, 8), dtype=np.float32))
        arr = h.tensor()
        assert arr.shape == (2, 4, 8)

    def test_handler_immutable_slice(self):
        """每次 kv[key] 返回新 Handler，互不影响。"""
        kv = persisting.open("ns", dims=DIMS, shape=SHAPE)
        h1 = kv[0, 0, 0:5]
        h2 = kv[0, 0, 5:10]
        h1.put(np.arange(5, dtype=np.float32))
        h2.put(np.arange(5, 10, dtype=np.float32))
        np.testing.assert_array_almost_equal(h1.tensor(), np.arange(5, dtype=np.float32))
        np.testing.assert_array_almost_equal(h2.tensor(), np.arange(5, 10, dtype=np.float32))


class TestMmapBacking:
    """MmapBacking：文件 + 偏移量。"""

    def test_mmap_w_plus_put_get(self, tmp_path):
        """w+ 模式创建文件，put/get 往返。"""
        path = tmp_path / "tensor.bin"
        shape = (2, 8)
        backing = MmapBacking(path, shape, dtype=np.float32, offset=0, mode="w+")
        store = LocalTensorStore(DIMS[:2], shape, dtype=np.float32, backing=backing)
        view = TensorView(DIMS[:2])
        store.put(view[0, 2:6], np.arange(4, dtype=np.float32) * 10)
        out = store.get(view[0, 2:6])
        np.testing.assert_array_almost_equal(out, [0, 10, 20, 30])
        assert path.exists()
        backing.flush()

    def test_mmap_offset(self, tmp_path):
        """同一文件内用 offset 映射第二块区域。"""
        path = tmp_path / "multi.bin"
        shape0 = (4,)  # 4 floats
        shape1 = (4,)  # 第二块 4 floats
        itemsize = np.dtype(np.float32).itemsize
        offset1 = 4 * itemsize  # 第二块从 4*4=16 字节开始
        # 先建文件：两段
        with open(path, "wb") as f:
            f.seek(offset1 + 4 * itemsize - 1)
            f.write(b"\x00")
        back0 = MmapBacking(path, shape0, dtype=np.float32, offset=0, mode="r+")
        back1 = MmapBacking(path, shape1, dtype=np.float32, offset=offset1, mode="r+")
        back0._arr[:] = np.array([1.0, 2.0, 3.0, 4.0], dtype=np.float32)
        back1._arr[:] = np.array([10.0, 20.0, 30.0, 40.0], dtype=np.float32)
        back0.flush()
        back1.flush()
        # 读回
        L = Dimension("l", "int")
        store1 = LocalTensorStore((L,), shape1, dtype=np.float32, backing=back1)
        view = TensorView((L,))
        out = store1.get(view[0:4])
        np.testing.assert_array_almost_equal(out, [10, 20, 30, 40])


class TestSafetensorsBacking:
    """SafetensorsBacking：.safetensors 文件 + 命名 tensor（需 pip install safetensors）。"""

    def test_safetensors_backing_read(self, tmp_path):
        """写入 .safetensors 后用 SafetensorsBacking 打开并读。"""
        pytest.importorskip("safetensors")
        from safetensors.numpy import save_file

        path = tmp_path / "tensors.safetensors"
        save_file(
            {"block": np.arange(12, dtype=np.float32).reshape(3, 4)},
            path,
        )
        backing = SafetensorsBacking(path, "block")
        assert backing.shape == (3, 4)
        assert backing.dtype == np.float32
        store = LocalTensorStore(
            (LAYER, HEAD),
            (3, 4),
            dtype=np.float32,
            backing=backing,
        )
        view = TensorView((LAYER, HEAD))
        out = store.get(view[1, :])
        np.testing.assert_array_almost_equal(out, [4, 5, 6, 7])

    def test_safetensors_backing_import_error_without_lib(self):
        """未安装 safetensors 时构造应给出明确 ImportError。"""
        try:
            import safetensors  # noqa: F401
            pytest.skip("safetensors 已安装，跳过")
        except ImportError:
            pass
        with pytest.raises(ImportError, match="safetensors"):
            SafetensorsBacking("/nonexistent.safetensors", "x")
