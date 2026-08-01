# pPilot 架构

> 正式名称：**pPilot — Durable Run Orchestrator**
>
> 状态：**Phase-1 已落地；目标架构部分对齐**
>
> 代码：`crates/persisting-ppilot`（library + 独立 `ppilot` CLI）
>
> 使用方式：`persisting batch/query`、`ppilot run/query/self-test` 或 Rust 库 API

pPilot 负责计划、调度、恢复和收成许多独立 Run。当前 Phase-1 以
`plan()` + `execute(item)` 提供 map 式批量编排；目标形态是只操作
`RunSpec`、`RunFuture` 和 `RunCommit`，将单 Run 执行交给 pVisor，将运行事实
交给 pChronicle。

面向用户的统一入口是 `persisting batch/query`；独立 `ppilot` binary 用于组件部署、
调试与自检。pPilot 拥有编排和分析交互，pChronicle 仍独占轨迹 schema、物理存储、
DataFusion datasource 与查询执行。

**不是** Ray，**不**定义 Agent DSL，**不**自研替代 `torchrun` 的启动器。

---

## 1. 定位

```text
用户脚本 / manifest     plan() 产出稳定 item
         ↓
pPilot                  计划 · 有界并发 · lease · retry · resume · collect
         ↓
当前：Worker + Python host          目标：RunSpec → pVisor → RunFuture
         ↓
当前：JSONL + optional Lance        目标：RunCommit → pChronicle
```

| 是 | 不是 |
|----|------|
| 独立任务的 map 式编排 | 分布式训练框架 |
| 编排独立 Run / task | 新 DSL / 装饰器框架 |
| 真实 `torchrun` 多进程 | 自研 rdzv / launcher |
| Rust Pulsing 做发现与投递 | Python 侧 Actor 编程模型 |
| 通过 provider 扩展执行位置 | 定义 Agent 会话格式或物理存储 |

`persisting` 对 batch/query 做薄转发，不让 pPilot 经过 Engine RON ABI。
`ppilot query` 直接调用 pChronicle library；落盘若走 Lance，由 sink adapter 写入
pChronicle；编排状态仍由本 crate 管理。

### 1.1 公共 CLI

```text
ppilot run <SCRIPT> [OPTIONS]
ppilot query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif]
ppilot self-test [OPTIONS]
```

`run` 收纳原有 `PPilotArgs`；`self-test` 是无需用户脚本的环境与执行链路验证。
`query` 对三表 Storyline Lance store、ATIF JSON/数组/JSONL/目录注册同名的 `runs`、
`steps`、`tool_calls` 表，执行一条只读 DataFusion SQL，并把 JSONL 写到 stdout。

CLI 通过 `cli` feature 构建；默认 library feature 仍为空，避免只嵌入调度器的应用
无条件编译 Lance/DataFusion：

```bash
cargo build -p persisting-ppilot --features cli --bin ppilot
```

---

## 2. 架构判断

当前实现的方向基本合理，但它是 pPilot 的可用起点，不是目标架构的完成态。

| 设计要求 | 当前状态 | 判断 |
|---|---|---|
| 稳定 task / Run identity | manifest item 强制稳定 `id`；adapter 生成确定性 `run_id` 并写入 TaskResult | **已对齐** |
| 有界并发与反压 | `max_inflight` + sink queue backpressure | **已对齐** |
| 基础设施重试与语义重试分离 | `--retries` 只处理 ask/worker 故障 | **已对齐** |
| 唯一结果收成路径 | Driver 经异步 sink 写 terminal result | **基本对齐**；尚无 RunCommit CAS |
| durable checkpoint | JSONL + `checkpoint.json` | **部分对齐**；不是 Job/Run/Attempt 状态存储 |
| lease 与 stale worker fencing | sticky/quarantine，缺少持久 lease epoch | **部分对齐** |
| pPilot 只操作 RunFuture | Worker 将 TaskExpr 转成 RunSpec，Python host 实现 pVisor RunExecutor | **部分对齐**；Driver transport 仍是 Worker ask |
| reconcile | resume 只扫描 JSONL，未核对活跃 Attempt 与 pChronicle | **缺失** |
| pChronicle terminal facts | 可选 Tee Lance 保存 `ppilot.result/failure` | **过渡实现**；需改为 canonical Event + RunCommit |

