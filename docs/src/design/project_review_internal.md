# Persisting 项目内部评审（直接版）

> **说明**：内部评审文档，不作为正式设计文档的一部分；设计以 [设计文档索引](index.md) 中的核心设计为准。

从开源运营 + 开发者双视角做的整体 review，自己人直接说，不绕弯子。

**一句话**：Persisting 做的就是**基于 Pulsing 快速构建可靠、高效的存储**。

---

## 一、整体评价

**现状**：Persisting 基于 Pulsing，提供队列/轨迹的持久化（Lance 后端 + Sampler）以及规划中的参数 / KV Cache / 轨迹统一存储，目标就是让在 Pulsing 上做存储**快速、可靠、高效**。设计文档写得认真（LMCache 参考、统一存储可行性等）。当前交付物主要是「Lance 队列后端 + 一套 TQ 对齐的 Sampler」；对外可以独立表述为「持久化后端 + 采样 + 未来统一存储」，但**实现与价值都建立在 Pulsing 之上**。README/Roadmap 里三大块只有「轨迹」沾边（队列即轨迹管道），参数和 KV Cache 仍是规划。

**水平**：代码结构干净、依赖清晰（基于 Pulsing + Lance），文档和示例有，属于**早期但方向明确**的库。和成熟开源比，缺的是：可验证的测试、可对比的性能数据、以及「一个先打透的单一价值点」。

---

## 二、与同类/相关项目对比

| 维度 | Persisting | LMCache | Redis/Valkey (做 cache) | Lance |
|------|------------|---------|-------------------------|-------|
| **定位** | 基于 Pulsing 快速构建可靠、高效的存储（队列/轨迹 + Lance + 未来统一存储） | LLM KV Cache 复用，减 TTFT、提吞吐 | 通用 KV/队列 | 列存格式与引擎 |
| **当前核心能力** | 队列 → Lance 持久化；Sampler；与 Pulsing 集成 | 多级存储、vLLM/SGLang 集成、多种后端 | 成熟、生态大 | 存储格式，被消费 |
| **依赖/生态** | 基于 Pulsing 构建；Lance 作存储引擎 | 绑 vLLM/SGLang，存储可插多种 | 无绑具体框架 | 无绑应用 |
| **成熟度** | 早期，队列后端落地 | 生产在用，多厂商集成 | 极成熟 | 成熟格式层 |

**结论**：

- Persisting 的定位就是**基于 Pulsing 快速构建可靠、高效的存储**；对外可以独立表述能力（队列/轨迹持久化、Sampler、未来统一存储），但价值与实现都落在 Pulsing 生态内。
- **差异化**：在 Pulsing 上开箱即用的 Lance 列存、统一存储方向（参数/KV/轨迹）、可作 LMCache 后端——让「在 Pulsing 上做存储」更快、更稳、更省事。
- **水平**：单就「队列持久化」这一块，实现是合理可用的；和 LMCache 那种多后端、多集成的完成度比，属于更早阶段，需要收敛到一个「第一成功用例」再铺开叙事。

**与 LMCache 的关系（能否代替）**：LMCache 设计上偏重（多级存储、多后端、与 vLLM/SGLang 深度集成、配置与运维成本高）。**在 Pulsing 生态内**，Persisting 可以做成「同栈、更轻」的 KV/缓存与持久化方案：用 Pulsing + Lance 直接提供所需能力，不必再接一套 LMCache。对这类用户而言，**在 Pulsing 场景下可以代替 LMCache**——不必上重量级方案，用 Persisting 快速、可靠、高效地搞定存储。在**泛 LLM 推理生态**（vLLM/SGLang 用户、要 LMCache 现成集成）里，Persisting 不覆盖、也代替不了 LMCache。总结：**在「基于 Pulsing 做存储」的前提下，Persisting 可以也应当成为 LMCache 的轻量替代，而不是只能当 LMCache 的一个后端**。

