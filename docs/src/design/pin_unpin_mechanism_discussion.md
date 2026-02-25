# Pin/Unpin 机制设计讨论

本文档整理显存/分层存储管理的设计**目标与问题**，并列出当前契约中的形态、开放点与可选方案。**当前方案仍不足**，文档用于推动讨论和迭代，而非描述已完成的方案。

---

## 0. 核心目标与约束（我们要解决的问题）

### 0.1 生命周期的自动管理

**关键点**：设计的第一要义不是「何时 unpin」「pin 与引用谁先谁后」，而是**生命周期的自动管理**。用户不应关心 block 何时可驱逐、何时做 unpin；只要还有任何东西（Handler、物化出的 GPU tensor、numpy 数组等）在使用这块数据，block 就不能被驱逐；当这些引用在各自层面被释放或回收后，block 自然可以回收/驱逐。这依靠**多层引用与垃圾回收**来实现：

- **多层**：Python 侧的 ref（Handler）、运行时对 block 的 hold、物化得到的 GPU tensor / CPU buffer、stream 上的使用区间等，每一层都是「引用」。
- **垃圾回收**：每一层在自己的生命周期结束时（with 退出、对象析构、stream 完成、GC 回收）自动释放对 block 的持有；不需要用户手写 unpin 或协调「先放 tensor 再 unpin」。
- **Pin/unpin** 是上述中的**一层**（例如 with 或 stream 回调带来的 hold），不是用户要单独推理的概念；与「派生 tensor 的引用」等一起，共同构成统一的生命周期，由运行时通过 ref-count 或 GC 自动管理。

因此下文中的「两个算子」「unpin 时还有引用」等，都应放在「如何实现生命周期的自动管理、多层引用如何在释放时自动反馈到 block 可驱逐性」的框架下理解；目标始终是：**用户无感，生命周期自动**。

**效果**：这种自动的显存/内存管理能**等效出更多的可用显存**：不再被误占或漏释的 block 可以尽早参与驱逐与复用，同一物理显存下可容纳更多有效工作集（更大 batch、更长 context、更多并发），从而**支撑更好的性能**（吞吐、延迟、可扩展性）。

**两条时间线**：显存管理最主要的问题在于存在**两条时间线**，且这两条时间线上的**显存状态并不一致**：

- **Host 时间线**：with 退出、Python 引用释放、GC、block manager 的 pin 计数等，按 CPU/主机顺序发生。
- **Device 时间线**：GPU kernel 提交、在 stream 上执行、真正用完某块显存的时间点，按设备顺序发生。

同一块显存在两条时间线上的「是否仍被占用」可能不同：Host 已认为释放（例如 with 已退出）时，Device 上 kernel 可能还在用；反之 Device 已算完成时，Host 可能尚未做 release。若只按一条线决策（例如只看 host 就驱逐），会 use-after-free 或过度保守、等效显存变少。因此设计必须**同时尊重两条时间线**：释放/可驱逐的判定要绑到「真正使用方完成」的那条线（如 `after=stream` 把 unpin 绑到 device 完成），两条线上的状态通过事件/回调同步，才能既安全又等效出更多显存。

**设备侧决策**：在此基础上，**关键是让设备有能力决策预取和驱逐**。预取、驱逐等动作若只由 host 按自己的时间线发起，容易与 device 实际使用错位；若由**设备**（或设备时间线上的事件/算子）参与或主导这些决策——例如在 stream 上「用完即触发驱逐」、或由设备侧策略决定下一批预取哪些 block——则动作与「谁在用、谁用完」一致，两条时间线对齐，显存利用更准、等效容量更大。因此设计上应让设备具备发起或驱动预取/驱逐的能力（如 device 回调、stream 上算子通知 block manager），而不仅是 host 单边下发。

**与 CUDA async malloc/free 的关系**：CUDA 的 **cudaMallocAsync / cudaFreeAsync**（流序分配与释放）已经在尝试解决同类问题：在指定 stream 上按顺序执行 alloc/free，释放与「该 stream 上真正用完」对齐，由设备时间线驱动显存生命周期，从而减少 host 与 device 时间线不一致带来的误释或过度保守。我们的「两条时间线 + 设备侧决策」与之一致；设计上可与 stream-ordered 语义对齐（例如 block 的 pin/unpin 或 L1 的分配/回收与 async alloc/free 同序），或在 CUDA 的 async pool 之上构建 L1/block 管理层，避免重复造轮且复用已有机制。

**多流同步仍是难点**：单流时 stream-ordered 的 alloc/free 相对清晰；一旦**多流**参与（同一 block 被多个 stream 使用、或预取/计算/驱逐分布在不同 stream），何时算「真正用完」、由哪条流触发释放、流与流之间的依赖与同步都会变得很麻烦。设计需要显式考虑多流场景：例如按「所有相关 stream 都完成」再释放、或引入统一的 completion 事件/句柄，避免只按单流假设导致误释或难以表达的同步逻辑。

### 0.2 当前方案的不足（还不行）

目前契约与讨论中的形态**尚未达到上述目标**，存在明显缺口：

- **设备并未真正「决策」**：预取、驱逐仍由 host 发起（`kv.prefetch(key)`、with 退出或 stream 回调触发 unpin）。设备只是「被通知完成后再回调 host」，没有「设备侧主动发起预取/驱逐」或「设备侧策略决定下一批动作」的能力。
- **两条时间线未在实现层对齐**：`after=stream` 只是把 unpin 绑到 stream 完成，stream 的完成事件仍由 host 侧监听并调 block manager；设备时间线上的状态没有成为 block manager 的 first-class 输入，容易仍是「host 猜 device 何时用完」。
- **生命周期自动管理未落地**：多层引用（如物化出的 GPU tensor 对 block 的 hold）多数未在运行时登记，block 可驱逐性仍主要依赖 pin 计数，存在「unpin 时还有引用」或「引用未释放就 evict」的风险。
- **预取与驱逐未形成闭环**：预取谁、何时驱逐谁，若都由 host 根据高层逻辑决定，与 device 实际访问模式可能脱节；缺少「设备侧可见的显存状态 + 设备侧可触发的动作」的闭环。

