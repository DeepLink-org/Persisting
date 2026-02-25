# Persisting 项目 Review：与 Pulsing 水平对齐的差距与瓶颈

> **目标**：将 Persisting 提升到与 Pulsing 类似的工程与产品水平。  
> **定位**：Persisting 负责与存储相关的大块数据存储和传输，一体化的 Tensor 分布式分层存储。  
> **参照**：Pulsing 的 CI、测试、文档与 API 一致性、Rust 核心角色。

---

## 一、现状概览

| 维度 | Pulsing | Persisting |
|------|--------|------------|
| **定位** | 分布式 Actor 运行时（控制面） | Tensor 分布式分层存储（数据面） |
| **核心交付** | Actor、Queue/Topic、Ray 兼容、集群 | Queue 持久化（Lance）+ Sampler + TAA 代数 |
| **README/文档承诺** | 与可实现 API 一致 | `open()` / KV / 参数 / 轨迹 多为规划，未实现 |
| **CI** | 完整：lint、Rust test、多平台 wheel、Python 多版本 test、覆盖率 | 仅 mkdocs 发布 |
| **测试** | 200+ 用例，core/streaming/apis/integrations 分层 | ~17 个用例，8 个文件，未进 CI |
| **Rust 角色** | 完整 runtime（actor、cluster、transport） | 仅 TAA 代数（Dimension/Point/Range/Region 等） |
| **发布** | PyPI、多平台 wheel、Codecov | 未上 PyPI，无基准数字 |

---

## 二、主要差距（相对 Pulsing）

### 2.1 承诺与实现错位

- **文档/README** 大量使用 `persisting.open("kvcache/v1", dims=(...), order_dim=TIME)`、`kv["s1", 0, 2, 0:512].tensor()`、参数/轨迹 `open()` 等。
- **代码** 中：
  - `persisting.open()` **未实现**（`__init__.py` 未导出 `open`）。
  - 实际可用的是：`Queue`、`LanceBackend`、Sampler、`TensorView`/TAA 下标规划（`Region`、`canonicalize`、`project_prefix` 等）。
- **影响**：用户按 README 尝试会失败；叙事是「Parameters, KV Cache, Trajectories」，交付集中在「队列 + 采样」，易形成「规划多、落地少」的印象。

**建议**：在 README 和文档中明确区分「当前可用」与「路线图」；当前版本主交付写死为「Lance 队列后端 + Sampler + TAA 规划」，KV/参数/轨迹仅作规划。

---

### 2.2 CI 与工程化

| 能力 | Pulsing | Persisting |
|------|--------|------------|
| Lint（Rust fmt/clippy + Python ruff） | ✅ 有 | ❌ 无 |
| Rust 单元/集成测试 | ✅ 有，多平台 | ❌ 无 |
| Python pytest 进 CI | ✅ 有，多 Python 版本 | ❌ 无 |
| 多平台 Wheel 构建 | ✅ macOS / Linux x86/aarch64 | ❌ 无 |
| 覆盖率（Rust + Python） | ✅ Codecov | ❌ 无 |
| 文档发布 | ✅ mkdocs | ✅ mkdocs |

**影响**：改 Queue/Sampler/TAA 或后续加功能，回归成本高；外部贡献者难以确认「改完是否还能用」。

**建议**：优先增加「CI 跑 pytest」（至少 LanceBackend put/flush/get、Sampler、TAA 规划）；可选增加 ruff、Rust fmt/clippy 与 Rust 单测。

---

### 2.3 测试深度与结构

- **Pulsing**：`tests/python/` 下按 core / streaming / apis / integrations 分层，覆盖 remote、actor_system、queue、topic、ray 兼容等。
- **Persisting**：`tests/` 约 8 个文件、17 个 test，覆盖：queue put/get、backend zerocopy、kv_interface、metadata、sampler、status_tracker、tensor_serde、taa_subscript_planning。缺少：
  - Lance 恢复、`get_by_indices`、flush 语义的专项测试；
  - 与 Pulsing 的集成测试（如通过 Pulsing Queue 使用 Persisting 后端）；
  - 性能回归或基准测试。

**建议**：补 LanceBackend 恢复与 `get_by_indices` 测试；Sampler + `get_sampled_batch` 与后端联调测试；至少一个可复现的吞吐脚本结果写入文档（条/秒或 MB/s）。

---

### 2.4 核心抽象：文档有、代码无

- **设计文档**（如 `architecture_core_abstractions.zh.md`）已明确：
  - 核心应是「**分布式 tensor 的存储与传输**」；
  - Tensor 为一等公民（shape/dtype/shard/placement/version）；
  - 传输：allocate / load / materialize，与 Pulsing 放置结合；
  - Store/Namespace 作为持久化/寻址层。
- **代码**：仅有 Queue 流式存储 + TAA 的 **规划**（Region、canonicalize、project_prefix）；没有：
  - Tensor 句柄/描述、传输 API；
  - 分层（GPU/host/SSD）的物化路径；
  - Store 协议或 UnifiedBackend。

**影响**：与「一体化 Tensor 分布式分层存储」的定位之间，缺一层可运行的抽象，长期会加重「设计多、实现少」的感觉。

**建议**：先收敛「当前阶段 = 队列 + Sampler + TAA 规划」的叙事；再选一个最小闭环落地（例如单节点「从 Lance 按 TAA 键 load 一块 tensor」或最小 Store 协议），避免核心抽象长期只停留在文档。