**Pulsing 的通信骨架能否帮 Persisting 超越 LMCache？** 可以，但是在「分布式与数据面统一」这个维度。LMCache 没有自己的通信骨架：它是「挂到 vLLM/SGLang + 插各种存储后端」，拓扑是围绕推理引擎的单点/客户端-服务端。Pulsing 提供的是**原生分布式**：Actor、放置、RPC、队列，跨节点一致的数据流与协调。Persisting 建在这套骨架上，能做出 LMCache 很难做的事：**放置感知的存储**（例如 KV 靠近要用的计算节点）、**同一数据面上的参数 / KV / 轨迹**（训练、推理、回放共用一套通信模型）、**多节点扩展**不依赖「一个中心 cache 服务」。所以：在「分布式、可放置、与训练/回放同栈」这些点上，Pulsing 的通信骨架确实能给 Persisting 一个超越 LMCache 的支点。在「生态、vLLM 现成集成、已有生产案例」上，LMCache 仍会领先，除非 Persisting 把 KV 真正跑在骨架上并拿出可复现的收益（延迟、吞吐、资源利用率）。**结论**：骨架是优势来源；是否「超越」取决于是否把 KV/参数 做实并证明「同栈 + 放置感知」带来可量化的好处。

**KV cache 和参数是否适合用 Persisting 存？**  
- **参数**：**适合**。模型参数是大块、读多写少、要持久化、多节点要能拉得到。Lance 列存 + Pulsing 放置（例如按分片挂到不同 Actor）很契合；格式（safetensors 等）、版本、大文件分片/流式读是设计量问题，没有本质冲突。用 Persisting 做参数存储是自然延伸。  
- **KV cache**：**适合做持久化层 / 温冷层，不适合做唯一热层**。KV 推理期极热、延迟敏感、生命周期短；若每次读都走 Lance 落盘，延迟会明显高于内存/本地缓存。更合理的用法是：**热路径用内存（或 Pulsing 上本机缓存），Persisting 负责落盘与复用**——例如前缀写入 Persisting，需要时从 Persisting 加载进内存再参与推理；或作为 eviction 的落盘后端。这样 Persisting 扮演「可靠、可复用的 KV 持久化」，而不是「每请求都直读磁盘」。结合 Pulsing 放置，还可以做「KV 靠近计算节点 + Persisting 做该节点的持久化后端」，体验会更好。  
总结：参数可以放心往 Persisting 上放；KV cache 适合作为 Persisting 的**持久化与加载源**，热路径仍要有一层内存/本地缓存，别把 Persisting 当唯一热存储用。

---

## 三、风险项（项目成功角度）

1. **叙事可独立，但别模糊「基于 Pulsing」**  
   对外可以独立表述 Persisting 的能力（队列/轨迹持久化、Lance、Sampler、未来统一存储），但核心信息要明确：**基于 Pulsing 快速构建可靠、高效的存储**。避免让人误以为 Persisting 是通用存储库、与 Pulsing 无关；也避免只讲 Pulsing 不讲 Persisting 能带来什么（快、稳、高效）。

2. **几乎没有自动化测试**  
   仓库里没有在 CI 里跑的 pytest；改 queue/sampler 或以后加 KV/参数，回归成本高，也难让外部贡献者放心改。

3. **承诺与交付错位**  
   标题和描述是「Parameters, KV Cache, and Trajectories」，实际可用的只有队列后端 + Sampler。容易给人「规划很多、落地很少」的感觉，除非明确把「当前阶段 = 队列持久化」说死。

4. **Sampler 的集成入口尚不完整**  
   `get_sampled_batch` 要配合带 `get_by_indices` 的后端（如 LanceBackend）用；若通过 Pulsing 的 QueueReader 用队列，目前没有「带 sampler 的读」的官方入口，Sampler 更像是「直接操作 backend 时用」的能力。独立表述时可以说清楚：Persisting 提供 Sampler + LanceBackend.get_by_indices，与具体队列消费者（Pulsing 或其他）的对接由集成方完成。

5. **没有可复现的性能基线**  
   没有 benchmark、没有「相对 memory 后端或落盘策略」的数字， adopters 很难判断「用 Persisting 持久化」要付出多少延迟/吞吐代价。