因此需要**重新设计或演进**：要么在现有 pin/unpin 之上增加「设备可发起/可参与」的接口与协议，要么引入设备侧 agent/策略与 block manager 的协作方式，使预取和驱逐真正由设备时间线驱动。下文「当前约定」仅作现状与讨论基准，**不是最终可行方案**。

### 0.3 方案方向：GPU 侧显存全用 ref，按算子 trigger 显存 ref 计算

一个可行的设计方向是：**把 GPU 侧的显存全部用 ref 表示**（不直接持有一块块「裸」显存，而是持有一组 ref，每个 ref 对应某 block/region）；**每引入一个新的算子、且该算子涉及新的 tensor 时，就 trigger 一次「显存 ref 的计算」**，根据当前已有的 ref 集合、新算子需要的 tensor（即新 ref 集合）、以及显存容量与策略，算出：
- **哪些 ref 需要加载**（对应 block 尚未在 GPU 或需更新 → prefetch/load）；
- **哪些 ref 可以驱逐**（不再被当前及后续可见的算子需要 → evict）。

这样：

- **决策时机**：以「新算子引入」为触发点，每次都是基于「当前图/调度状态」做一次显存决策，天然与计算图/算子边界对齐；设备侧只要在加入新 op 时触发这次计算，就由设备侧（或调度器）驱动加载与驱逐。
- **多流**：多流下可以按「各 stream 上已提交的算子 + 本次新算子」一起参与 ref 计算，得到「所有相关 ref 的 live 集合」，再决定加载/驱逐，避免单流假设；同步逻辑收敛到「何时做下一次 ref 计算」和「计算输入里包含哪些 stream 的哪些 op」。
- **两条时间线**：ref 计算发生在「引入新算子」的时刻（可以是 host 提交时，也可以是设备侧调度时），计算结果是「需要加载 / 需要驱逐」的集合，执行可以按 stream 序排队；释放（驱逐）与「谁在用」由 ref 的 liveness 决定，而不是 host 单边说了算。
- **实现形态**：可以是「调度器每 enqueue 一个涉及新 tensor 的 op，就调一次 block manager 的 ref 计算接口，传入当前 GPU 上的 ref 集合 + 新 op 的 input/output ref，得到 load set 与 evict set，再下发 prefetch/evict」。

**约束：kernel 内不能 trigger CPU 侧显存管理**。显存 ref 计算、prefetch/evict 的决策与下发都跑在 host（CPU）上，而 **kernel 在 device 上执行，不能直接调用 host 或触发 CPU 侧逻辑**。因此「每引入一个新算子就 trigger 一次」的 trigger 必须是 **host 侧** 的时机，例如：
- 在 **enqueue 该算子之前**（host 提交时）：先做 ref 计算、下发 load/evict，再 enqueue kernel；
- 或 **stream 上某次 host 回调**（例如上一批 kernel 完成后的 callback）：在回调里做 ref 计算、再 enqueue 下一批。

不能依赖「kernel 内部某条指令触发 CPU 显存管理」；若希望更贴近 device 时间线，只能通过「host 在合适的时机（如 enqueue 前、或 stream 完成回调）主动算一次」来近似。

**若 GPU 能自己 trigger 这些管理操作，的确能解决很多问题**：决策与执行都发生在设备时间线上，无需 host 猜测「device 何时用完」；多流下可由各 stream 或设备侧调度器在「真正用完」或「即将需要」的瞬间发起 load/evict，两条时间线自然合一，显存利用更准、同步逻辑更简单。当前主流 GPU 编程模型（如 CUDA）下 kernel 不能发起对 host 的调用或对「显存管理器」的请求，因此只能依赖上述 host 侧 trigger。若未来有**设备侧可发起**的机制——例如 device 上 persistent kernel 或轻量 agent 在计算 kernel 间隙运行、可 enqueue 对 block manager 的请求，或硬件/驱动支持「device-initiated callback / request」——则可以把 ref 计算或 load/evict 的触发权真正下放到 GPU，设计上应预留或优先考虑这类扩展。

**延伸：GPU 自维护 ref 表并自行加载/驱逐**。若 **GPU 自己维护一份 ref 表**（在 device 上或 device 可访问的显存中），并**由 GPU 自己进行加载和驱逐**的决策与执行（例如 device 侧 persistent kernel 或调度逻辑根据 ref 表与当前/即将执行的算子决定 load/evict，再通过 DMA 或与 L2/L3 的接口搬数据），则：
- **更简单**：显存状态与决策权集中在一侧（GPU），无需 host 与 device 各持一份视图再同步；host 只负责提交「要算哪些 ref」或高层 DAG，设备侧负责 ref 表、liveness、何时 load/evict。
- **更高效**：决策基于设备时间线与设备本地状态，无 host–device 往返；加载/驱逐与计算在同一时间线上，可做更激进的预取与更及时的驱逐，等效显存利用率更高。

实现上 ref 表可放在 GPU 显存或统一地址空间中 device 可读写的区域；加载/驱逐可由 device 上的轻量调度循环或与计算 kernel 交错的 micro-op 完成（依赖运行时/硬件对「device 发起 DMA 或 block 请求」的支持）。若该方向可行，可作为「设备侧决策」的终极形态纳入设计目标。

