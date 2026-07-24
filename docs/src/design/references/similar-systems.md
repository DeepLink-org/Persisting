# 类似系统参考 — 分布式分层存储

**文档用途**：基于 [Distributed Tiered Storage](distributed_tiered_storage.md) 的设计要点，梳理可参考、借鉴的类似系统，供实现与演进时对照。

---

## 1. 本设计要点回顾

Persisting 分布式分层存储的核心特征可概括为：

| 维度 | 要点 |
|------|------|
| **寻址** | TTAS：多维张量地址（session/layer/head/time 等），Region → partition_key 路由、order_dim 上 block 化 |
| **粒度** | Block 为存储/传输/缓存单位；Block Table 跟踪 tier、状态、位置 |
| **透明性** | mmap + userfaultfd 将不连续 Block 映射为连续 VA，numpy/torch 无感 |
| **分层** | L0 GPU / L1 CPU / L2 Remote / L3 SSD；缺页/预取由同一事件循环调度 |
| **传输** | PCIe/NVLink/RDMA/io_uring 等按 tier 选路；Rust 侧事件循环避免 GIL |
| **场景** | KV Cache、PS/Optimizer state、轨迹等，统一在 TTAS + Block 模型下 |

下面按「KV Cache / 分层卸载」「用户态按需填页」「GPU 虚拟内存」「分布式对象/存储」四类，列出可借鉴系统并简要对比。

---

## 2. KV Cache 与分层卸载

### 2.1 LMCache

- **定位**：企业级 LLM 推理的 KV Cache 管理层，多 tier（GPU / CPU DRAM / 本地磁盘 / 远程）存储与传输。
- **与本文设计的关系**：
  - **相似**：多级存储、eviction/prefetch、与 vLLM/SGLang 等引擎集成；支持「存储模式」（offload 冷块）与「传输模式」（prefill-decode disaggregation，跨节点传 KV）。
  - **可借鉴**：异步数据移动、批量化、计算与 I/O 流水线；LRU 等 eviction 策略；与推理引擎的 connector 形态（我们可类比 BlockStore 的 backend 与 Pulsing 的集成方式）。
- **差异**：LMCache 以「KV 块」和业务 API 为主，不强调 TTAS 形式化寻址与通用多维 Region；我们则用 TTAS 统一 KV/PS/轨迹等命名空间。

### 2.2 vLLM KV Offloading Connector

- **定位**：vLLM 0.11+ 的可插拔 KV 卸载后端，内置 CPU DRAM backend。
- **与本文设计的关系**：
  - **相似**：异步 transfer API、CPU 作为 staging 再 offload 到更下层；request 级 preemption 时避免重算 KV。
  - **可借鉴**：Connector 的接口形态（谁负责「从哪搬到哪」）、与调度器的衔接方式；我们 BlockStore 的「选层 + 拉取」可参考其抽象边界。
- **差异**：vLLM 侧重单机/单引擎内 GPU↔CPU；我们还有 L2 Remote（跨节点）、L3 SSD，以及 mmap+UFFD 的透明 VA 视图。

### 2.3 NVIDIA Dynamo / KVBM

- **定位**：NVIDIA 的 KV Block Manager，协调多 tier 放置，配合 NIXL 做低延迟、异步、非阻塞传输。
- **与本文设计的关系**：
  - **相似**：Block 级管理、跨 tier 放置、专用传输库。
  - **可借鉴**：多 tier 协调策略、与 GPU 传输栈的集成方式；我们 L0↔L1 的 PCIe/NVLink 选路可参考其思路。
- **差异**：闭源/商用，我们以开源栈（UFFD、RDMA、io_uring）为主；我们还有 TTAS 与分布式路由（Pulsing）。

### 2.4 FlexGen

- **定位**：单 GPU 下用 CPU + 磁盘做权重与 attention cache 的 offload，用线性规划搜索 tensor 在各 tier 的存放与访问模式。
- **与本文设计的关系**：
  - **相似**：显式多 tier（GPU/CPU/Disk）、以 block/tensor 为单位做放置与搬移。
  - **可借鉴**：离线/在线对「谁放哪、何时搬」的建模思路；我们预取/驱逐策略可借鉴其「计算与 I/O 重叠」的建模。
- **差异**：FlexGen 偏 throughput、高延迟可接受；我们还要兼顾 P99 与透明 VA，且引入 L2 Remote 与 TTAS。

### 2.5 KVSwap（Disk-aware Offloading）

- **定位**：面向嵌入式/移动端的长上下文推理，KV 全量落盘，用紧凑元数据做预加载预测，计算与存储访问重叠。
- **与本文设计的关系**：
  - **相似**：磁盘为底层、块级管理、预取与 I/O 模式匹配。
  - **可借鉴**：设备特性与 I/O 模式匹配、预加载预测；我们 L3 SSD 的预取策略可参考。
