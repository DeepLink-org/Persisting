# 分层张量地址空间（TTAS）—— Persisting 分布式分层内存的寻址模型

**版本**：2.0（重写）  
**状态**：设计冻结前审阅版  
**目标**：为 Persisting 的分布式分层内存（GPU ↔ host ↔ SSD，跨节点）提供寻址模型——多维地址、约束代数、规范化与 lowering。

为与 **PGAS**（Partitioned Global Address Space）对标的说法，我们将 Persisting 的寻址模型称为 **分层张量地址空间（Tiered Tensor Address Space，TTAS）**。

---

## 0. TTAS 在 Persisting 中的位置

### 0.1 Pulsing + Persisting 的分工

- **Pulsing**：分布式 actor 运行时（控制面）——actor 发现、消息传递、生命周期、集群管理。
- **Persisting**：为 Pulsing 扩展**内存语义**（数据面）——多维地址空间 + GPU/host/SSD 分层 + 基于 Pulsing 的跨节点分布。

TTAS 是 Persisting 数据面的**寻址模型**：定义"数据在多维地址空间中如何定位"，驱动分层 placement、跨节点路由和批量优化。

### 0.2 与 PGAS 的对比：TTAS 与 PGAS 对标

PGAS（UPC、Chapel）为 HPC 提供了**按 rank 分区的全局地址空间**，但受限于：单一扩展维（rank）、线性地址、shmem API、无分层。AI workload 需要不同的地址空间形态：

| | PGAS | TTAS（Persisting） |
|-|------|-------------------|
| **全称** | Partitioned Global **Address Space** | **T**iered **T**ensor **A**ddress **S**pace |
| **扩展维度** | 1 个（rank） | 任意多个（session, layer, head, time, shard, ...） |
| **地址形态** | 线性（rank 内一维偏移） | 多维（各维独立，可点/范围/集合） |
| **访问方式** | shmem put/get | tensor 下标：`kv["s1", 0, 2, 0:512]` |
| **分层** | 无（扁平内存） | GPU ↔ host ↔ SSD，策略驱动 |
| **分布式** | 语言运行时 | Pulsing actor 运行时 |
| **放置** | rank → 进程 | ρ(π_D(a)) → 节点/设备/层，由投影+分区键显式给出 |

PGAS 在 HPC 失败的教训：仅靠"更好的地址模型"不能推动帕累托曲线。Persisting 从中吸取的经验：**TTAS 是内部寻址基础设施，对外不卖"地址空间"概念——价值用 KV Cache P99、GPU 利用率、内存效率来衡量。**

### 0.3 TTAS 解决什么

TTAS 为 Persisting 的三类内存场景提供统一寻址：

- **KV Cache**：`(session, layer, head, time)` — 点查 / 单维范围 / 滑窗预取
- **PS / Optimizer state**：`(param_id, shard)` — 批量点查
- **轨迹**：`(run_id, time)` — 单维范围扫描

并通过**确定性规范化**优化数据面操作：

- **分区裁剪**：从请求中提取 partition key，路由到正确的节点/层
- **批量合并**：同 shard 的点查合并为 `mget`；同前缀的范围合并
- **预取生成**：对有序维度的滑窗访问生成预取序列

### 0.4 TTAS 不做什么

- 通用查询优化器 / 执行引擎 / Region 集合运算闭包
- 物理连续字节偏移假设
- 分层 placement policy 的在线决策（由 Persisting 的 tiering 层负责，TTAS 只提供地址信息）

用户看到的是 `kv["s1", 0, 2, 0:512].tensor()`。TTAS 在底层驱动寻址和路由，用户不需要直接操作。

---

## 1. 关键约束：可实现的 Key Schema 假设

本设计不依赖物理连续布局，但依赖 **key 的字典序（lexicographic order）**，以支持范围扫描。

对每一种数据集合（可理解为一个“表/命名空间”，例如 `kvcache/v1`），必须显式声明：

- **维度全集**：`Dims = (d0, d1, ..., dn)`
- **排序维度**：`order_dim ∈ Dims`（可选；没有则不支持 range_scan）
- **key 排序顺序**：`KeyOrder = (prefix_dims..., order_dim)`，其中 `prefix_dims = Dims \ {order_dim}`

**硬性要求**：

- 若声明了 `order_dim`，则它必须位于 `KeyOrder` 的最后一位。  
  否则“固定前缀 + order_dim 范围”无法保证落在一个连续的 key 区间。

**直观解释**：