**关键前提：kernel 能 trigger copy engine，或 kernel 能 launch kernel / 操作 event**。与「host 侧 trigger + 回调」方案的本质差异在于：要由 GPU 自己完成加载和驱逐，就必须在 **device 上** 具备以下能力之一（或组合）：
- **Kernel 触发 copy engine**：kernel 内能发起一次 copy/DMA 请求（例如从 L3/显存某处搬到当前使用的显存），而不需要 host 下发 copy 命令；这样 device 侧调度逻辑在决定「需要加载某 ref」时可以直接驱动 copy engine。
- **Kernel launch kernel**（动态并行）：一个 kernel 可以 launch 另一个 kernel，这样 device 上的「调度 kernel」或 persistent 逻辑可以按需 launch「copy kernel」或下一阶段计算 kernel，实现真正的设备侧流水与显存管理。
- **Kernel 操作 event**：kernel 能 record 或 wait event，从而在 device 上形成依赖与顺序，供 copy 与计算交错、或多流协同；若再配合「event 触发 copy」等机制，也可在无 host 参与下推进。

若硬件/驱动不暴露上述能力，则「GPU 自维护 ref 表并自行加载/驱逐」无法落地，只能退回到 host 侧在 enqueue 前或回调里 trigger 的形态。设计时应明确依赖哪类能力，并区分「有 device 侧 trigger 时的目标架构」与「仅 host trigger 时的降级方案」。

**这些能力的存在性与可能性**（简要结论，便于讨论）：

| 能力 | 现状 | 说明 |
|------|------|------|
| **Kernel launch kernel** | **已存在** | CUDA Dynamic Parallelism（CDP）：device 上的 kernel 可 launch 子 grid，父 grid 会等子 grid 完成。有额外开销与约束（如栈、设备运行时），但在 sm_3.5+ 上可用，CDP2 为 CUDA 12 默认。可用于「调度 kernel 按需 launch copy 或下一阶段计算」。 |
| **Kernel 触发 copy（GPU 内部）** | **已存在** | kernel 内异步 copy：LDGSTS（global→shared，sm_8.0+）、TMA（Tensor Memory Accelerator）等，由 kernel 发起、copy engine 执行，与计算重叠。但限于 **GPU 内部**（global/shared/distributed），不涉及 system memory 或 SSD。 |
| **Kernel 触发 copy（off-chip：系统内存/SSD→GPU）** | **部分已存在（GDA-KI/IBGDA）** | 标准 CUDA 未暴露通用 API，但 **GPUDirect Async Kernel-Initiated (GDA-KI)** 及 **IBGDA**（InfiniBand GPU Direct Async）已实现「kernel 指令网卡做 transfer」：kernel 在 GPU 内存中提交 WQE（work queue entry），网卡 DMA 引擎取走 WQE 并执行单向 RDMA READ，将结果写回 GPU 可访问地址，从而实现 **system/remote memory → GPU** 的 copy，且**由 kernel 发起、无需 host 参与**。典型栈为 DOCA GPUNetIO + InfiniBand/RoCE 网卡；NIC 与 GPU 需在同一 PCIe 域（GPUDirect RDMA）。因此「kernel 指令网卡进行 system→GPU copy」**已有可能**，可用于 L3/ host 到 GPU 的加载，前提是运行时与网卡支持 GDA-KI/IBGDA。 |
| **Kernel 操作 event** | **受限** | event 的 record/wait 在 CUDA 中主要由 **host** 或 **stream 顺序** 使用；device 端没有标准的 cudaEventRecord。依赖 stream 的隐式顺序或 graph 可部分达到「计算与 copy 的依赖」，但非「kernel 内主动 record/wait event」。部分厂商或扩展可能有 device 侧同步原语，需按具体栈查证。 |

**IBGDA / GDA-KI 与「kernel 指令网卡做 system→GPU copy」**：**可以**。IBGDA（InfiniBand GPU Direct Async）是 GDA-KI（GPUDirect Async Kernel-Initiated）在 Mellanox Connect-X 上的实现，支持 **kernel 在 device 上发起** 网络/RDMA 操作：kernel 在 GPU 内存中填写 WQE（work queue entry），网卡 DMA 引擎取走 WQE，执行单向 RDMA READ（从 system/远端内存读），并将结果写入 GPU 可访问地址，从而完成 **system/remote → GPU** 的 copy，全程无需 CPU 参与。因此「指令网卡进行 system 到 GPU 的 copy」在支持 GDA-KI/IBGDA 的栈（如 DOCA GPUNetIO + 对应网卡与驱动）上**已可实现**，可作为「kernel trigger copy engine」在 off-chip 路径上的一种落地方式，用于 L3/host 到 GPU 的按需加载。

**小结**：**Kernel launch kernel** 已具备；**kernel 内 copy** 仅限 GPU 内部；**off-chip 的 kernel 触发 copy** 在 GDA-KI/IBGDA 下已可实现（kernel 指令网卡做 system→GPU transfer）；**kernel 操作 event** 在标准 API 下较弱。设计上可假设「目标架构支持 device 发起 copy（含 GDA-KI/IBGDA 或等价能力）」，在文档中注明对现有硬件的依赖与降级路径（仅 host trigger + CDP 做有限设备侧调度）。

