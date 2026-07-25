# TTAS Core Types

The Tiered Tensor Address Space (TTAS) types are available from `persisting.core`. These are the building blocks for multi-dimensional tensor addressing — used internally by `persisting.open()`, but also available directly for routing, planning, and optimization.

---

## `Dimension`

```python
from persisting.core import Dimension

d = Dimension(name: str, kind: str)
```

| `kind` | Values | Range support |
|--------|--------|---------------|
| `"int"` | Integer coordinates | ✅ |
| `"str"` | String coordinates (requires `catalog`) | ❌ |
| `"bytes"` | Bytes coordinates (requires `catalog`) | ❌ |

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
TIME    = Dimension("time", "int")
```

---

## `TensorView`

Construct regions via tensor-style subscript.

```python
from persisting.core import TensorView

tv = TensorView(dims)
region = tv["s1", 0, 0:512]                   # positional
region = tv[{SESSION: "s1"}, :, :, 0:512]      # dict
```

---

## `Region`

A conjunction of per-dimension constraints.

```python
from persisting.core import Region

r = Region({dim: constraint, ...})
```

### Constraints

```python
from persisting.core import Point, Range, SetC

Point(value)            # exact match
Range(lo, hi)           # half-open [lo, hi) — int only
SetC({v1, v2, ...})     # set membership
```

### Accessing Constraints

```python
r[dim]               → constraint or KeyError if unconstrained
dim in r             → bool
r.constraints_dict() → dict
```

---

## Operations

### `canonicalize(region)` → `Region`

Normalize constraints: merge same-dim constraints with `meet`, simplify singleton sets to points, sort by dimension name.

```python
from persisting.core import canonicalize

r = canonicalize(tv["s1", 0, 0:512])
```

### `project_prefix(region, dims)` → `tuple`

Extract partition key values. The region must have Point constraints on all requested dimensions.

```python
from persisting.core import project_prefix

key = project_prefix(region, (SESSION, LAYER, HEAD))
# → ("s1", 0, 2)
```

### `is_point_query(region, dim)` → `bool`

```python
from persisting.core import is_point_query

is_point_query(region, TIME)  # False (it's a range)
is_point_query(region, LAYER)  # True
```

### `is_range_query(region, dim)` → `bool`

```python
from persisting.core import is_range_query

is_range_query(region, TIME)  # True
```

---

## Block I/O (Rust)

Low-level block read/write from Rust:

```python
from persisting.core import block_read, block_write

data = block_read(path, offset, length)
block_write(path, offset, data)
```

---

## Mmap (Rust)

Unix-only. Available when compiled on Linux/macOS:

```python
from persisting.core import MmapRegion, mmap_reserve

region = mmap_reserve(length)        # → MmapRegion
region.copy_in(offset, data)         # write to mmap
data = region.copy_out(offset, len)  # read from mmap
region.base_address                  # → int (ptr)
```

---

## TieredLoop (Rust)

Background event loop for block prefetch and page fault handling:

```python
from persisting.core import TieredLoop

loop = TieredLoop()
loop.start()
loop.submit_prefetch(blocks)
loop.stop()
```

---

## UFFD / Page Fault Handler

Linux userfaultfd / macOS Mach exception handler:

```python
from persisting.core import start_uffd_handler

fd = start_uffd_handler(base_address, length, block_size, l3_file_path)
# Returns fd (Linux: uffd fd; macOS: shutdown pipe fd)
# Close fd to stop the handler
```

→ [BlockStore Internals](../design/block-store.md) for implementation details.
