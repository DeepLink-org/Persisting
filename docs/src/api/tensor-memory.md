# Tensor Memory API

## `persisting.open()`

Open a tensor namespace for multi-dimensional access.

```python
def open(
    namespace: str,
    dims: tuple[Dimension, ...],
    order_dim: Dimension | None = None,
    partition_dims: tuple[Dimension, ...] | None = None,
    *,
    backend: str = "local",
    shape: tuple[int, ...],
    dtype: np.dtype | type = np.float32,
    catalog: dict[Dimension, dict[Any, int]] | None = None,
    block_tokens: int = 64,
) -> TensorNamespace
```

| Argument | Required | Description |
|----------|----------|-------------|
| `namespace` | ✅ | Namespace name (e.g. `"kvcache/v1"`) |
| `dims` | ✅ | Tuple of `Dimension` in declaration order |
| `order_dim` | tiered | Dimension for range scans and block partitioning |
| `partition_dims` | ❌ | Dimensions for cross-node routing (planned) |
| `backend` | ❌ | `"local"` (flat array) or `"tiered"` (L1+L3 BlockStore) |
| `shape` | ✅ | Tuple of sizes, one per dimension |
| `dtype` | ❌ | numpy dtype (default `float32`) |
| `catalog` | ❌ | `{dim: {coord → index}}` for str/bytes dimensions |
| `block_tokens` | ❌ | Tokens per block on `order_dim` (default 64) |

Returns a `TensorNamespace`.

---

## `TensorNamespace`

```python
class TensorNamespace:
    def __getitem__(self, key) -> Handler
    def prefetch(self, key) -> None
    def wait(self, key) -> None
```

### `__getitem__(key)` → `Handler`

Create a slice handler. Returns a new Handler every call.

```python
h = kv["s1", 0, 0:512]                       # positional
h = kv[{SESSION: "s1"}, :, :, slice(0, 512)]  # dict + slice
```

### `prefetch(key)` → `None`

Asynchronously pull blocks covering this slice from L3 into L1 (tiered backend only).

```python
kv.prefetch(("s1", 0, 0:512))
```

### `wait(key)` → `None`

Block until prefetched blocks for this slice are in L1 (tiered backend only).

```python
kv.wait(("s1", 0, 0:512))
```

---

## `Handler`

```python
class Handler:
    def tensor(self) -> np.ndarray
    def put(self, data: np.ndarray) -> None
```

### `tensor()` → `ndarray`

Materialize data from storage. This is the only operation that copies data.

```python
arr = kv["s1", 0, 0:512].tensor()
```

### `put(data)` → `None`

Write data back to the slice's address.

```python
kv["s1", 0, 0:512].put(some_ndarray)
```

---

## Storage Backends

These are internal implementations available from `persisting.store`. Most users should use `persisting.open()`.

### `LocalTensorStore`

Single-node flat tensor store.

```python
store = LocalTensorStore(dims, shape, dtype=np.float32, catalog=None, backing=None)
store.get(region)    → ndarray
store.put(region, data)
```

### `BlockStore`

Two-tier block-granularity store.

```python
store = BlockStore(
    dims, shape, dtype=np.float32,
    order_dim=TIME, block_tokens=64,
    l1_backing=None,    # default: NumpyBacking
    l3_backing=None,    # default: NumpyBacking
)
store.get(region)      → ndarray
store.put(region, data)
store.prefetch(region)
store.wait(region)
```

### `ActorStore`

BlockStore exposed as a Pulsing Actor.

```python
from persisting.store import get_block_store_actor_class, ActorStore

actor_cls = get_block_store_actor_class()
actor = await actor_cls.spawn(dims_ser, shape, order_dim_name, block_tokens, catalog_ser)
store = ActorStore(actor.proxy, dims)
data = await store.get_async(region)
```

---

## Backing Types

Available from `persisting.store`:

| Backing | Description |
|---------|-------------|
| `NumpyBacking(shape, dtype)` | In-memory numpy array (default) |
| `MmapBacking(shape, path, dtype)` | Memory-mapped file |
| `SafetensorsBacking(shape, path, tensor_name, dtype)` | safetensors file |
| `BlockMappedBacking(shape, dtype, block_table, use_mmap)` | Block-granularity async, optional mmap |
| `RemoteBacking(shape, dtype, get_block, put_block)` | Remote via callback/RPC |