- 你可以把 key 看成复合 tuple。只要 `order_dim` 是最后一列，那么对“其余列全等 + 最后一列范围”就能用一次 range_scan 表达。

---

## 2. 数据模型：Address / Constraint / Region（收敛到可证明子语言）

### 2.1 Dimension（维度）

维度只承担两件事：**命名**与**类型/值域约束**。不要引入单例缓存、动态扩展域等隐性复杂度。

```python
@dataclass(frozen=True)
class Dimension:
    name: str
    # 仅用于校验与编码；不把 Domain 做成可变/动态扩展机制
    kind: Literal["int", "str", "bytes"]
```

约束：

- `name` 在一个集合内唯一
- `kind` 决定编码方式（以及是否能做 Range）

### 2.2 Address（点地址）

**Address 是一个“完全指定的点”**。它必须对集合声明的所有维度都给出值（不允许 ⊥ / wildcard）。

```python
@dataclass(frozen=True)
class Address:
    values: Mapping[Dimension, Any]  # 必须覆盖 Dims 全集
```

约束：

- `Address.values` 的 key 必须覆盖该集合的 `Dims`
- 每个维度恰好一个值

### 2.3 Constraint（单维约束）

只支持三种（足够覆盖我们目标 workload）：

- **Point**：`d = v`
- **Range**：`lo ≤ d < hi`（仅 `int` 维度支持）
- **Set**：`d ∈ {v1, v2, ...}`（用于小集合；不承诺可无限扩展）

```python
@dataclass(frozen=True)
class Point:
    value: Any

@dataclass(frozen=True)
class Range:
    lo: int
    hi: int  # half-open [lo, hi)

@dataclass(frozen=True)
class SetC:
    values: frozenset[Any]
```

### 2.4 Region（合取约束区域）

Region 是 **“一组维度约束的合取（AND）”**，没有表达式树，没有 union/intersection。

```python
@dataclass(frozen=True)
class Region:
    constraints: Mapping[Dimension, Point | Range | SetC]
```

语义（集合论）：

\[
\llbracket R \rrbracket \;=\;\{ a \in Address \mid \forall d \in Dims,\; a(d) \text{ 满足 } R.constraints[d]\}
\]

**注意**：

- Region 允许“缺维度约束”，表示该维度不受限（但 lowering 会因此拒绝某些模式；见第 4 章）
- Region 不保证可枚举。只有当所有约束都可有限展开且维度数很小、且调用方明确需要时才允许 enumerate（否则禁用）

---

## 3. 核心操作：只保留能产生确定性优化的那部分

### 3.1 Projection（投影）——只用于路由/分组

投影不是“查询算子”，它的用途是从请求中抽取 partition key 进行分组/路由。

```python
def project_prefix(addr_or_region: Address | Region,
                   dims: tuple[Dimension, ...]) -> tuple[Any, ...]:
    """
    返回按 dims 顺序的值/约束摘要（用于分组/路由）。
    若是 Region，则 dims 中必须都是 Point 约束，否则无法形成确定的路由键。
    """
```

关键限制：

- 对 Region 做 `project_prefix` 时，只接受被投影维度都为 `Point` 的情况（否则无法路由到确定 shard）

### 3.2 Selection（切片）——唯一的“查询构造”操作

Selection 的工程意义是：构造/更新某维的约束，并在构造时做“同维合并/判空”。

```python
def select(r: Region, d: Dimension, c: Point | Range | SetC) -> Region:
    """
    r ∧ (d 满足 c)
    """
```

### 3.3 Offset（偏移）——限定为“order_dim 上的 range 平移”（用于预取）

Offset 不作为通用代数闭包运算。它只服务一个用途：生成预取窗口。

```python
def shift_range(r: Region, order_dim: Dimension, delta: int) -> Region:
    """
    仅当 r 在 order_dim 上是 Range 时，执行 [lo,hi) -> [lo+delta, hi+delta)
    其余约束保持不变。若越界则判空或裁剪（由集合配置决定）。
    """
```

---

## 4. 规范化与重写：确定性、可证明、可收敛

本设计的“优化”不做计划搜索，只做 **规范化（canonicalization）**：

- 目标：把 Region 规范化成少数“可 lowering 的 Normal Form”
- 结果：缓存命中更稳定、批处理合并更容易、lowering 更简单

### 4.1 约束交（Constraint meet）——必须实现但范围很小

定义 `meet(c1, c2)` 返回“同时满足”的最强约束，或 `Empty` 表示矛盾。