---

### 2.5 Rust 侧能力

- **Pulsing**：Rust 承担 actor、mailbox、cluster、transport、streaming 等完整运行时。
- **Persisting**：Rust（`persisting-core`）只做 TAA 代数（Dimension、Point、Range、SetC、Address、Region、canonicalize、project_prefix 等），无存储、无网络、无 tiering。

**影响**：高性能数据路径（编码、零拷贝、分层调度）目前都在 Python 侧，Rust 未参与热路径。

**建议**：短期可接受；中长期若要做「与 Pulsing 类似水平」的数据面，需考虑在 Rust 中落地 key 编码、分批规划或与 Pulsing 的零拷贝协议对接等，再逐步把热点下推到 Rust。

---

### 2.6 与 Pulsing 的集成完整性

- **设计**：Persisting 作为 Pulsing 的 StorageBackend，在 BucketStorage Actor 内运行；数据经 StorageManager → 一致性哈希 → BucketStorage → Backend。
- **代码**：Persisting 提供 `LanceBackend`/`PersistingBackend` 和 `KVInterface`；是否已在 Pulsing 的 `BucketStorage`/streaming 中**默认或官方**挂接，需要对照 Pulsing 代码确认。
- **入口**：Sampler 的 `get_sampled_batch` 依赖带 `get_by_indices` 的后端；若通过 Pulsing 的 QueueReader 消费，是否有「带 sampler 的读」的官方入口，文档已指出尚不完整。

**建议**：确认 Pulsing 侧是否已把 Persisting 的 Backend 作为推荐/默认后端之一，并在文档中写清「Persisting + Pulsing」的推荐用法与入口。

---

## 三、主要瓶颈总结

1. **承诺与交付错位**  
   README/quickstart 主推 `open()` / KV/参数/轨迹，但未实现；容易导致信任度下降。**收窄当前版本叙事**可立即缓解。

2. **无自动化测试与 CI**  
   改动易引入回归，贡献者难以验证。**把 pytest 纳入 CI** 是提升到「Pulsing 类似水平」的前提之一。

3. **无性能基线**  
   「高效」「一体化存储」缺少可复现数字（吞吐、延迟）。**一个最小 benchmark 脚本 + 文档中的基线**即可建立改进锚点。

4. **核心抽象未落地**  
   「分布式 tensor 存储与传输」、分层、Store 协议仅在设计文档中；代码仍以 Queue + TAA 规划为主。需要**选一个最小闭环**（例如单节点 load 或最小 Store）先实现，再扩展。

5. **Rust 仅做 TAA**  
   数据面热路径仍在 Python，Rust 未参与存储与网络。中长期若对标 Pulsing 的「Rust 核心」，需规划 Rust 侧编码/规划/零拷贝等。

6. **与 Pulsing 的集成与入口**  
   需确认 BucketStorage 集成是否完整、Sampler 在「通过 Pulsing 读队列」场景下是否有清晰入口，并在文档中写清。

---

## 四、建议优先级（对齐 Pulsing 水平）

| 优先级 | 行动项 | 说明 |
|--------|--------|------|
| **P0** | 收窄当前版本叙事 | README/文档明确：当前 = Lance 队列后端 + Sampler + TAA 规划；KV/参数/轨迹为路线图；`open()` 未实现则不在 Quick Start 主流程展示可运行代码。 |
| **P0** | CI 跑 pytest | 至少：LanceBackend put/flush/get、恢复、get_by_indices；Sampler + get_sampled_batch；TAA 规划。保证主路径不静默坏。 |
| **P0** | 建立最小性能基线 | 一个脚本（如 10 万条 write/flush/read）输出 throughput，写入 README 或 docs，便于后续优化对比。 |
| **P1** | 补测试与 Rust 单测 | Lance 恢复、get_by_indices、与 Pulsing 的集成测试；persisting-core 的 Rust 单测。 |
| **P1** | 最小核心抽象或 Store | 定义 Store 协议（put/get/exists/list_prefix）或「单节点按 TAA 键 load 一块 tensor」的最小实现，让「tensor 存储与传输」在代码里有一点落点。 |
| **P1** | Sampler 集成入口 | 在 Pulsing QueueReader 或 Persisting 侧提供「带 sampler 的读」的官方用法与文档。 |
| **P2** | Lint/多平台/覆盖率 | ruff + Rust fmt/clippy；可选多平台 wheel、覆盖率上传；CONTRIBUTING/issue 模板等。 |

---

## 五、总结

- **定位**：Persisting 作为「基于 Pulsing 的 Tensor 分布式分层存储」是清晰的；当前实现以 **Lance 队列 + Sampler + TAA 规划** 为主，与文档中「Parameters, KV Cache, Trajectories」和 `open()` 的承诺存在明显错位。
- **与 Pulsing 的主要差距**：CI/测试/工程化不足、承诺与实现不一致、核心抽象（tensor 存储与传输、分层）尚未在代码中落地、Rust 仅做 TAA、与 Pulsing 的集成与入口需确认并文档化。
- **提升到「Pulsing 类似水平」的关键**：先对齐「可验证、可复现」——收窄叙事、CI 跑测试、建立性能基线；再补核心抽象的最小实现与集成入口，避免长期停留在设计文档。

以上可直接作为 Persisting 的差距清单与短期路线图使用。