**能否实现完全以 GPU 为中心的显存管理？** 在上述能力具备的前提下，**可以**。拼图如下：**(1) ref 表在 GPU 显存**，由 device 上的调度 kernel 或 persistent 逻辑维护；(2) **加载**：调度逻辑根据 ref 表与即将执行的算子决定需要哪些 block，由 kernel 通过 **GDA-KI/IBGDA 向网卡提交 WQE**，网卡执行 system/remote→GPU 的 RDMA READ，数据落盘到 GPU，无需 host；(3) **驱逐**：调度逻辑决定某 block 可驱逐时，若仅为「标记可复用、允许被覆盖」则完全在 GPU 侧完成；若需**写回 L3/system**（如 dirty 块），则可由 kernel 通过 GDA-KI 提交 RDMA WRITE 的 WQE，由网卡执行 GPU→system 的写回，同样无需 host；(4) **调度循环**：通过 **kernel launch kernel**（CDP）或 persistent kernel 在计算间隙运行，不断根据 ref 表与 DAG 更新 load/evict 并提交 WQE。这样显存状态、加载与驱逐的**决策与触发**全部在 GPU 侧，host 只提交高层任务或 DAG，不再参与显存调度，两条时间线合一，即**完全 GPU 为中心的显存管理**。前提是运行时与硬件支持 GDA-KI/IBGDA（含 WRITE 方向）、CDP 及 GPU 可访问的 ref 表与 WQE 队列；多流/多卡下的 ref 表一致性与同步需在设计中单独考虑。

**GPU 侧指针的擦除处理**：在完全 GPU 为中心的显存管理下，某 block 被驱逐后，其原先在 GPU 上的物理地址可能被复用或释放；若 GPU 侧仍有**指针**（或等价句柄）指向该地址，会变成悬空指针，产生 use-after-free。因此需要对 **GPU 侧指针做擦除/失效处理**：(1) **不持裸指针**：GPU 上只持 ref ID 或 slot 索引，通过 ref 表解析到当前物理地址；驱逐时只更新 ref 表项（如标记无效或指向新加载位置），所有「指针」实为间接引用，自然随表项更新而一致。(2) **驱逐时显式失效**：若某些 kernel 或结构体持有该 block 的 GPU 地址，驱逐时必须能识别并**擦除**（写无效标记或统一 sentinel），或保证在驱逐前不再被访问。(3) **生命周期约束**：约定「指向某 block 的 GPU 指针仅在该 block 的 pin/live 期内有效」，驱逐前确保没有 kernel 或持久结构仍持有该指针；若难以保证，则必须采用 (1) 的间接引用。设计 ref 表与调度策略时，应明确「指针」的表示（裸地址 vs ref ID）以及驱逐时的擦除/更新协议，避免悬空引用。

**给算子传参时参数的地址：迟 binding**。与上述一致，给算子（kernel）传参时，**参数的地址可以采用迟 binding**：不传最终的 GPU 物理地址，而传 **ref ID 或 slot 索引**；在 kernel 真正使用该参数（或在内核内首次解引用）时，再通过 **ref 表** 查当前绑定到的物理地址。这样 (1) 传参在调度/launch 时即可完成，不依赖 block 是否已加载；(2) 若 block 在 kernel 运行前才由 GDA-KI 加载完成，binding 发生在使用点，拿到的是加载后的地址；(3) 驱逐与复用只更新 ref 表项，传参（ref ID）不变，无需擦除或回写调用方。迟 binding 的时机可以是：launch 前由调度器在 ref 表里解析一次并写入 kernel 参数区；或 kernel 内第一次访问该 ref 时查表并缓存；或由硬件/ABI 支持「间接参数」，执行时再解析。设计时需约定 binding 发生的时机与谁负责解析（host 侧 launch 前 vs device 侧首次访问）。

**CUDA 下是否有参数迟绑定？** CUDA **没有** 提供「参数迟绑定」的专用 API 或语言机制；kernel 参数在 launch 时由 host 写入参数区，传入的是当时的值（含指针即当时地址）。但**用编程方式可以实现**迟绑定：(1) 传参不传指针，而传 **ref ID 或 slot 索引**（整数）；(2) 将 **ref 表** 放在 device 可访问内存（如 `__device__` global 或 constant，或通过 kernel 参数传入表基址）；(3) kernel 内**首次使用**该参数时，用 `ref_table[ref_id]` 或等价索引取当前绑定的指针，再访问。这样「绑定」发生在 kernel 执行时的查表，由应用层完成，而非 CUDA runtime 内置。代价是每次使用需多一次（或缓存后首次）间接访存；若 ref 表在 constant 或 L2 可缓存，开销可控。因此迟绑定在 CUDA 下是**可实现的编程模式**，不是现成 API。

**用 persistent kernel 实现 GPU 上的直接显存管理**。可以。用一个 **persistent kernel**（持久 kernel）常驻在 GPU 上，在循环中直接负责显存管理：(1) 该 kernel 不退出，而是循环从 **工作队列** 或 **ref 表 + DAG 状态** 取任务；(2) 维护 ref 表（在 device global 或该 kernel 可写的共享结构），根据即将执行的算子与当前显存占用决定 **load 集合** 与 **evict 集合**；(3) 对需加载的 ref 通过 **GDA-KI/IBGDA** 向网卡提交 WQE（RDMA READ），对需驱逐的 ref 可写回 L3（提交 RDMA WRITE）或仅标记可复用；(4) 可与计算 kernel 交错：persistent kernel 在「间隙」或独立 stream 上运行，或通过 **CDP** 在适当时机 launch 计算 kernel，计算 kernel 传 ref ID、在内部迟绑定查表访问。这样显存管理的**调度循环**完全在 GPU 上，无需 host 在每次 enqueue 前或回调里做决策；host 只负责把高层 DAG 或任务描述写入 GPU 可访问的队列/结构，由 persistent kernel 消费并驱动 load/evict 与计算。实现时需约定 persistent kernel 与计算 kernel 的同步方式（如 queue、event、或 ref 表上的状态位）以及 ref 表、WQE 队列的并发访问规则。

