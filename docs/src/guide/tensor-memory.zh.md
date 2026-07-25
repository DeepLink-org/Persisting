# 张量内存 (TTAS)

> **状态**: 🧪 实验性 — `local` 和 `tiered` 后端已可用。GPU 分层和跨节点分布规划中。

AI 数据不适合放进扁平的键值存储。KV Cache 块由 `(session, layer, head, time)` 定位。模型参数由 `(param_id, shard)` 定位。轨迹事件由 `(run_id, time)` 定位。这些都是**多维的**——但之前每种数据都被塞进各自的孤岛。

Persisting 用 **TTAS**（分层张量地址空间）解决这个问题。一套寻址模型，覆盖所有三类数据。一个存储引擎。跨 GPU、host 内存和 SSD 的统一分层策略。

---

## 核心思想

把 TTAS 想象成 numpy 的高级索引——但数据存在分层存储中，只有在你物化时才会流动到你手里。

```
   声明维度               打开命名空间                 下标切片
   ──────────            ──────────────               ──────────────
   session: str          kv = open(                   h = kv["s1", 0, 0:512]
   layer: int              "kvcache/v1",
   head:  int              dims=(sess, layer,         arr = h.tensor()
   time:  int              head, time),
                           backend="tiered",
                           shape=(100, 32, 8, 4096))
```

下标 `kv["s1", 0, 0:512]` 不移动任何数据。它创建一个 **Handler**——一个承诺，说"我要这些坐标上的数据"。只有当你调用 `.tensor()` 时，数据才真正从它所在的位置（host 内存、SSD、未来是 GPU 或远程节点）流入你的 numpy 数组。

这种**寻址与物化分离**的设计，使得分层成为可能。Handler 不知道数据在哪——存储引擎负责找到最快路径。

---

## 三类数据，一套接口

### 参数：命名的权重与分片

模型权重天然是多维的。一个 70B 模型，100 个命名参数，每个 8 路分片：

```python
import persisting
from persisting.core import Dimension

PARAM_ID = Dimension("param_id", "str")
SHARD    = Dimension("shard", "int")

ps = persisting.open("params/llama-70b",
    dims=(PARAM_ID, SHARD),
    backend="tiered",     # Block 粒度分层：host 内存 + SSD
    shape=(100, 8),
)

# 两个维度的点查——快速、可预测
embed_weight = ps["embed.weight", 0].tensor()
lm_head      = ps["lm_head.weight", 0].tensor()

# 写回（write-through：同时更新 host 缓存和 SSD 基线）
ps["embed.weight", 0].put(updated_tensor)
```

参数访问以点查为主——你总是确切知道需要哪个权重和分片。TTAS 在内部将其路由为批量 mget 操作。

### KV Cache：会话、层、头与时间

这是 TTAS 名称的由来。KV Cache 有一个**时间**维度，在生成过程中单调增长，而且你经常需要范围扫描（"给我这个 session/layer/head 的 token 0 到 512"）：

```python
SESSION = Dimension("session", "str")
LAYER   = Dimension("layer", "int")
HEAD    = Dimension("head", "int")
TIME    = Dimension("time", "int")

kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,           # 用来做范围扫描的维度
    backend="tiered",
    shape=(100, 32, 8, 4096),
    block_tokens=64,           # 把 TIME 切分成 64-token 的块
)

# 写入计算好的 KV block
kv["s1", 0, 2, 100].put(kv_tensor)

# 范围查询——"给我这个 session/layer/head 的所有 token 0-512"
h = kv["s1", 0, 2, 0:512]    # → Handler，还没移动数据

# 物化
arr = h.tensor()              # BlockStore 查 L1 缓存，miss 则走 SSD
```

`order_dim=TIME` 配合 `block_tokens=64` 告诉存储引擎："把这个维度切成 64 元素一块，并优化沿这个维度的范围扫描。"当你请求 `0:512` 时，引擎知道要读 8 个 block，从最快的可用层提供数据。

**预取**让你在实际需要之前就把 block 拉到 host 内存：

```python
# 在处理 token 0-512 的同时，预取 512-1024
kv.prefetch(("s1", 0, 2, 512:1024))

# ... 做计算 ...

# 阻塞直到预取完成，然后零 SSD 延迟读取
kv.wait(("s1", 0, 2, 512:1024))
next_chunk = kv["s1", 0, 2, 512:1024].tensor()
```

### 轨迹（规划中）

轨迹事件——Agent LLM 调用、工具输出、奖励——以 `(run_id, time)` 寻址。它们共用同一套 TTAS 模型。目前轨迹走 `persisting traj` CLI 和 Queue API；TTAS 原生的轨迹命名空间在路线图上，用于 RL 批量扫描场景。

---

## 分层到底怎么工作

当你调用 `h.tensor()` 时，BlockStore 对你范围内的每个 block 走一个简单的决策树：

```
这个 block 在 L1（host 内存）吗？
  ├── 是 → 直接读，~100ns 延迟
  └── 否 → 从 L3（SSD）拉到 L1，再读，~10μs 延迟
```

写入时，`h.put(data)` 使用 **write-through**：数据同时写入 L1 和 L3。这意味着每次写入都是持久化的——即使进程崩溃，SSD 上也有你的数据。