因此重构顺序不是推翻现有调度器，而是：

1. 将当前 Worker 内的 `TaskExpr ↔ RunSpec/RunResult` adapter 提升为 Driver 可观察的 RunFuture。
2. 将 checkpoint 扩展为 Job/Task/Run/Attempt + lease epoch 的持久状态。
3. 增加 reconciler，对照 checkpoint、pVisor Attempt 和 pChronicle RunCommit 收敛。

---

## 3. 当前 Phase-1 全景

```text
                 plan.py
            （spawn python · bootstrap）
                       │ NDJSON 任务流
                       ▼
              Driver（rank0 / 本地）
           plan emit + 有界 inflight
           least-loaded / sticky ask
          ┌────────────┼────────────┐
          ▼            ▼            ▼
     WorkerActor 槽位（workers × per-worker）
          │  每槽：独立 Python host
          └──── stdin/stdout JSON ────┘
                       │
                       ▼
              plan.py::execute(item)
                       │
                       ▼
         ResultSink（--sink JSONL · 可选 --traj）
```

要点：

- **Driver 不是 Pulsing Actor**：串行领取 plan、并行 `ask` worker，用 `FuturesUnordered` 有界 drain。
- **Pulsing** 负责命名解析、mailbox、Supervision、本地 DeathWatch；**不做**业务调度。
- **任务粒度**由用户决定：适合分片 / 文件级；不适合把每一行都打成控制面任务。

---

## 4. 用户合约（算法面）

内部 Python workload 合约只有一个文件、两个函数：

1. **`plan()`**（或常量 `PLAN`）流式产出可 JSON 序列化的 object；每项必须包含稳定、非空的字符串或数字 `id`。系统不生成随机 ID，因为同一逻辑任务跨运行必须保持相同 ID，才能可靠去重和续跑。
2. **`execute(item)`** 收到与 yield **同形**的 dict（`{id, …fields}`）。
3. **argv 一致**：嵌入方传入的 plan 参数与 Python 直接执行时保持一致。
4. **扩规模不改文件**：只换 `-w` / `--per-worker` / `torchrun`。

内部控制面会把平面 JSON 归一成 `TaskExpr`（`id` / `op=execute` / `args` / `meta`）；产品面 **只支持** `op=execute`。新能力写在用户 `execute` 里，不靠扩展 op 表。

结果线格式为 `TaskResult`：`task_id`、`run_id`、`ok`、`cancelled`、`value` / `error`、`worker`、时间戳、`infra_retries` 等。

算法脚手架扩展保持在这个边界内：`setup_worker(context)` 与
`teardown_worker()` 为可选的 process-local hook，`execute(item)` 仍是唯一必需且
无状态的计算入口。worker context 通过 Python 的 `persisting_ppilot.context()`
读取，不作为 `execute` 参数传递。`TaskResult` 还可投影用户返回 object 中的
`metrics`（数值）与 `artifacts`（大结果引用）；它们不改变原始 `value`。

---

## 5. 运行时拓扑

### 5.1 本地 `-w N`

单进程：`N × per_worker` 个槽位 Actor；本进程即 Driver。

### 5.2 torchrun

| Rank | 角色 |
|------|------|
| 0 | Driver（plan + dispatch）+ 本机槽位 |
| >0 | 本机槽位，等 `Shutdown` |

- 读 `RANK` / `WORLD_SIZE` / `MASTER_ADDR` / `MASTER_PORT`（及可选 `LOCAL_RANK`）。
- Pulsing 种子端口默认 `MASTER_PORT+17`，可用 `PERSISTING_PULSING_PORT` 覆盖，避免与 c10d 冲突。
- 控制面**不**自己拉齐进程；由 torchrun 负责。

### 5.3 槽位命名

池按 **slot-major** 展平：先各 worker 的 slot0，再 slot1…  
扁平下标：`slot * n_workers + worker`（DeathWatch / quarantine 必须用这套下标，不能用本 rank 局部序数）。

---

## 6. 调度与执行

### 6.1 Driver 循环

