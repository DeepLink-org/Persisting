# Compute 架构

> 状态：**Phase-1 已落地**  
> 代码：`crates/persisting-compute` · CLI：`persisting compute`  
> 用法：[Compute 快速上手](../guide/compute_quickstart.zh.md) · 示例：`examples/compute/`

薄编排层：算法写一个 Python 文件（`plan` + `execute`），控制面负责流式派发、并行执行、落盘与续跑。  
**不是** Ray，**不**嵌用户解释器，**不**自研替代 `torchrun` 的启动器。

---

## 1. 定位

```text
用户脚本          plan() 产出任务 · execute(item) 做计算
     ↓
L2 Compute        流式领取 · 有界并发 · sticky 派发 · 取消 / 重试
     ↓
L3 执行缝         每槽一个长驻 Python host，只调用户 execute
     ↓
唯一 sink         控制面 append 结果（JSONL 账本；可选 Tee Vortex）
```

| 是 | 不是 |
|----|------|
| 独立任务的 map 式编排 | 分布式训练框架 |
| 调用用户 `execute(item)` | 新 DSL / 装饰器框架 |
| 真实 `torchrun` 多进程 | 自研 rdzv / launcher |
| Rust Pulsing 做发现与投递 | Python 侧 Actor 编程模型 |
| spawn 外部 `--python` | PyO3 内嵌用户环境 |

与 `search` / `traj` 不同：`compute` **静态链接**进 CLI，直接 async 跑，不经 engine RON ABI。落盘若走 Vortex，再经 sink adapter 写轨迹；编排本身仍在本 crate。

---

## 2. 系统全景

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

## 3. 用户合约（算法面）

产品面只有一个文件、两个函数（细节与示例见[快速上手](../guide/compute_quickstart.zh.md)）：

1. **`plan()`**（或常量 `PLAN`）流式产出可 JSON 序列化的 object。
2. **`execute(item)`** 收到与 yield **同形**的 dict（`{id, …fields}`）。
3. **argv 一致**：`python task.py --n 2` 与 `persisting compute task.py -- --n 2` 看到同一套 `sys.argv`。
4. **扩规模不改文件**：只换 `-w` / `--per-worker` / `torchrun`。

内部控制面会把平面 JSON 归一成 `TaskExpr`（`id` / `op=execute` / `args` / `meta`）；产品面 **只支持** `op=execute`。新能力写在用户 `execute` 里，不靠扩展 op 表。

结果线格式为 `TaskResult`：`task_id`、`ok`、`cancelled`、`value` / `error`、`worker`、时间戳、`infra_retries` 等。

---

## 4. 运行时拓扑

### 4.1 本地 `-w N`

单进程：`N × per_worker` 个槽位 Actor；本进程即 Driver。

### 4.2 torchrun

| Rank | 角色 |
|------|------|
| 0 | Driver（plan + dispatch）+ 本机槽位 |
| >0 | 本机槽位，等 `Shutdown` |

- 读 `RANK` / `WORLD_SIZE` / `MASTER_ADDR` / `MASTER_PORT`（及可选 `LOCAL_RANK`）。
- Pulsing 种子端口默认 `MASTER_PORT+17`，可用 `PERSISTING_PULSING_PORT` 覆盖，避免与 c10d 冲突。
- 控制面**不**自己拉齐进程；由 torchrun 负责。

### 4.3 槽位命名

池按 **slot-major** 展平：先各 worker 的 slot0，再 slot1…  
扁平下标：`slot * n_workers + worker`（DeathWatch / quarantine 必须用这套下标，不能用本 rank 局部序数）。

---

## 5. 调度与执行

### 5.1 Driver 循环

1. 从 `plan()` 流式读任务（NDJSON）。
2. 全局 `max_inflight`（默认 `workers × per_worker`）限制在途量。
3. 派发前 **SkipSet claim** `task_id`（resume 种子 + 本 job 已派发）；同 id 不二次派发。
4. 首触：**least-loaded** 选槽；对该槽发起过 `Execute` 之后 → **sticky-only**（只打同一槽）。
5. 完成即 `on_result`，并 `await` 异步 sink enqueue（队列满则背压 Driver）。

### 5.2 sticky-only 与 quarantine

| 情况 | 行为 |
|------|------|
| 尚未接触任何槽 | least-loaded 选槽 |
| 已 ask 过某槽 | infra 重试 **只**打该槽 |
| 该槽被 quarantine | **拒绝**改投他槽，任务记 infra 失败（避免跨槽 at-least-once 重跑 `execute`） |
| 全槽 quarantine | acquire fail-fast，不再死等 |