**Persistent kernel 如何被唤醒？** CUDA 下 **device 端没有「阻塞直到 host 通知」的原语**，kernel 不能 sleep 等 host 或等某个跨 device 的 event。因此通常采用：(1) **持久轮询**：persistent kernel 在循环中不断检查 GPU 可见的**工作队列**或**标志位**（在 device global 或 mapped host 内存）；host 或其它 kernel 向队列 push 新任务或写标志后，persistent kernel 在下一轮循环即可看到并处理，即「唤醒」= 向队列/标志写入，由轮询自然发现。(2) **按需 re-launch**：若希望空闲时少占 SM、省电，可让 persistent kernel 在连续若干轮空闲后 **exit**，host 在提交新 DAG/任务时再 **重新 launch** 该 kernel，相当于「按需持久」；唤醒 = host 提交时 launch。(3) **多 stream**：host 在 stream A 上 enqueue 任务描述到 GPU 可见的队列，persistent kernel 在 stream B 上轮询该队列并执行 load/evict、必要时用 CDP launch 计算 kernel；stream 间通过 queue 与 ref 表状态协调，**host 提交即等效唤醒**（下一轮轮询可见）。若未来硬件支持 device 侧「wait on address/event」或 host 可发起的 device 中断，可考虑真正的阻塞式唤醒；目前以轮询或 re-launch 为主。

**如何借助硬件机制调度？什么情况下会被交换出去？**

**借助硬件的调度**：(1) **Block/warp 级**：GPU 硬件调度器负责把 thread block 放到 SM 上执行，我们只负责 launch kernel，block 级调度由硬件完成；persistent kernel 与计算 kernel 一样，作为 grid 被硬件调度。(2) **显存/页级（UVM）**：若使用 **Unified Virtual Memory / cudaMallocManaged**，GPU 与驱动会在 **page fault**（device 访问未 resident 的页）或 **容量压力** 时自动做页迁移（将某页从 GPU 换出到 host 或反向换入），此时「何时换出」由运行时/驱动按策略（如 LRU、或为新高优先级分配腾挪）决定，应用层不直接控制。(3) **显式 ref 表 + GDA-KI**：我们的方案不依赖 UVM 的自动换页，而是由 persistent kernel 根据 ref 表与 live set 显式决定 load/evict 并提交 WQE；**调度逻辑在软件**，硬件提供 GDA-KI（网卡执行 WQE）与 CDP（kernel launch kernel）。若希望与硬件行为对齐，可让 ref 对应 UVM 的某段地址：换入/换出既可由我们发 GDA-KI，也可在 fault 时由 UVM 驱动，二者可配合（例如我们做粗粒度 block 管理，UVM 做细粒度页迁移）。

**什么情况下会被交换出去？** (1) **在我们显式管理下**：当 persistent kernel 决定**驱逐**某 ref 时，对应 block 被换出——典型条件为：该 ref **不在当前及可预见的 live set**（liveness 分析或 DAG 已不用）、或 **显存容量满** 且按 **LRU/策略** 选出 victim、或 **显式 unpin/释放** 后 ref count 归零。(2) **在 UVM 自动管理下**：当 GPU 显存紧张或发生 page fault 且需为新区腾挪时，驱动/运行时按自身策略（如 LRU）选择若干页换出到 host；应用通常不直接感知「哪一页何时被换出」。(3) **Kernel 自身被「换出」**：若系统支持 GPU 抢占（如 MPS、或研究中的 driver 级 context 调度），更高优先级任务到来时当前 kernel 可能被暂停/换出，SM 资源让给其它 grid；此时 persistent kernel 也会被交换出去，恢复后继续轮询。设计时需约定：我们管理的「换出」仅指 **ref 对应 block 的驱逐**，条件为 live set + 容量 + 策略；若与 UVM 混用，需明确与 UVM 换页的边界（如我们管 block 粒度，UVM 管页粒度，或我们完全接管某段地址范围）。

**Persistent kernel 自身在访存等情况下也会被换出**。是的。Persistent kernel 与其它 kernel 一样，不享有豁免：(1) **其访问的内存**若在 UVM 管理下（如 ref 表、工作队列在 managed 区域），在显存压力下这些页可能被驱动换出到 host；kernel 访存时若触及已换出的页会触发 page fault，再由运行时把页迁回，kernel 会经历延迟甚至多次 fault。(2) **kernel 作为 grid** 在抢占或 MPS 等场景下可能被暂停/换出，SM 资源让给其它任务，恢复后再继续执行。因此 persistent kernel 的实现需考虑：ref 表/队列尽量常驻或 pin 在 GPU（如用 device 专有分配避免被 UVM 换出），或接受访存路径上的 fault 与延迟；若依赖其「常驻」做调度，需注意在抢占恢复后状态一致性与队列不丢。

**原子操作会导致 persistent kernel 被换出吗？** **原子操作本身不会「导致」kernel 被换出**——原子指令由硬件执行，不会触发 UVM 换页或抢占。但有两类关联：(1) **原子作用在 UVM 管理的地址上**：若 ref 表/队列在 managed 区域，该页在显存压力下仍可能被换出；原子完成后、或其它 warp 再访问该地址时可能 page fault，与普通访存一样，并非原子「引起」换出，而是**原子目标所在页**可能被换出。(2) **抢占发生在原子执行期间**：若 GPU 发生抢占，kernel（含正在执行原子的 warp）可能被整体暂停/换出；恢复后原子要么已完整完成要么由硬件保证可恢复，取决于具体架构，但若用原子维护 ref 表/队列一致性，需注意**抢占恢复后**状态是否仍一致（如半更新的表项）。建议：对 ref 表、队列、WQE 等关键结构用 **device 专有分配**（非 managed），减少被 UVM 换页；原子仅用于必要的同步，并约定在抢占恢复后做一致性检查或幂等重试。