规则（只列必要的）：

- `meet(Point(v), Point(v)) = Point(v)`；`meet(Point(v), Point(u)) = Empty`（v≠u）
- `meet(Point(v), Range(lo,hi)) = Point(v)` 若 lo≤v<hi，否则 Empty
- `meet(Range(a,b), Range(c,d)) = Range(max(a,c), min(b,d))` 若非空，否则 Empty
- `meet(Set(S), Set(T)) = Set(S∩T)`；空集则 Empty
- `meet(Set(S), Point(v))` 同理；`meet(Set(S), Range)` 可选（实现为过滤出落在 range 内的元素；若集合过大则拒绝）

### 4.2 规范化（Canonicalization）流程

规范化是一个**确定性**过程，不做搜索、不做代价模型。输入 `Region`，输出：

- `Empty`：无解
- 或一个“规范 Region”（约束合并完成、表示稳定）

规范化步骤：

1. **同维合并**：对同一维度的多次约束（来自多次 `select`）用 `meet` 合并；若为 `Empty` 则整体判空。
2. **约束简化**：
   - `SetC({v})` 归一化为 `Point(v)`
   - `Range(lo, hi)` 若 `lo >= hi` 判空
3. **确定性排序**：把 `constraints` 按维度名排序，作为 canonical 表示的一部分（用于缓存 key、去重、哈希）。

设计约束（避免爆炸）：

- `SetC` 只用于**小集合**，并应由上层传入明确的上限（例如 `max_set_cardinality <= 64`）。超过上限应直接拒绝进入 lowering，而不是尝试“聪明”处理。

### 4.3 可 lowering 的 Normal Form（NF）

为保证 lowering 简单且可证明，我们只承认两种可执行形态：

- **NF-PointQuery**：所有维度都是 `Point`
- **NF-RangeQuery**：存在 `order_dim`，且：
  - `order_dim` 是 `Range(lo,hi)`
  - 其余所有维度都是 `Point`

非 NF 的 Region 仍然有数学语义，但 v1 **不保证可执行**；必须在上层拆解/限制后再提交。

---

## 5. Lowering：从 Region 到后端原语（固定模式匹配）

Lowering 的目标：把 NF 转成少量后端调用，不引入“查询引擎”。

### 5.1 后端原语（最小集合）

每个数据集合（namespace）必须实现：

```python
class Backend(Protocol):
    def mget(self, keys: list[bytes]) -> list[bytes | None]:
        """批量点查；缺失返回 None。"""

    def put_batch(self, items: list[tuple[bytes, bytes]]) -> None:
        """批量写入（幂等性/覆盖语义见 6.3）。"""

    def delete_batch(self, keys: list[bytes]) -> None:
        """批量删除。"""

    def range_scan(self, prefix: bytes, lo: int, hi: int) -> Iterator[tuple[bytes, bytes]]:
        """
        单维范围扫描：只对 key 的最后一个分量（order_dim）做 [lo,hi) 范围。
        返回 (key,value) 流，调用方负责 decode 与过滤（通常不需要额外过滤）。
        """

    def prefetch(self, keys: list[bytes]) -> None:
        """可选：异步预取提示。未实现可作为 no-op。"""
```

### 5.2 Key 编码（必须可排序、可前缀）

要求 `encode_key(Address)` 满足：

- 按 `KeyOrder = (prefix_dims..., order_dim)` 编码
- 字节序的排序与 tuple 的字典序一致（至少在同 namespace 内一致）

实践上推荐：

- `int`：big-endian 定长编码（保持数值顺序）
- `str`：length-prefix UTF-8 编码（先写 4 字节大端长度，再写 UTF-8 字节）
- `bytes`：length-prefix

> 注：这里的“可排序”不是为了“数据库”，只是为了让 `range_scan(prefix, lo, hi)` 成立。

### 5.3 模式匹配 lowering

给定集合 schema（含 `Dims/KeyOrder/order_dim`）与一个已规范化的 `Region`：

- **NF-PointQuery → mget**
  - 构造唯一 `Address`（所有维度均为 Point）
  - `key = encode_key(address)`
  - emit：`mget([key])`（批量由上层聚合）

- **NF-RangeQuery → range_scan**
  - `prefix_addr` 取除 `order_dim` 外的所有点值，按 `prefix_dims` 编码为 `prefix`
  - 取 `Range(lo,hi)` 的 `lo,hi`
  - emit：`range_scan(prefix, lo, hi)`