同槽 **ResultCache**（`task_id → TaskResult`）跨 Supervision 重启共享：丢 reply 时可幂等取回，不重跑 `execute`。

`--retries` 只覆盖 **worker ask / 基础设施**失败；**不**解读业务 `ok=false`（那是 L3 语义重试，未做）。

### 5.3 L3 执行缝

每槽一个长驻 Python 子进程（行协议 JSON）：

| cmd | 行为 |
|-----|------|
| `run_plan` | 加载脚本（带 argv），调 `execute(item)`；按路径缓存模块 |
| `shutdown` | 退出 host |

Worker **不**解释业务；Driver **不**碰 Python。失败 traceback 编进 `TaskResult`。

### 5.4 取消

- Ctrl-C → job `CancellationToken`：停接新任务；在途 acquire / ask 可取消。
- 在途 execute：**kill Python host**，返回 `cancelled`（下一任务会重新拉起 host）。
- 跨 rank：每 rank 独立 `JobControlActor`（与 Worker **分邮箱**），rank0 广播 `Cancel`，避免被串行 `Execute` 堵住。
- 已成功的结果**不会**被改写成 `cancelled`。

---

## 6. 耐久、幂等与续跑

### 6.1 唯一 sink

只有控制面写就绪结果；Executor **禁止**直写存储账本。

| 路径 | 作用 |
|------|------|
| `--sink DIR` | `ready.ndjson` + `failures.ndjson` + `checkpoint.json` |
| 无 `--sink` | 默认仅 stdout NDJSON（开发视图，非耐久） |
| `--traj` | Tee 到 Vortex；**JSONL 仍是 resume 真相** |

`JsonlFileSink`：打开时从已有文件 **seed** `task_id`；先 reserve 再写盘，失败则 **rollback seen**（seen ⊆ durable）。  
异步 `sink_writer`：`join` 汇总 persist 失败并 fail job；失败时 `SkipSet::remove`，便于后续 `--resume` 发现未落盘 id。损坏 / 无 `task_id` 的 JSONL 行：**skip + warn**，不拖垮整本账本。

### 6.2 Checkpoint / `--resume`

- 账本 = sink 下已终端的 `task_id`（ready ∪ failures）。
- `--resume`：Driver 跳过这些 id；**`plan()` 仍会再 emit 一遍**（大 plan 的成本是已知限制）。
- 失败 / 取消默认也不重跑；要重跑请编辑 failures 或换目录。

### 6.3 两类重试（边界）

| 层 | 管什么 | Phase-1 |
|----|--------|---------|
| L2 | 计算单元在不在（ask / 节点） | `--retries` + sticky + quarantine |
| L3 | 要不要再产一次（业务） | 未做；留给用户 `execute` |

---

## 7. 模块地图

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

## 8. 非目标与下一阶段

### 刻意不做（保持薄）

- 多 op 产品面、独立 Python 编排包、内嵌用户解释器
- 自研进程启动器替代 torchrun
- 在 L2 堆 harness / 判分 / Agent 会话
- Meta 语义重试、模板 fan-out、配额、亲和调度

### 已知差距

| 项 | 说明 |
|----|------|
| 昂贵 plan 无 cursor | resume 仍全量再 emit |
| 真 torchrun CI | 有双 ActorSystem 烟测；真实多进程 e2e 仍薄 |
| 错误分类 | infra / execute / cancel 尚未成稳定枚举字段 |
| DeathWatch | 仅本地；远端死槽靠 ask 失败路径 |
| 对外命名 | 内部仍 `TaskExpr`；文档侧逐步对齐 TaskSpec |

### 设计原则（摘要）

1. 算法入口极轻：两个函数；本地 `for` 循环即真。  
2. 扩规模换命令，不改文件。  
3. Driver ≠ Actor；Pulsing ≠ 调度器。  
4. 跨槽拒绝优先于跨槽重跑。  
5. persist 失败必须可见；JSONL 是 resume 真相。  
6. 新产品能力优先进 `execute`，不膨胀 CLI。

---

## 9. 相关文档

- [Compute 快速上手](../guide/compute_quickstart.zh.md)
- [CLI 整体架构](cli_architecture.zh.md)（`compute` 为例外静态路径）
- [轨迹存储](trajectory_storage.zh.md)（`--traj` / L1）
- 示例：[`examples/compute/`](../../../examples/compute/)