本节仅作方案方向记录，具体 ref 的粒度（block / region）、liveness 的算法（如基于 DAG 的 last-use）、与现有 pin/unpin 的兼容方式、以及 ref 表与 device 侧调度器的具体形态等有待细化。

**为何之前没有这一类工作？** 可能原因包括：(1) **能力出现较晚**：GDA-KI/IBGDA、kernel 发起网卡 transfer 等是近年才在特定栈（NVIDIA + Mellanox/DOCA）上成熟并暴露给上层，早年只有 host 下发 copy，device 无法主动拉数，想做「完全 GPU 中心」也缺硬件基础。(2) **传统分工**：GPU 长期被当作「加速器」——host 管调度与数据搬移，GPU 只跑 kernel；驱动与 CUDA 生态也是 host-centric，大家习惯在 host 上做 prefetch/evict，没有强烈动机把整块显存管理迁到 device。(3) **需求未到**：早期显存能装下多数工作集，或 batch/context 较小，host 侧按需 copy 或静态分配就够用；近年 LLM 长 context、大 KV cache、图/推荐等把「显存装不下、需频繁换入换出」推到前台，两条时间线与 host 猜 device 的代价才被放大，GPU 中心管理的收益才明显。(4) **实现门槛高**：完整做 ref 表、liveness、策略、与 GDA-KI/网卡/驱动的对接，需要跨栈深度集成，且依赖特定硬件组合，通用性与可移植性有限，很多团队先选 host 侧方案或依赖框架自带调度。(5) **生态与厂商**：GDA-KI/IBGDA 绑定 NVIDIA + InfiniBand/RoCE，非通用标准，研究和工程多集中在 HPC/超算或大厂内部，未形成广泛可见的「GPU 中心显存管理」范例。综上，并非想法不可行，而是能力、需求与生态在近期才交汇，后续若 LLM/长上下文等继续放大显存压力，这类工作可能会增多。

---

## 1. 当前约定（契约与示例中的形态，尚不足）

### 1.1 用户 API

- **模块级上下文**：`persisting.pin(ref)`，`ref` 为切片引用（当前约定为 `kv[key]` 得到的 **Handler**）。
- **进入 with**：对 `ref` 执行 pin → 通知 block manager 对该切片覆盖的所有 block 做 **per-block 引用计数 +1**，这些 block 在 count > 0 时不被驱逐。
- **退出 with**（正常或异常）：**launch 两个算子**，由这两个算子通知 block manager 对 `ref` 对应的 block 执行 unpin（ref-count -= 1）；unpin 通过这两个算子**异步**完成。

```python
ref = kv["s1", 0, 0:512]
with persisting.pin(ref):
    x = ref.tensor()
    # ... 使用 x ...
# 退出：launch 两个算子 → block manager 对 ref 对应 block 执行 unpin
```

### 1.2 语义要点

- **Ref-count 是 per-block**：多个重叠或嵌套的 `pin(ref)` 会叠加同一 block 的计数；只有 count 归零的 block 才参与驱逐。
- **生命周期由 with 管理**：用户不手写 unpin，避免泄漏或忘记 unpin。

---

## 2. 需要讨论的开放点

### 2.1 「两个算子」的具体含义与动机

当前契约只写“退出时 launch 两个算子，两算子通知 block manager 去 unpin 对应的 block”，没有约定“两个”在语义上代表什么。可能理解包括：

| 理解 | 含义 | 实现影响 |
|------|------|----------|
| **A. 冗余/双路** | 同一套 unpin 请求发两遍（例如双写、容错） | 需要约定是否幂等、是否允许只成功一次 |
| **B. 两阶段** | 第一个算子做某准备（如收集 block 列表、本地状态），第二个算子真正发“unpin”给 block manager | 契约需明确两阶段的职责与顺序 |
| **C. 双角色** | 例如一个算子管“本地 pin 记录释放”，一个管“通知远端/Block Manager” | 单节点时可能退化为一次本地调用 + 一次空或合并 |
| **D. 按 ref 拆分** | 把 `ref` 覆盖的 block 分成两批（如按 layer、按 time 前半/后半），各发一个算子 | 与并行度、负载均衡相关；需约定拆分策略是否对用户可见 |

**建议**：先明确“两个算子”是**实现细节**（例如当前单节点实现就是两次 task 提交）还是**契约语义**（两阶段/双角色必须对外可观测）。若为契约，需要在 §2.7 中简短描述两算子的职责或约束。

---

### 2.2 `ref` 的抽象与类型

- **当前**：`ref` 即 `kv[key]` 的返回值 **Handler**（内部持 `store` + `region`），block 集合可由 `region_to_blocks(region, ...)` 推导。
- **问题**：
  - Handler 是否**唯一**作为 pin 的合法 ref？是否允许“只带 namespace + key 的轻量 ref”（不持有 store 引用）以便跨进程/序列化？
  - **嵌套/重叠**：同一 Handler 被多次 `with persisting.pin(ref):`（如嵌套 with），应视为同一 ref 的多次 pin → ref-count 正确累加、退出时每次退出对应一次（或多次）unpin。若“两个算子”是按“每次 exit 发两个算子”，则每次 exit 发 2 个 unpin 请求，block manager 侧需保证 ref-count 只减 1 次 per exit（即两算子合起来语义上是一次 unpin）。
  - **ref 生命周期**：两算子是异步的，退出 with 时 `ref` 可能已不再被用户持有。需约定：unpin 所需信息（如 block id 列表或 ref 的序列化形态）在 **enter 时**就已交给运行时/block manager，退出时两算子只带“这次 with 对应的一次 unpin”的 token/句柄，而不依赖 `ref` 对象本身仍存活。否则需要 ref 的拷贝或序列化在 enter 时完成。