否则：`UnsupportedPattern`（明确返回，不尝试“自动变通”）。

---

## 6. 路由与一致性：最小、明确、可实现

### 6.1 分区键与路由

每个集合必须声明 `partition_dims ⊆ prefix_dims`。路由键由这些维度的点值构成：

- `partition_key = project_prefix(region, partition_dims)`
- `shard = route(namespace, partition_key)`（hash/range 均可）

硬性要求：

- lowering 前必须能得到确定的 `partition_key`（即 `partition_dims` 都是 Point）。否则拒绝执行。

### 6.2 批量合并（系统级优化，不是查询优化器）

对一批请求（多个 NF query），系统做三件事：

- **按 shard 分组**：减少网络往返
- **点查合并**：同 shard 的 PointQuery 合并成一次 `mget(keys)`
- **范围合并**：同 shard 且同 `prefix` 的 RangeQuery，若范围重叠/相邻则合并

范围合并的确定性规则：

- 对相同 `prefix` 的区间集合按 `lo` 排序，线性合并重叠/相邻区间（`next.lo <= cur.hi`）。

### 6.3 写入语义（必须写清楚，否则一切都不严谨）

v1 建议固定为最简单的“覆盖写”语义：

- `put_batch`：同 key 多次写入，以最后一次为准（last-write-wins，在单机内）
- 不提供事务/多 key 原子性
- 如果需要版本/TTL，请在 namespace 层声明并体现在 value 编码中（不要把版本当成“隐式维度”塞进代数系统）

---

## 7. 预取（Prefetch）：只做确定性生成，不做在线自适应

预取只基于“有序维度 range + 固定步长/窗口”，不做学习、不做自适应。

给定一个 RangeQuery：

- 当前读取：`[lo, hi)`
- 预取 N 个窗口：`[lo+Δ, hi+Δ)`, `[lo+2Δ, hi+2Δ)`, ...

其中 `Δ` 通常等于 block_size（例如 KV cache 的 token block）。

预取的输出是额外的 `range_scan` 或 `mget`（取决于你是否将 block 抽象为点 key）。v1 建议：

- 若存储按 token block 对齐，把每个 block 作为 PointQuery（更易控 IO）
- 若后端支持连续 token 的 range_scan，则用 range 形式预取

---

## 8. 正确性声明与测试（只对“受限子语言”负责）

### 8.1 我们承诺证明/测试的内容

- `meet` 正确：\(\llbracket meet(c1,c2)\rrbracket = \llbracket c1\rrbracket \cap \llbracket c2\rrbracket\)
- 规范化幂等：`canon(canon(R)) == canon(R)`
- 规范化保持语义：\(\llbracket canon(R)\rrbracket = \llbracket R\rrbracket\)
- lowering 对 NF 保持语义：执行计划返回的键集合（或键-值流）与 \(\llbracket R\rrbracket\) 一致（在后端契约成立的前提下）

### 8.2 建议的 property-based 测试清单

- **Constraint meet**：随机生成 `Point/Range/Set`，用枚举（小域）对比 `meet` 结果集合
- **Canonicalization**：随机多次 `select` 组合，验证幂等性与交换/同维合并一致性
- **Lowering**：对 NF-PointQuery/NF-RangeQuery，比较：
  - `encode_key`/`range_scan` 返回的 key 集合
  - 与直接枚举 \(\llbracket R\rrbracket\)（仅在小域）的一致性

---

## 9. 实现路线图（现实版本）

| 阶段 | 目标 | 验收标准 |
|------|------|---------|
| P0 | Schema/KeyOrder 定义 + key 编码 + `meet/canon/lowering` | property-based 测试通过；NF lowering 全覆盖 |
| P1 | 单机后端（内存 + 可选 RocksDB）支持 `mget/range_scan` | 在目标 workload 上减少 IO 次数/减少 RPC 次数（可量化） |
| P2 | 批量合并 + 预取（固定规则） | P99 延迟下降；CPU 开销下降（profiling 可见） |
| P3 | 分布式路由（按 partition_dims） | 跨 shard 路由正确；失败/重试语义明确 |

---

## 10. 设计要点回顾（防止再次过度设计）

- 代数的价值来自：**稳定表示 + 可规范化 + 可 lowering**
- 我们只对“Point + 单维 Range”负责，不承诺通用 Region 计算
- 我们不做优化器/执行引擎；批量合并与范围合并是系统级确定性规则
- 依赖的是 **key 排序假设**，不是"物理连续字节布局"