| 层 | 角色 | 状态 |
|----|------|------|
| L0 (GPU) | 活跃推理 block 热缓存 | 规划中 |
| L1 (Host) | 热缓冲，可 mmap | 已可用 |
| L3 (SSD) | 持久化基线，Lance 文件 | 已可用 |
| 远程节点 | 通过 Pulsing 跨节点共享 | 规划中 |

关键洞察：**L3 永远是数据真相的来源。** L1 和 L0 是加速。如果 block 被从 L1 驱逐，它仍然存在于 SSD 上。这就是分层透明的含义——你永远不用操心缓存驱逐导致的数据丢失。

---

## 选择后端

### `backend="local"` — 原型开发

小数据集或开发阶段，内存中的扁平 numpy 数组：

```python
kv = persisting.open("params/v1",
    dims=(PARAM_ID, SHARD),
    shape=(1000, 8),
    backend="local",
)
```

无分层，无 block。适合在扩展到 tiered 之前测试维度布局和下标模式。

### `backend="tiered"` — 生产环境

Block 粒度分层存储。比 `local` 多约 10 行代码：

```python
kv = persisting.open("kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,           # ← 新增：哪个维度被切块
    backend="tiered",         # ← 新增：启用分层
    shape=(100, 32, 8, 4096),
    block_tokens=64,           # ← 新增：块大小
)
```

---

## 定义维度

维度是命名空间的 schema。每个维度有名字和类型：

```python
from persisting.core import Dimension

SESSION = Dimension("session", "str")   # 字符串坐标
LAYER   = Dimension("layer", "int")     # 整数坐标
TIME    = Dimension("time", "int")      # 整数，支持范围查询
```

| 类型 | 范围查询 | 需要 catalog |
|------|:---:|:---:|
| `"int"` | ✅ | 否 |
| `"str"` | ❌ | 是（字符串 → 整数下标） |
| `"bytes"` | ❌ | 是 |

字符串维度需提供 catalog，将坐标名映射到整数下标：

```python
catalog = {SESSION: {"s1": 0, "s2": 1, "s3": 2}}
kv = persisting.open("kvcache/v1", dims=(SESSION, TIME), shape=(3, 512), catalog=catalog)
```

---

## 下标速查

| 形式 | 示例 | 含义 |
|------|------|------|
| 值 | `"s1"`, `0` | 此维上的精确匹配 |
| 冒号 | `:` | 全部值（不约束） |
| 范围 | `0:512` | 半开区间 `[0, 512)`——仅 int |

两种等价写法：

```python
kv["s1", 0, 2, 0:512]                        # 位置式
kv[{SESSION: "s1"}, :, :, slice(0, 512)]      # dict + slice 对象
kv[{SESSION: "s1"}, :, :, 0:512]              # dict，范围简写
```

---

## 自定义存储后端

用 mmap、safetensors 或远程后端替换默认的 numpy 数组：

```python
from persisting.store import LocalTensorStore, MmapBacking, SafetensorsBacking

# 内存映射文件——跨进程共享
store = LocalTensorStore(DIMS, SHAPE, backing=MmapBacking(SHAPE, path="/data/cache.bin"))

# safetensors——直接加载模型权重
store = LocalTensorStore(DIMS, SHAPE, backing=SafetensorsBacking(SHAPE, path="/data/params.safetensors"))
```

分层模式下可以独立替换 L1 和 L3：

```python
from persisting.store import BlockStore, MmapBacking, RemoteBacking

store = BlockStore(
    DIMS, SHAPE,
    order_dim=TIME, block_tokens=64,
    l1_backing=MmapBacking(SHAPE, path="/fast-nvme/l1.bin"),
    l3_backing=RemoteBacking(SHAPE, get_block=fetch_from_remote, put_block=push_to_remote),
)
```

这就是将远程 SSD 或对象存储接入 L3 基线的方式。

---

## 底层原理

对于大多数用户，`kv[key].tensor()` 和 `kv[key].put(data)` 是全部接口。如果要做路由、规划或批量优化，底层的 **Region** 抽象也可直接使用：

```python
from persisting.core import TensorView, canonicalize, is_point_query, is_range_query

tv = TensorView(DIMS)
region = tv["s1", :, :, 0:100]
region = canonicalize(region)              # 规范化约束
key = region.project_prefix([SESSION])     # → ("s1",) 用于路由
assert is_range_query(region, TIME)        # → True
```

→ [TTAS 设计文档](../design/tensor-address-space.md) 了解完整形式化模型。

---

## 后续计划

- **GPU 分层 (L0)** — CUDA 虚拟内存映射，用于热 KV cache block
- **跨节点 TTAS** — Pulsing 路由 + RDMA 数据面
- **轨迹命名空间** — `persisting.open("trajectories/v1", dims=(RUN_ID, TIME))` 用于 RL 批量扫描
- **`persisting.pin()`** — 显式 pin/unpin 驱逐控制（已有规格，等待实现）

---

## 下一步

- [API 参考 — Tensor Memory](../api/tensor-memory.md) — 所有方法签名
- [TTAS 设计文档](../design/tensor-address-space.md) — 形式化寻址模型
- [分布式分层存储](../design/distributed-tiered-storage.md) — block 模型、mmap+UFFD、跨节点
