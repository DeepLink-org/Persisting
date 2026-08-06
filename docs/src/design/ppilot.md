# pPilot 架构

> 正式名称：**pPilot — Durable Run Orchestrator**
>
> 状态：**Phase-1 已落地；lease epoch、reconciler、RunCommit CAS 已对齐**
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
durable result journal → RunCommit CAS → JSONL / optional Lance
```

| 是 | 不是 |
|----|------|
| 独立任务的 map 式编排 | 分布式训练框架 |
| 编排独立 Run / task | 新 DSL / 装饰器框架 |
| 真实 `torchrun` 多进程 | 自研 rdzv / launcher |
| Rust Pulsing 做发现与投递 | Python 侧 Actor 编程模型 |
| 通过 provider 扩展执行位置 | 定义 Agent 会话格式或物理存储 |

`persisting` 对 batch/query 做薄转发；pPilot 直接调用 pChronicle。
`ppilot query` 直接调用 pChronicle library；落盘若走 Lance，由 sink adapter 写入
pChronicle；编排状态仍由本 crate 管理。

### 1.1 公共 CLI

```text
ppilot run <SCRIPT> [OPTIONS]
ppilot chronicle import <INPUT> <STORE>
ppilot query <INPUT> (--sql <SQL> | --sql-file <FILE|->) [--source auto|lance|atif] [--table NAME=FORMAT:PATH]...
ppilot produce <PLANNER.py> --output <DIR> [--parallelism N] [-- <PLANNER_ARGS>...]
ppilot analysis <INPUT> [--output <DIR>] [--fmt jsonl|json|toml] [--parallelism N] (--sql <SQL> | --sql-file <FILE>)
ppilot process <INPUT> (--script <FILE> | --count <METRIC>) [--mappers N] [--output <DIR>]
ppilot self-test [OPTIONS]
```

`run` and `produce` embed a job-scoped Supervisor automatically. pPilot passes
its bootstrap through `RunSpec`; users do not start a separate Supervisor
daemon. `--cluster-network-limit RATE` issues an initial local OverlayNet quota
to every pVisor launched by `produce`; the configured job rate is divided by
the maximum concurrent Run count, so fixed shares cannot exceed the aggregate
but may leave idle bandwidth unused. The long-lived Python host behind `run`
does not yet expose this option. The current torchrun implementation embeds one
Supervisor per rank, so supervision is rank-local rather than one strict
cross-rank token ledger.

`run` 收纳原有 `PPilotArgs`；`self-test` 是无需用户脚本的环境与执行链路验证。
`chronicle import` 将 ATIF JSON、数组、JSONL/NDJSON 或目录通过 pChronicle 规范化后，
按 `session_id` 原子写入本地或对象存储 Storyline Lance store。
`query` 对三表 Storyline Lance store、ATIF JSON/数组/JSONL/目录注册同名的 `runs`、
`steps`、`tool_calls` 表；可通过可重复的 `--table NAME=FORMAT:PATH` 注册 CSV、JSON
对象数组或 JSONL 外部表，随后执行一条只读 DataFusion SQL，并把 JSONL 写到 stdout。

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
| 唯一结果收成路径 | durable result journal → pChronicle RunCommit CAS → sink | **已对齐** |
| durable checkpoint | result journal + Run control record；JSONL/checkpoint 是用户视图 | **基本对齐** |
| lease 与 stale worker fencing | 持久 epoch 贯穿 Driver/Worker/pVisor/TaskResult；在途 heartbeat 续租；CAS 拒绝旧 epoch | **已对齐** |
| pPilot 只操作 RunFuture | Worker 将 TaskExpr 转成 RunSpec，Python host 实现 pVisor RunExecutor | **部分对齐**；Driver transport 仍是 Worker ask |
| reconcile | 启动时核对 result journal、pChronicle Attempt registry 与 Run control record | **已对齐**；active/pending Attempt 会进入 defer 集合，terminal RunResult 可恢复提交 |
| pChronicle terminal facts | 每个 Run 只有一个 CAS terminal RunCommit | **已对齐**；Event 高水位字段已预留 |

下一步不需要推翻现有调度器：重点是让 Driver 直接观察远端 pVisor RunFuture，
并把当前“嵌入式 Worker 创建 pVisor”的 placement provider 替换为可独立部署的远端 fleet。

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

结果线格式为 `TaskResult`：`task_id`、`run_id`、`attempt_id`、`lease_epoch`、
`ok`、`cancelled`、`value` / `error`、`worker`、时间戳、`infra_retries` 等。

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

同槽 **ResultCache**（`(task_id, lease_epoch) → TaskResult`）跨 Supervision 重启共享：
同一 ownership generation 丢 reply 时可幂等取回；新 epoch 绝不读取旧 owner 的结果。

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

每个 Task Run 同时启动长驻 `PilotRuntimeBridge`：按 pVisor 下发的间隔 heartbeat，
注册 worker 进程，监听 Shutdown/Quiesce，并把 Task 包装为带稳定 idempotency key 的
`ppilot.task` effect。Quiesce 到达后停止接收新 effect；当前 Python 调用完成、effect
journal 清空并进入 Idle 后才确认 checkpoint。pVisor 在确认后建立逻辑 OverlayFS 快照，
再通过 Continue 恢复 pPilot。

RunSpec 使用 `parent_run_id` 表达 Job/Batch → Task Run 层级；pVisor 的 RunRecord 与
Run Bundle 持久化 `parent_run_id`、`task_id` 和经过筛选的 `ppilot.*` 编排元数据。

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
| `--sink DIR` | durable result journal + Run control + `ready.ndjson` / `failures.ndjson` / `checkpoint.json` |
| `--control-uri URI` | 将 Run control offload 到 pChronicle 支持的本地或对象存储（需同时给 `--sink`） |
| 无 `--sink` | 默认仅 stdout NDJSON（开发视图，非耐久） |
| `--traj` | RunCommit 成功后 Tee 到 Lance；RunCommit 是 terminal truth |

提交顺序固定为：`{sink}/.ppilot-state/results` 原子写 result → pChronicle 对
`(run_id, attempt_id, lease_epoch, digest)` 做 RunCommit CAS → 用户 sink。旧 epoch、不同
attempt 或不同 digest 不能覆盖已提交结果。相同请求重放返回 AlreadyCommitted。

`JsonlFileSink` 仍按 `task_id` 幂等；异步 `sink_writer` 的 `join` 汇总错误并 fail job。
启动 reconciler 会补齐“result 已 stage、commit 未写”和“commit 已写、sink 未 append”两个崩溃窗口。

### 7.2 Checkpoint / `--resume`

- terminal truth = pChronicle RunCommit；本地 result journal 保留可重放的完整 TaskResult。
- JSONL ledger 继续提供用户可读的 ready/failure 视图和旧版本兼容。
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
| Driver / rank0 在 result stage 后、RunCommit 前崩溃 | reconciler 用 journal 重放 CAS，不重新执行 |
| RunCommit 后、用户 sink append 前崩溃 | reconciler 重放幂等 sink append |
| 旧 worker 晚到 | `lease_epoch` fencing 拒绝旧结果；不会覆盖 canonical commit |
| 未提交 lease 且 pVisor attempt 不存在 | reconciler 标记 retry，下一次派发显式 takeover 并递增 epoch |
| 未提交 lease 仍有效但 Attempt 尚未注册 | reconciler defer 该 task，不猜测 orphan、不重新派发 |
| pPilot 重启时 Attempt 心跳仍有效 | 从 pChronicle registry 识别为 active 并加入 SkipSet |
| pPilot 重启时 registry 已有 terminal RunResult | 恢复 TaskResult，完成 RunCommit CAS 与幂等 sink append |
| terminal RunCommit 已存在 | 稳定 `task_id` 加入 SkipSet，不再派发 |
| JSONL append 返回成功但机器随后掉电 | 当前仅 `flush`、未逐条 `fsync`；不能视为断电级 durability |
| 用户取消在途任务 | kill 对应 Python host 并记录 cancelled；外部副作用是否已发生不可由控制面回滚 |

因此当前语义可概括为：

- **同一作业内的 infra retry**：sticky、偏向避免重复执行；
- **整个作业崩溃后 resume**：已 stage/commit 的终态不会重新执行；完全没有终态 payload 的 orphan attempt 会以新 epoch 重试；
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
| `coordination` | lease/takeover · durable result journal · RunCommit · reconcile |
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
| DeathWatch | 仅本地；远端死槽靠 ask 失败路径 |
| Run contract | adapter 已落地；Driver 尚不能直接观察 RunHandle/Attempt 状态 |
| pVisor boundary | Python host 已实现 RunExecutor，但 provider 代码仍在 pPilot crate |
| reconcile provider | pChronicle Attempt registry、心跳、终态恢复已接入；当前默认 placement 仍是嵌入式 pVisor，独立远端 fleet 尚未提供 |

### 设计原则（摘要）

1. 算法入口极轻：两个函数；本地 `for` 循环即真。  
2. 扩规模换 deployment，不改 workload 文件。
3. Driver ≠ Actor；Pulsing ≠ 调度器。  
4. 跨槽拒绝优先于跨槽重跑。  
5. persist 失败必须可见；RunCommit 是 terminal truth，JSONL 是用户视图。
6. 新产品能力优先进 `execute`，不膨胀 CLI。

---

## 10. 相关文档

- [Agent 基础设施](agent-infrastructure.md)
- [`ppilot` 命令参考](cli-ppilot.md)
- [轨迹存储](trajectory.md)