**建议**：在契约中明确 (1) 合法 `ref` 的类型（目前即 Handler；若未来支持其他类型可扩展）；(2) 多次 pin 同一 ref 的 ref-count 语义；(3) unpin 在异步算子中不依赖 `ref` 对象生命周期（由 enter 时登记的信息驱动）。

---

### 2.3 Block Manager 的边界与单节点/分布式

- **单节点**：block manager 可能只是 BlockStore 内的一块逻辑（或同一进程内的一个组件），“通知 block manager unpin”可以是内存中的方法调用或进程内 task。
- **分布式**：block manager 可能是独立节点/进程（如 Pulsing actor），unpin 需通过 RPC/消息；“两个算子”可能是两条消息或两个异步调用。
- **契约**：当前“launch 两个算子通知 block manager”可以保持为**抽象表述**，不绑定单节点或分布式，实现自行选择。但若“两个算子”有明确语义（见 2.1），需要在契约里用抽象语言描述（例如“一次 unpin 由两次逻辑操作完成”），便于单节点与分布式一致实现。

---

### 2.4 失败与幂等

- **Unpin 失败**：若两个算子之一失败（如网络、block manager 不可用），是否允许“只成功一次”、ref-count 仍正确减 1？若允许，则 unpin 请求需设计为**幂等**（同一 with-exit 的两次通知等价于一次逻辑 unpin）。
- **Pin 失败**：enter 时若 block manager 不可用，是抛异常（with 体不执行）还是允许“尽最大努力 pin”需约定。
- **Ref-count 下溢**：unpin 时若 block 当前 count 已为 0（例如重复 unpin 或 bug），应 clamp 为 0 并可选打日志，避免未定义行为。

**建议**：契约中简短注明“unpin 在 block manager 侧应幂等或等价于单次逻辑 unpin”“ref-count 不应下溢”，具体错误处理与日志为实现细节。

---

### 2.5 是否保留显式 pin/unpin API

- **当前契约**：只暴露 `with persisting.pin(ref):`，没有单独的 `persisting.pin(ref)` / `persisting.unpin(ref)` 或 `kv.pin(key)` / `kv.unpin(key)`。
- **若只提供 with**：生命周期清晰，不易误用；但无法表达“先 pin 后在某处异步 unpin”或与外部调度器配合。
- **若额外提供显式 pin/unpin**：更灵活，但需约定 ref-count 与 with 的配合（例如 with 内部再调一次 pin 则 count+1，退出时 with 的 unpin 只减 1）。

**建议**：首版可仅提供 `with persisting.pin(ref)`；若后续有明确需求再增加显式 `pin(ref)` / `unpin(ref)` 并写清与 with 的 ref-count 一致性与使用场景。

---

### 2.6 与 prefetch / wait 的配合

- Pin 只保证“这些 block 不被驱逐”；不保证 block 已在 L1。若用户希望“在 with 内一定从 L1 读”，需要先 prefetch 再 pin，或约定“pin 时若 block 不在 L1 则拉入 L1”（即 pin 隐含 prefetch）。
- **建议**：在文档中注明“pin 不保证 block 已在 L1；若需确保在 L1，请在 with 内或进入前 prefetch/wait”，避免误解。

---

### 2.7 异步执行与 GPU stream：unpin 时机绑定到「使用方执行完成」

**问题**：若在 GPU stream 上异步执行（例如把 `ref` 物化到 GPU 后 launch  kernel），Python 的 `with` 退出时 GPU 可能**尚未**用完这些 block；若此时就 unpin，block manager 可能驱逐仍在被 GPU 使用的数据，导致错误。因此 unpin 必须在**真正使用这些 block 的 GPU 工作完成之后**才能发生。

**期望语义**：

- **同步路径**：无 stream / 纯 CPU 使用时，退出 `with` 时安排 unpin（例如当前说的“两个算子”或一次异步 unpin）。
- **异步路径（GPU stream）**：用户传入一个 **stream**（或等价“完成事件”），表示“使用 ref 的算子在 stream 上执行”。退出 `with` 时**不立即 unpin**，而是在该 stream 上**挂一个「在 stream 上执行的算子」**，该算子在本轮 GPU 工作（使用 ref 的 kernel）**之后**执行，执行时通知 block manager 对 ref 对应 block 做 unpin。即：**由 GPU 上的算子执行完后，通过该算子（或由其触发的回调）直接 unpin 对应的 block**。

这样：

1. Pin 的时长覆盖整个“CPU 提交 + GPU 执行”直到“使用 ref 的 GPU 工作完成”。
2. Unpin 由**执行端**（GPU stream 的完成）驱动，而不是由 Python 控制流驱动，避免过早释放。

**API 形态（讨论）**：

- **方案 A**：`persisting.pin(ref, after=stream)` 或 `persisting.pin(ref, stream=stream)`  
  - 进入 with：pin(ref)。  
  - 退出 with：不立即 unpin；向 `stream` 注册一个“在 stream 当前工作之后执行的算子”，该算子完成后通知 block manager unpin。  
  - 若无 `stream`，则退化为“退出 with 时异步 unpin”（同步/CPU 语义）。

- **方案 B**：`persisting.pin(ref)` 不变，由用户在 with 内自行在 stream 上 enqueue 一个“unpin 算子”或 callback，API 只提供 `persisting.unpin_after(ref, stream)` 或 `persisting.schedule_unpin(ref, stream)`，在 with 体内调用。  
  - 缺点：生命周期容易用错（忘记调用或 stream 传错）。

- **方案 C**：with 只负责 pin；unpin 完全由“用户传入的 completion”驱动，例如 `persisting.pin(ref, unpin_when=stream)`，语义同方案 A。

**实现要点**：