- **差异**：我们面向数据中心、多节点与 RDMA，且强调统一 TTAS + Block 模型。

---

## 3. 用户态按需填页（mmap + userfaultfd）

### 3.1 UMap (LLNL)

- **定位**：用户态、基于 userfaultfd 的 memory-mapping 库，大容量/外存数据集通过 mmap 风格接口访问，页 fault 在用户态异步处理。
- **与本文设计的关系**：
  - **相似**：UFFD 监听缺页、自定义 handler 从「文件 / 内存服务器 / 网络」拉取页；可配置页大小、cache 大小、prefetch/eviction 策略；Manager + Evict Manager 分离。
  - **可借鉴**：UFFD 的注册与 UFFDIO_COPY 使用方式；预取/驱逐策略的可插拔设计；我们 BlockMappedBacking 与 UFFD handler 的边界可参考 UMap 的 Manager 抽象。
- **差异**：UMap 通用、页粒度、不绑定 tensor 与 Block；我们是 Block 粒度、TTAS 命名空间、且与 GPU VA 和分布式路由一体。

### 3.2 PageFlex (eBPF + userfaultfd)

- **定位**：用 eBPF 在用户态表达 paging policy，与 userfaultfd 结合做 tier 间页迁移。
- **与本文设计的关系**：
  - **相似**：用户态控制「哪一页从哪一 tier 拉」、UFFD 驱动填页。
  - **可借鉴**：policy 与 mechanism 分离的思路；若未来把「选层/预取/驱逐」做成可编程策略，可参考 eBPF 式解耦。
- **差异**：我们当前以 Rust 内固定策略为主，暂无 eBPF 扩展计划。

---

## 4. GPU 虚拟内存与按需迁移

### 4.1 CUDA Unified Memory (UVM)

- **定位**：主机与设备共享统一虚拟地址空间，GPU 访问未 resident 的页时触发 page fault，由驱动将页从 host 迁到 device（或反向）。
- **与本文设计的关系**：
  - **相似**：透明 VA、按需迁移、多级存储（host ↔ device）。
  - **可借鉴**：我们 L0 GPU 侧用 CUDA Virtual Memory API（cuMemCreate/cuMemMap）做「不连续 Block → 连续 GPU VA」的思路与 UVM 一致；可参考其 fault 行为与性能调优文档。
- **差异**：UVM 是驱动/运行时层、页粒度、不涉及 SSD/Remote；我们显式 Block、L3/L2 并在 Rust 侧统一事件循环。

### 4.2 CUDA Virtual Memory Management API

- **定位**：显式预留 VA、按需映射物理块、多 GPU 间共享 VA。
- **与本文设计的关系**：
  - **相似**：reserve → map physical chunk → set access，与我们在 distributed_tiered_storage 中「GPU 侧 reserve + 按 block 映射」一致。
  - **可借鉴**：多 GPU 下 VA 与物理块映射的最佳实践；我们 L0 的 Block 映射可直接对照该 API 的文档与示例。

---

## 5. 分布式对象存储与零拷贝

### 5.1 Ray Plasma

- **定位**：单节点进程间共享、不可变对象、零拷贝读取（如 numpy 数组）；多节点时通过 object ref 在节点间传递。
- **与本文设计的关系**：
  - **相似**：分布式内存、对象/块级访问、避免多余拷贝。
  - **可借鉴**：同节点内零拷贝与 seal 语义；我们 L1 上多进程共享同一 BlockStore 时可参考其共享内存与引用方式。
- **差异**：Plasma 是「对象」粒度、无 tier 与缺页透明；我们是 Block + tier + UFFD，且与 TTAS 绑定。

---

## 6. 可借鉴点与差异小结

| 方向 | 可借鉴点 | 本设计的差异/侧重 |
|------|----------|--------------------|
| **KV 分层** | LMCache/vLLM/Dynamo 的 tier 划分、connector 抽象、eviction/prefetch 策略 | TTAS 统一寻址、L2 Remote + L3 SSD、mmap+UFFD 透明 VA |
| **按需填页** | UMap 的 UFFD 使用方式与 Manager/Evict 分离；PageFlex 的 policy 解耦 | Block 粒度、与预取/缺页同一事件循环、Rust 侧实现避 GIL |
| **GPU VA** | CUDA UVM/VMM API 的 reserve-map-setAccess、多 GPU VA 实践 | 仅 L0 用 GPU VA；L1 用 mmap+UFFD；调度统一在 Rust |
| **分布式** | Plasma 同节点零拷贝与对象语义 | 我们以 Block 为单元、跨节点经 Pulsing 路由、RDMA/ RPC |

实现时建议优先参考：**UMap**（UFFD 与 handler 设计）、**LMCache**（多 tier 与推理引擎集成）、**CUDA VMM**（L0 Block 映射），并在文档与代码中保持与 [Distributed Tiered Storage](distributed_tiered_storage.md) 的对应关系。