1. 从 `plan()` 流式读任务（NDJSON）。
2. 全局 `max_inflight`（默认 `workers × per_worker`）限制在途量。
3. 派发前 **SkipSet claim** `task_id`（resume 种子 + 本 job 已派发）；同 id 不二次派发。
4. 首触：**least-loaded** 选槽；对该槽发起过 `Execute` 之后 → **sticky-only**（只打同一槽）。
5. 完成即 `on_result`，并 `await` 异步 sink enqueue（队列满则背压 Driver）。

### 6.2 sticky-only 与 quarantine

| 情况 | 行为 |
|------|------|
| 尚未接触任何槽 | least-loaded 选槽 |
| 已 ask 过某槽 | infra 重试 **只**打该槽 |
| 该槽被 quarantine | **拒绝**改投他槽，任务记 infra 失败（避免跨槽 at-least-once 重跑 `execute`） |
| 全槽 quarantine | acquire fail-fast，不再死等 |

同槽 **ResultCache**（`task_id → TaskResult`）跨 Supervision 重启共享：丢 reply 时可幂等取回，不重跑 `execute`。

`--retries` 只覆盖 **worker ask / 基础设施**失败；**不**解读业务
`ok=false`。质量重试、重新采样和策略变化必须显式创建派生 Run。

### 6.3 pVisor Python-host provider

每槽一个长驻 Python 子进程（行协议 JSON）：

| cmd | 行为 |
|-----|------|
| `run_plan` | 加载脚本（带 argv），调 `execute(item)`；按路径缓存模块 |
| `shutdown` | 退出 host |

Worker **不**解释业务；Driver **不**碰 Python。每个 Task 先转换为稳定 RunSpec，长驻
Python host 实现 pVisor `RunExecutor`；取消和终态经 RunHandle/RunResult 返回，再由唯一
adapter 转为 TaskResult。provider 代码目前仍位于 pPilot crate，后续可独立成部署 provider。

### 6.4 取消

- Ctrl-C → job `CancellationToken`：停接新任务；在途 acquire / ask 可取消。
- 在途 execute：**kill Python host**，返回 `cancelled`（下一任务会重新拉起 host）。
- 跨 rank：每 rank 独立 `JobControlActor`（与 Worker **分邮箱**），rank0 广播 `Cancel`，避免被串行 `Execute` 堵住。
- 已成功的结果**不会**被改写成 `cancelled`。

---

## 7. 耐久、幂等与续跑

### 7.1 唯一 sink

只有控制面写就绪结果；Executor **禁止**直写存储账本。

| 路径 | 作用 |
|------|------|
| `--sink DIR` | `ready.ndjson` + `failures.ndjson` + `checkpoint.json` |
| 无 `--sink` | 默认仅 stdout NDJSON（开发视图，非耐久） |
| `--traj` | Tee 到 Lance；**JSONL 仍是 resume 真相** |

`JsonlFileSink`：打开时从已有文件 **seed** `task_id`；先 reserve 再写盘，失败则 **rollback seen**（seen ⊆ durable）。  
异步 `sink_writer`：`join` 汇总 persist 失败并 fail job；失败时 `SkipSet::remove`，便于后续 `--resume` 发现未落盘 id。损坏 / 无 `task_id` 的 JSONL 行：**skip + warn**，不拖垮整本账本。

### 7.2 Checkpoint / `--resume`

- 账本 = sink 下已终端的 `task_id`（ready ∪ failures）。
- `--resume`：Driver 跳过这些 id；**`plan()` 仍会再 emit 一遍**（大 plan 的成本是已知限制）。
- 失败 / 取消默认也不重跑；要重跑请编辑 failures 或换目录。

### 7.3 两类重试（边界）

| 类型 | 管什么 | Phase-1 |
|----|--------|---------|
| 基础设施重试 | 已有 Run/Attempt 是否因 ask、worker 或节点故障丢失 | `--retries` + sticky + quarantine |
| 语义重试 | 是否因质量、采样或策略变化创建新 Run | 未做；由 workload policy 显式触发 |

### 7.4 执行与恢复语义契约

pPilot 不承诺 exactly-once。用户应根据下表判断 `execute()` 是否需要使用
`task_id` 在外部系统中实现幂等：