- “GPU 上的算子”可以是：在 stream 上 enqueue 的一个 no-op kernel 或 record 一个 event，再在 host 侧用该 event 触发“通知 block manager unpin”；或某些后端支持在 GPU 上执行的回调（如 CUDA graph callback）直接调 host 的 unpin。  
- Unpin 所需信息（block 列表或 ref 的 token）须在 **enter 时**就确定并交给运行时，以便在 stream 完成时异步执行 unpin，不依赖 `ref` 对象仍存活。

**建议**：契约中明确 (1) 支持“unpin 绑定到用户指定的 stream/完成事件”；(2) 若提供 `stream`（或等价参数），则退出 with 时不立即 unpin，而是由「在该 stream 上、于使用 ref 的算子之后执行的算子/回调」负责通知 block manager unpin；(3) 无 stream 时行为与当前一致（退出 with 时安排 unpin）。设计讨论中采用方案 A 风格的 `after=stream` 作为讨论基准。

---

### 2.8 生命周期的自动管理：多层引用与垃圾回收

**目标**：block 何时可驱逐应由**生命周期自动管理**决定，而不是由用户或单次「unpin」决定。只要任意一层还有引用（Handler、物化出的 GPU tensor、numpy 数组、stream 上的使用等），block 就不能被驱逐；当各层通过自己的生命周期结束或 GC 释放了引用后，block 才可驱逐。实现这一点的机制就是**多层引用 + 各层在释放/GC 时自动反馈**。

**多层**示例：

| 层 | 持有者 | 何时释放（自动） |
|----|--------|------------------|
| with / pin | `persisting.pin(ref)` 的 with 或 stream 回调 | with 退出、或 stream 上「使用 ref 的算子」完成时 |
| 物化结果 | `ref.tensor()` 得到的 GPU tensor / numpy 等 | 对象析构、或该后端对应的 GC/ref-count 归零时 |
| 其它 Handler / view | 同一 block 的其它 kv 切片 | Handler 不再被持有时 |

每一层在「释放」时都对 block 做一次「hold 减 1」（或等价）；block 仅在**所有层的 hold 都归零**后才可驱逐。这样用户无需关心「unpin 时还有没有引用」——因为 unpin 只是某一层的释放，其它层的引用会继续占着 hold，直到它们也被释放或回收。

**实现形态（选一或组合）**：

1. **统一 ref-count**：所有「使用该 block」的持有者（with、GPU tensor、buffer 等）共用一个 ref-count；任一持有者创建时 +1，释放时 -1；归零即可驱逐。需要运行时登记「tensor / buffer ↔ block」依赖。
2. **多层 hold（pin_count + use_count 等）**：pin 层（with/stream）管 pin_count；派生对象（tensor、buffer）管 use_count；可驱逐当且仅当 pin_count == 0 且 use_count == 0。每层在自己的生命周期结束时自动减自己的计数。
3. **用户保证 + 单层 pin**：仅实现 with/stream 的 pin 一层；约定用户在使用期内不长期持有派生 tensor，或由上层框架保证。实现简单，但生命周期自动性弱。

**建议（讨论用）**：契约规定「block 仅在无任何引用时方可驱逐」；实现通过**多层引用与在各层释放/GC 时自动减 hold** 达成生命周期自动管理；不暴露「先 unpin 还是先释放引用」给用户，用户只写 with 或绑定 stream，其余由运行时和 GC 负责。

---

## 3. 建议的下一步（补齐不足、向目标演进）

1. **承认当前方案不足**：在契约或设计总览中注明「§2.7 与本节为设计方向与讨论基准，实现尚未满足两条时间线对齐与设备侧决策」。
2. **设备侧决策能力**：定义「设备可发起/可参与」的接口或协议（例如 device 回调、stream 上算子可调用的 block manager API、或设备侧策略与 block manager 的协作），使预取与驱逐能由设备时间线驱动，而不只由 host 单边下发。
3. **两条时间线在实现层对齐**：block manager 的输入除 host 的 pin/unpin 外，应包含设备时间线上的完成事件或设备侧触发的释放；避免仅靠 host 推断 device 何时用完。
4. **生命周期自动管理落地**：实现多层引用登记（如物化 GPU tensor 时对 block 的 hold），在各层释放/GC 时自动减 hold；block 仅在所有层释放后可驱逐。
5. **其它**：明确「两个算子」语义（若保留）、ref 与 unpin 不依赖 ref 存活、unpin 幂等与 ref-count 下溢处理；再在 `tiered_storage_implementation_steps.md` 中拆解为可执行步骤。

---

## 4. 小结表（讨论用）

| 主题 | 当前契约/假设 | 待决问题 / 缺口 |
|------|----------------|------------------|
| **方案状态** | **讨论中的形态，尚不足** | **设备未真正决策；两条时间线未在实现层对齐；多层引用未落地；预取/驱逐未形成设备侧闭环。需重新设计或演进。** |
| 两个算子 | 退出时 launch 两个算子通知 block manager unpin | 两算子的语义？是否保留？ |
| ref 类型 | Handler（kv[key]） | 是否允许其他 ref；unpin 不依赖 ref 存活 |
| 异步/GPU | after=stream 把 unpin 绑到 stream 完成 | 仍为 host 监听 stream；设备侧未主动发起/决策 |
| 生命周期自动管理 | 目标已写，未落地 | 物化 tensor 等对 block 的 hold 未登记；需多层释放自动减 hold |
| 设备侧决策 | 目标已写，未实现 | 预取/驱逐由 host 发起；需设备可发起或参与决策的接口与协议 |
| 失败与幂等 | 未写 | unpin 幂等；ref-count 下溢 |

讨论定稿并补齐上述缺口后，再将结论同步到 `llms.binding.md` §2.7 与实现。