6. **缺乏整体架构与核心抽象**  
   参数/KV/轨迹归一的设计文档有，但缺少**整体架构图**和**核心抽象**。已补 [Architecture & Core Abstractions](architecture_core_abstractions.zh.md)，并明确：**核心应是「分布式 tensor 的存储与传输」**，而非仅一层统一 key-value；Store/Namespace/Backend 作为持久化/寻址层服务于该核心。代码层尚未有 tensor 存储/传输抽象或 Store 协议，若长期只有队列 Backend，会加重「规划多、实现少」的印象。

7. **社区与协作配套缺失**  
   单作者、无 CONTRIBUTING、无 issue/PR 模板、无 CoC。若目标只是内部或小范围用，没问题；若想拉外部贡献或做大声量，会显得准备不足。

---

## 四、改进建议（按优先级）

**P0（先做再谈别的）**

1. **补测试并进 CI**  
   至少：LanceBackend 的 put/flush/get、恢复、get_by_indices；Sampler 的 SequentialSampler + get_sampled_batch。CI 里跑 pytest，保证主路径不 silently 坏。

2. **收窄「当前版本」的叙事**  
   在 README 和文档里明确写：Persisting **基于 Pulsing 快速构建可靠、高效的存储**；当前主交付是「Lance 队列后端 + Sampler」，参数/KV/轨迹是路线图。既说清「在 Pulsing 上做存储」的价值，也说清当前做到哪一步。

3. **做一个最小可用的性能数字**  
   一个脚本即可：例如 10 万条 record 写满 + flush + 读回，报 throughput（条/秒或 MB/s），写在 README 或 docs。不需要和谁 PK，先有自己的基线。

**P1（半年内值得做）**

4. **落实「第一成功用例」**  
   二选一或都做：  
   - 把「队列持久化」做到可生产：例如 WAL/恢复、简单 compaction 或文档说明当前取舍；或  
   - 出一个**最小 LMCache 后端**（一个文件实现 StoragePluginInterface，存 Lance），证明「Persisting 作为 LMCache 后端」可行，并和统一存储叙事对齐。

5. **整体架构与核心抽象落地**  
   文档已补 [Architecture & Core Abstractions](architecture_core_abstractions.zh.md)。代码上：定义 **Store 协议**（put/get/exists/delete/list_prefix）与 **Namespace** 约定，实现**最小 UnifiedBackend**（或扩展 LanceBackend 实现 Store），先支持一个 ns（如 `traj`），让队列/轨迹在架构上归到 Store(traj)，参数/KV 后续再接。

6. **Sampler 在集成侧有一处清晰入口**  
   若走 Pulsing：可在 read_queue / QueueReader 上支持 `sampler=SequentialSampler()`，内部用 `get_sampled_batch`。独立表述时则明确：Persisting 提供 Sampler + backend.get_by_indices，集成方（Pulsing 或其它）负责在「读队列」时挂上 sampler。

**P2（开源与生态）**

7. 若目标是吸引外部贡献：加 CONTRIBUTING.md、issue/PR 模板、CoC；在 README 里写「当前维护状态」和「欢迎贡献的方向」。

8. 可选：在文档和示例里加一段「不依赖 Pulsing 的用法」（例如直接用 LanceBackend 做单机队列或轨迹 dump），强化 Persisting 独立可用的印象。

---

## 五、总结

- **定位**：Persisting 做的就是**基于 Pulsing 快速构建可靠、高效的存储**（队列/轨迹 + Lance + Sampler + 未来统一存储）；与 LMCache/Redis 是互补或集成关系。对外可独立表述能力，但核心信息始终是「在 Pulsing 上，用 Persisting 把存储做得快、稳、好用」。
- **水平**：实现干净、设计想得远，但交付还集中在队列后端；需要把「当前阶段 = Lance 队列后端 + Sampler」说清楚，并把测试、性能基线和「一个打透的用例」补上。
- **成功关键**：先让「Persisting 队列持久化」（无论是否经 Pulsing 用）或「Persisting 作为 LMCache 后端」中的至少一个成为可验证、可复现、有数字的故事；再在统一存储上落一点代码，避免长期停留在设计文档。

以上可直接当内部 roadmap 和风险清单用；对外发布时可按需摘成「当前状态」和「路线图」，不必照搬这份语气。
