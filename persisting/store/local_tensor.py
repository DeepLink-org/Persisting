"""单机 tensor 存储：元数据（布局）与存储后端分离，便于接入 GPU / mmap 等。

- TensorLayout：纯元数据（dims, shape, dtype, catalog），Region → 索引，无存储。
- Backing：存储后端协议，read(indices)/write(indices, data)；可实现为 numpy、mmap、GPU 等。
- LocalTensorStore：组合 layout + backing，对外 get(region)/put(region, data)。

后续可增 Backing 实现示例：
- MmapBacking(path, shape, dtype)：np.memmap 映射磁盘文件；
- GpuBacking(shape, dtype, device)：GPU 显存，read 返回到 CPU 或 device 张量。
与 llms.binding.md 一致：open → subscript → h.tensor() / h.put(data)。
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Protocol

import numpy as np

from persisting.core import Dimension, Region, TensorView


def _norm_indices(indices: tuple[int | slice | np.ndarray, ...]) -> tuple[Any, ...]:
    """将索引元组规范为可哈希键：(slice 转为 (start, stop)，ndarray 转为 tuple)。"""
    out = []
    for x in indices:
        if isinstance(x, slice):
            out.append((x.start, x.stop))
        elif isinstance(x, np.ndarray):
            out.append(tuple(x.ravel().tolist()))
        else:
            out.append(x)
    return tuple(out)


def _indices_to_contiguous_byte_range(
    shape: tuple[int, ...],
    dtype: np.dtype | type,
    indices: tuple[int | slice | np.ndarray, ...],
):
    """若 indices 对应连续元素，返回 (start_byte, nbytes, result_shape)；否则 (None, None, result_shape)。"""
    dt = np.dtype(dtype)
    size = int(np.prod(shape))
    flat = np.arange(size, dtype=np.intp).reshape(shape)[indices]
    result_shape = flat.shape
    if flat.size == 0:
        return 0, 0, result_shape
    flat_ravel = np.sort(flat.ravel())
    lo, hi = int(flat_ravel[0]), int(flat_ravel[-1])
    if not np.all(flat_ravel == np.arange(lo, hi + 1, dtype=np.intp)):
        return None, None, result_shape
    return lo * dt.itemsize, (hi - lo + 1) * dt.itemsize, result_shape


class Store(Protocol):
    """Store 协议：get(region)/put(region, data)，供 Handler / TensorNamespace 使用。"""

    def get(self, region: Region) -> np.ndarray: ...
    def put(self, region: Region, value: np.ndarray) -> None: ...


def _constraint_to_index(
    constraint: Any,
    dim: Dimension,
    dim_size: int,
    catalog: dict[Dimension, dict[Any, int]] | None,
) -> int | slice | np.ndarray:
    """将单维 Constraint (Point/Range/SetC) 转为 numpy 索引。"""
    if constraint is None:
        return slice(None)

    # Point
    if hasattr(constraint, "value") and not hasattr(constraint, "lo") and not hasattr(constraint, "values"):
        v = constraint.value
        if dim.kind == "int":
            i = int(v)
        else:
            if catalog is None or dim not in catalog:
                raise ValueError(f"str/bytes 维度 {dim.name} 需要提供 catalog")
            i = catalog[dim].get(v)
            if i is None:
                raise KeyError(f"catalog 中无 {dim.name}={v!r}")
        if i < 0 or i >= dim_size:
            raise IndexError(f"{dim.name} 索引 {i} 超出 [0, {dim_size})")
        return i

    # Range
    if hasattr(constraint, "lo") and hasattr(constraint, "hi"):
        lo, hi = int(constraint.lo), int(constraint.hi)
        if lo < 0 or hi > dim_size:
            raise IndexError(f"{dim.name} 范围 [{lo}, {hi}) 超出 [0, {dim_size})")
        return slice(lo, hi)

    # SetC
    if hasattr(constraint, "values"):
        vals = list(constraint.values)
        if dim.kind == "int":
            indices = np.array([int(x) for x in vals], dtype=np.intp)
        else:
            if catalog is None or dim not in catalog:
                raise ValueError(f"str/bytes 维度 {dim.name} 需要提供 catalog")
            indices = np.array([catalog[dim][v] for v in vals], dtype=np.intp)
        if np.any(indices < 0) or np.any(indices >= dim_size):
            raise IndexError(f"{dim.name} 集合索引超出 [0, {dim_size})")
        return indices

    return slice(None)


def region_to_index(
    region: Region,
    dims: tuple[Dimension, ...],
    shape: tuple[int, ...],
    catalog: dict[Dimension, dict[Any, int]] | None = None,
) -> tuple[int | slice | np.ndarray, ...]:
    """将 TAA Region 转为 numpy 高级索引元组。

    Args:
        region: TAA Region（各维约束的合取）
        dims: 维度顺序，与 shape 一一对应
        shape: 各维大小
        catalog: 可选，str/bytes 维度到整数下标的映射，catalog[dim][coord] -> int

    Returns:
        可用于 arr[indices] 的索引元组
    """
    if len(dims) != len(shape):
        raise ValueError("dims 与 shape 长度须一致")
    indices = []
    for dim, dim_size in zip(dims, shape):
        try:
            c = region[dim]
        except KeyError:
            c = None
        idx = _constraint_to_index(c, dim, dim_size, catalog)
        indices.append(idx)
    return tuple(indices)


# ---------------------------------------------------------------------------
# TensorLayout：纯元数据，Region → 索引，无存储（便于后续 GPU / mmap 等只换 Backing）
# ---------------------------------------------------------------------------


class TensorLayout:
    """Tensor 元数据与索引逻辑，与具体存储分离。

    持有 dims、shape、dtype、catalog，提供 region_to_index(region)。
    不持有任何 buffer；实际存储由 Backing 提供。
    """

    __slots__ = ("_dims", "_shape", "_dtype", "_catalog")

    def __init__(
        self,
        dims: tuple[Dimension, ...],
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        catalog: dict[Dimension, dict[Any, int]] | None = None,
    ):
        if len(dims) != len(shape):
            raise ValueError("dims 与 shape 长度须一致")
        self._dims = tuple(dims)
        self._shape = tuple(shape)
        self._dtype = dtype
        self._catalog = catalog or {}

    @property
    def dims(self) -> tuple[Dimension, ...]:
        return self._dims

    @property
    def shape(self) -> tuple[int, ...]:
        return self._shape

    @property
    def dtype(self):
        return self._dtype

    @property
    def catalog(self) -> dict[Dimension, dict[Any, int]]:
        return self._catalog

    def region_to_index(self, region: Region) -> tuple[int | slice | np.ndarray, ...]:
        """TAA Region → numpy 风格索引元组（供 Backing.read/write 使用）。"""
        return region_to_index(region, self._dims, self._shape, self._catalog)


# ---------------------------------------------------------------------------
# Backing：存储后端协议，可换为 numpy / mmap / GPU 等
# ---------------------------------------------------------------------------


class Backing(Protocol):
    """存储后端：按索引读写，不关心 TAA。"""

    @property
    def shape(self) -> tuple[int, ...]: ...
    """逻辑 shape，与 TensorLayout.shape 一致。"""

    @property
    def dtype(self) -> np.dtype | type: ...

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        """按索引读出，返回拷贝。"""
        ...

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        """按索引写入。"""
        ...


class NumpyBacking:
    """单块 numpy 数组作为后端（内存）。"""

    __slots__ = ("_arr",)

    def __init__(self, shape: tuple[int, ...], dtype: np.dtype | type = np.float32):
        self._arr = np.zeros(shape, dtype=dtype)

    @property
    def shape(self) -> tuple[int, ...]:
        return self._arr.shape

    @property
    def dtype(self) -> np.dtype | type:
        return self._arr.dtype

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        return self._arr[indices].copy()

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        self._arr[indices] = data

    @property
    def numpy(self) -> np.ndarray:
        """底层数组（只读使用）。"""
        return self._arr


class MmapBacking:
    """文件 + 偏移量的内存映射后端（path + offset）。"""

    __slots__ = ("_path", "_offset", "_mode", "_arr")

    def __init__(
        self,
        path: str | Path,
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        offset: int = 0,
        mode: str = "r+",
    ):
        """
        Args:
            path: 文件路径
            shape: 逻辑 shape，与 TensorLayout 一致
            dtype: 元素类型
            offset: 文件内字节偏移，tensor 数据从该位置开始
            mode: 'r' 只读，'r+' 读写（文件须已存在且足够大），'w+' 创建/截断并读写
        """
        path = Path(path)
        self._path = path
        self._offset = offset
        self._mode = mode
        nbytes = int(np.prod(shape)) * np.dtype(dtype).itemsize
        if mode == "w+":
            path.parent.mkdir(parents=True, exist_ok=True)
            with open(path, "wb") as f:
                f.seek(offset + nbytes - 1)
                f.write(b"\x00")
        self._arr = np.memmap(path, dtype=dtype, mode=mode, offset=offset, shape=shape, order="C")

    @property
    def path(self) -> Path:
        return self._path

    @property
    def offset(self) -> int:
        return self._offset

    @property
    def shape(self) -> tuple[int, ...]:
        return self._arr.shape

    @property
    def dtype(self) -> np.dtype | type:
        return self._arr.dtype

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        return self._arr[indices].copy()

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        if self._mode == "r":
            raise ValueError("MmapBacking 为只读 (mode='r')")
        self._arr[indices] = data
        self._arr.flush()

    def flush(self) -> None:
        """将修改刷回磁盘。"""
        if hasattr(self._arr, "flush"):
            self._arr.flush()


class BlockMappedBacking:
    """将不连续 Block 映射为连续虚拟地址空间的 Backing。

    - use_mmap=False 或 mmap 不可用：内部 NumpyBacking，与普通 Backing 一致。
    - use_mmap=True 且 mmap_reserve 可用：用 Rust MmapRegion，read/write 经 copy_out/copy_in（仅支持连续索引）。
    - block_table 非 None 暂未实现。
    """

    __slots__ = ("_shape", "_dtype", "_inner", "_region")

    def __init__(
        self,
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        *,
        block_tokens: int = 64,
        block_table: dict | None = None,
        use_mmap: bool = False,
    ):
        if block_table is not None:
            raise NotImplementedError("block_table 暂未实现，请传 None 使用降级实现")
        self._shape = tuple(shape)
        self._dtype = dtype
        dt = np.dtype(dtype)
        total_bytes = int(np.prod(shape)) * dt.itemsize

        if use_mmap:
            try:
                from persisting.core import mmap_reserve
                if mmap_reserve is not None:
                    self._region = mmap_reserve(total_bytes)
                    self._inner = None
                    return
            except ImportError:
                pass
            self._region = None
            self._inner = NumpyBacking(shape, dtype=dtype)
        else:
            self._region = None
            self._inner = NumpyBacking(shape, dtype=dtype)

    @property
    def shape(self) -> tuple[int, ...]:
        return self._shape

    @property
    def dtype(self) -> np.dtype | type:
        return self._dtype

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        if self._inner is not None:
            return self._inner.read(indices)
        start_byte, nbytes, out_shape = _indices_to_contiguous_byte_range(
            self._shape, self._dtype, indices
        )
        if start_byte is None:
            raise NotImplementedError("BlockMappedBacking(mmap) 暂仅支持连续索引")
        data = self._region.copy_out(start_byte, nbytes)
        return np.frombuffer(data, dtype=self._dtype).reshape(out_shape).copy()

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        if self._inner is not None:
            self._inner.write(indices, data)
            return
        start_byte, nbytes, _ = _indices_to_contiguous_byte_range(
            self._shape, self._dtype, indices
        )
        if start_byte is None:
            raise NotImplementedError("BlockMappedBacking(mmap) 暂仅支持连续索引")
        self._region.copy_in(start_byte, np.asarray(data, dtype=self._dtype).tobytes())


class RemoteBacking:
    """按块委托到「远程」的 Backing：read/write 通过 get_block/put_block 调用（可 stub 为内存 dict，或 RPC）。

    用于 BlockStore 的 L3 服务化：L3 在远程节点时，用 RemoteBacking(get_block=..., put_block=...)，
    单测可用内存 dict 或本地 BlockStore 作为 stub。
    """

    __slots__ = ("_shape", "_dtype", "_get_block", "_put_block")

    def __init__(
        self,
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        *,
        get_block: Any,
        put_block: Any | None = None,
    ):
        self._shape = tuple(shape)
        self._dtype = dtype
        self._get_block = get_block
        self._put_block = put_block

    @property
    def shape(self) -> tuple[int, ...]:
        return self._shape

    @property
    def dtype(self) -> np.dtype | type:
        return self._dtype

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        key = _norm_indices(indices)
        return np.asarray(self._get_block(key), dtype=self._dtype).copy()

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        if self._put_block is None:
            raise ValueError("RemoteBacking 未提供 put_block，只读")
        key = _norm_indices(indices)
        self._put_block(key, np.asarray(data, dtype=self._dtype).copy())


class SafetensorsBacking:
    """基于 safetensors 库的 mmap backing：从 .safetensors 文件中映射指定命名 tensor。

    使用 safe_open(..., framework=\"np\") + get_tensor(name)，safetensors 内部使用 mmap 零拷贝。
    依赖可选：pip install safetensors。未安装时构造会报错。
    """

    __slots__ = ("_path", "_tensor_name", "_handle", "_arr")

    def __init__(
        self,
        path: str | Path,
        tensor_name: str,
    ):
        """
        Args:
            path: .safetensors 文件路径
            tensor_name: 文件中要映射的 tensor 键名
        """
        try:
            from safetensors import safe_open
        except ImportError as err:
            raise ImportError("SafetensorsBacking 需要安装 safetensors: pip install safetensors") from err

        path = Path(path)
        self._path = path
        self._tensor_name = tensor_name
        self._handle = safe_open(path, framework="np", device="cpu")
        self._arr = self._handle.get_tensor(tensor_name)

    @property
    def path(self) -> Path:
        return self._path

    @property
    def tensor_name(self) -> str:
        return self._tensor_name

    @property
    def shape(self) -> tuple[int, ...]:
        return tuple(self._arr.shape)

    @property
    def dtype(self) -> np.dtype | type:
        return self._arr.dtype

    def read(self, indices: tuple[int | slice | np.ndarray, ...]) -> np.ndarray:
        return self._arr[indices].copy()

    def write(self, indices: tuple[int | slice | np.ndarray, ...], data: np.ndarray) -> None:
        self._arr[indices] = data
        if hasattr(self._arr, "flush"):
            self._arr.flush()

    def flush(self) -> None:
        if hasattr(self._arr, "flush"):
            self._arr.flush()


# ---------------------------------------------------------------------------
# LocalTensorStore：layout + backing，对外 get(region)/put(region, data)
# ---------------------------------------------------------------------------


class LocalTensorStore:
    """Tensor 存储：元数据（TensorLayout）+ 存储后端（Backing），二者分离。

    便于日后替换 Backing 为 mmap、GPU 等，而不改 TAA/Region 逻辑。
    """

    __slots__ = ("_layout", "_backing")

    def __init__(
        self,
        dims: tuple[Dimension, ...],
        shape: tuple[int, ...],
        dtype: np.dtype | type = np.float32,
        catalog: dict[Dimension, dict[Any, int]] | None = None,
        backing: Backing | None = None,
    ):
        """
        Args:
            dims: 维度顺序，与 shape 一一对应
            shape: 各维大小
            dtype: 元素类型
            catalog: 可选，str/bytes 维度坐标 → 整数下标
            backing: 可选，存储后端；默认 NumpyBacking(shape, dtype)
        """
        self._layout = TensorLayout(dims, shape, dtype=dtype, catalog=catalog)
        self._backing = backing if backing is not None else NumpyBacking(shape, dtype=dtype)
        if self._backing.shape != shape or self._backing.dtype != dtype:
            raise ValueError("backing.shape/dtype 须与 layout 一致")

    @property
    def dims(self) -> tuple[Dimension, ...]:
        return self._layout.dims

    @property
    def shape(self) -> tuple[int, ...]:
        return self._layout.shape

    @property
    def layout(self) -> TensorLayout:
        """元数据层，可用于自定义 Backing 时复用。"""
        return self._layout

    @property
    def backing(self) -> Backing:
        """存储后端，可替换为 mmap / GPU 等实现。"""
        return self._backing

    @property
    def numpy(self) -> np.ndarray:
        """仅当 backing 为 NumpyBacking 时可用；否则 AttributeError。"""
        if isinstance(self._backing, NumpyBacking):
            return self._backing.numpy
        raise AttributeError("numpy 仅适用于 NumpyBacking，当前 backing 为 %s" % type(self._backing).__name__)

    def get(self, region: Region) -> np.ndarray:
        """按 TAA Region 读出，返回拷贝。"""
        idx = self._layout.region_to_index(region)
        return self._backing.read(idx)

    def put(self, region: Region, value: np.ndarray) -> None:
        """将 value 写入 Region 对应位置。"""
        idx = self._layout.region_to_index(region)
        self._backing.write(idx, value)


# ---------------------------------------------------------------------------
# Handler + TensorNamespace（与 llms.binding.md 一致）
# ---------------------------------------------------------------------------


class Handler:
    """切片句柄：表示「某 namespace 的一个下标切片」，不可变。

    - h.tensor()：物化，从分层存储读出数据（唯一产生拷贝的读）
    - h.put(data)：将数据写入该切片地址
    """

    __slots__ = ("_store", "_region")

    def __init__(self, store: Store, region: Region):
        self._store = store
        self._region = region

    def tensor(self) -> np.ndarray:
        """物化：从存储读出，返回 ndarray（拷贝）。"""
        return self._store.get(self._region)

    def put(self, data: np.ndarray) -> None:
        """将 data 写入该切片对应的地址。"""
        self._store.put(self._region, data)


class TensorNamespace:
    """open() 返回的 namespace 句柄：通过下标得到 Handler。"""

    __slots__ = ("_name", "_dims", "_store", "_view")

    def __init__(
        self,
        name: str,
        dims: tuple[Dimension, ...],
        store: Store,
    ):
        self._name = name
        self._dims = tuple(dims)
        self._store = store
        self._view = TensorView(self._dims)

    def __getitem__(self, key: Any) -> Handler:
        """kv[key] → Handler，无数据拷贝。"""
        region = self._view[key]
        return Handler(self._store, region)

    def prefetch(self, key: Any) -> None:
        """预取：若 store 支持则拉取该切片对应 Block 到 L1（tiered 时可用）。"""
        if hasattr(self._store, "prefetch"):
            self._store.prefetch(self._view[key])

    def wait(self, key: Any) -> None:
        """显式等待：若 store 支持则阻塞直到该切片数据就绪（tiered 时可用）。"""
        if hasattr(self._store, "wait"):
            self._store.wait(self._view[key])