| 场景 | 保证与行为 |
|------|------------|
| worker reply 丢失、原 slot 仍可用 | 只在同一 slot 重试；优先从 slot `ResultCache` 返回结果，不跨 slot 重跑 |
| 已接触的 slot 被 quarantine / 永久失联 | 拒绝跨 slot 重跑，任务终止为 infra failure |
| `execute()` 返回业务错误 | 记录 failure；`--retries` 不重跑业务错误 |
| Driver / rank0 在结果提交前崩溃 | 该任务处于不确定状态；全作业重启后可能再次执行 |
| terminal result 已写入 JSONL | `--resume` 按稳定 `task_id` 跳过，不再派发 |
| JSONL append 返回成功但机器随后掉电 | 当前仅 `flush`、未逐条 `fsync`；不能视为断电级 durability |
| 用户取消在途任务 | kill 对应 Python host 并记录 cancelled；外部副作用是否已发生不可由控制面回滚 |

因此当前语义可概括为：

- **同一作业内的 infra retry**：sticky、偏向避免重复执行；
- **整个作业崩溃后 resume**：以已提交 JSONL 为界，未提交任务可能 at-least-once；
- **外部副作用**：由用户以稳定 `task_id` 作为 idempotency key。

---

## 8. 模块地图

| 模块 | 职责 |
|------|------|
| `cli` | 参数 → fleet / sink / Ctrl-C |
| `runtime` | local / torchrun 装配（共享 spawn / driver loop） |
| `driver` | plan 流 + 有界 drain + skip + sticky 派发 |
| `scheduler` | least-loaded · sticky · quarantine |
| `worker` | `Execute` / `Shutdown` · ResultCache · Supervision |
| `executor` / `plan` | Python host · plan NDJSON bootstrap |
| `dist` | torchrun 环境 · slot 命名 / 扁平下标 |
| `job_control` | 旁路 cancel · 本地 DeathWatch |
| `sink` / `sink_writer` / `checkpoint` / `skip` | 唯一落盘 · 异步 persist · resume 账本 · live claim |
| `observe` | 可选进度事件 |
| `task` | `TaskExpr` / `TaskResult` 线格式 |

多数模块 `pub(crate)`；公开面主要服务 CLI 与集成测。

语义原语 id 见 `blocks.rs`。合约测与源码同文件；跨模块行为在 `tests/integration_*.rs`。

---

## 9. 非目标与下一阶段

### 刻意不做（保持薄）

- 多 op 产品面、独立 Python 编排包、内嵌用户解释器
- 自研进程启动器替代 torchrun
- 在 pPilot 堆 harness / 判分 / Agent 会话
- Meta 语义重试、模板 fan-out、配额、亲和调度

### 已知差距

| 项 | 说明 |
|----|------|
| 昂贵 plan 无 cursor | resume 仍全量再 emit |
| 真 torchrun CI | 有双 ActorSystem 烟测；真实多进程 e2e 仍薄 |
| 错误分类 | infra / execute / cancel 尚未成稳定枚举字段 |
| DeathWatch | 仅本地；远端死槽靠 ask 失败路径 |
| Run contract | adapter 已落地；Driver 尚不能直接观察 RunHandle/Attempt 状态 |
| pVisor boundary | Python host 已实现 RunExecutor，但 provider 代码仍在 pPilot crate |
| reconcile | 尚无 checkpoint / active Attempt / RunCommit 三方收敛 |
| terminal commit | JSONL 是 resume truth；尚无 pChronicle CAS |

### 设计原则（摘要）

1. 算法入口极轻：两个函数；本地 `for` 循环即真。  
2. 扩规模换 deployment，不改 workload 文件。
3. Driver ≠ Actor；Pulsing ≠ 调度器。  
4. 跨槽拒绝优先于跨槽重跑。  
5. persist 失败必须可见；JSONL 是 resume 真相。  
6. 新产品能力优先进 `execute`，不膨胀 CLI。

---

## 10. 相关文档

- [Agent 基础设施](agent-infrastructure.md)
- [`ppilot` 命令参考](cli-ppilot.md)
- [轨迹存储](trajectory.md)
