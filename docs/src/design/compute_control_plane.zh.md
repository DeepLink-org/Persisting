# Compute 控制面设计

> 状态：**Phase-1 已落地**（可运行竖切）  
> 代码：`crates/persisting-compute` · CLI：`persisting compute`  
> 示例：`examples/compute/plan_simple.py`  
> 对齐主张：Agentic Infra（L2 编排 · L3 执行缝 · 唯一 sink）

薄编排层：算法工程师写一个 Python 文件，本地能跑，换启动命令即可扩规模。  
**不是** Ray，**不**嵌用户解释器，**不** DIY `torchrun`。

---

## 1. 定位与边界

### 1.1 在 Agentic Infra 中的位置

```text
L3 Workload   复杂负载 → 干净、可调度的一次运行（本阶段 = plan.py::execute）
L2 Compute    把众多 Future 编成吞吐（本 crate）
L1 Storage    就绪 Result 落盘为资产（Phase-1 = JSONL；目标 = Vortex 轨迹）
```

三层是**模块边界**；部署上仍是**单一控制面入口**（`persisting compute`），不是三套集群产品。

### 1.2 是 / 不是

| 是 | 不是 |
|----|------|
| L2 编排：plan 流式 → Driver 分发 → Worker 执行 | 分布式训练框架 |
| L3 执行缝：调用用户 `execute(item)` | 新编程模型 / DSL / 装饰器框架 |
| 用真实 `torchrun` 做多进程 | 自研 launcher / rdzv |
| 用 Rust Pulsing actors 做骨架 | Python Pulsing / Ray 替代品 |
| 控制面 **spawn** `--python` | PyO3 内嵌用户环境 |

### 1.3 与 CLI 引擎架构的关系

`search` / `traj` 走「瘦 CLI + 动态加载 engine」。  
**`compute` 不同**：逻辑在 `persisting-compute` 库内、由 CLI **静态链接并直接 async 跑**，不经 RON/engine ABI。落盘目标接上 Vortex 后，再经 engine 的 `TrajectoryAppend` 写 L1（sink adapter），编排本身仍留在本 crate。

---

## 2. 用户合约（算法面）

产品面只有一个文件、两个函数：

```python
import argparse

def _parse_args(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("-n", "--n", type=int, default=10)
    return p.parse_args(argv)

def plan():
    args = _parse_args()          # 推荐：在 plan()/execute 内解析，勿依赖模块 import 副作用
    for i in range(args.n):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return {"x2": item["x"] * 2}

if __name__ == "__main__":
    for xx in plan():
        print(execute(xx))
```

### 2.1 约定

1. **`plan()`**（或常量 `PLAN`）产出任务；每项可 JSON 序列化为 object。
2. **`execute(item)`** 收到的对象与 `plan()` yield **同形**（`{id, …fields}`），不是内部控制面线格式。
3. **argv 一致**：`python task.py --n 2` 与 `persisting compute task.py -- --n 2` 看到同一套 `sys.argv`（`--` 之后原样转发）。
4. **扩规模不改文件**：只换启动命令（本地 `-w` / `torchrun`）。

### 2.2 argv 注意

- Plan 进程（emit）与 Execute host（worker）都会设置 `sys.argv = [script, *user_args]`。
- Execute host **按脚本路径缓存**已 import 的模块：若在**模块顶层**解析 argparse，同路径不会因换参数而重载。  
  **推荐**在 `plan()` / `execute()` 内调用 `parse_args()`。

---

## 3. CLI

```bash
# 本地扩规模
persisting compute SCRIPT [-w N] [--per-worker N] [--python PY] [-E PATH] \
  [--retries N] [--sink DIR] [--results ndjson|summary|quiet] -- <script_args>

# 校验（不替代完整跑）
persisting compute SCRIPT --check [--limit N] -- <script_args>

# 内置 smoke（须显式）
persisting compute --self-test

# 多进程（真实 torchrun）
torchrun --nproc_per_node=K -- persisting compute SCRIPT -- <script_args>
```

| 开关 | 含义 |
|------|------|
| （默认） | **run**：拉起 fleet，跑完退出 |
| `-w N` | 本地 worker 数（非 torchrun 时） |
| `--per-worker N` | 每 worker/rank 的并发槽数（每槽独立 Actor + Python host） |
| `--observe` / `--observe-file` / `--observe-json` | 可观测：stderr `[obs]` 进度；file/json 为 NDJSON。须写在 `--` 前；开启后默认 `--results quiet` |
| `--retries N` | L2 基础设施重试次数（默认 2；仅 worker ask 失败） |
| `--sink DIR` | 唯一落盘目录：`ready.ndjson` + `failures.ndjson` + `checkpoint.json` |
| `--resume` | 从 `--sink` 账本跳过已完成 `task_id`（须同时给 `--sink`） |
| `--traj` | Tee 写 Vortex 轨迹（`compute.result`）；须同时 `--sink` |
| `--results` | 终端展示：`ndjson`（默认）/ `summary` / `quiet` |
| Ctrl-C | Job cancel：未启动任务标 `cancelled`（见 §7.1 限制） |
| `--check` | env → plan emit → `execute` 存在 → 本地试跑 |
| 省略 SCRIPT | **报错**（不再默认跑 smoke） |

唯一入口：`persisting compute`。无独立 `persisting-compute` 二进制，无 `check/run/emit/worker` 子命令族。

---

## 4. 数据面

### 4.1 任务（内部名 `TaskExpr`）

线格式：一行一个 JSON（NDJSON）。用户侧推荐平面写法（与 yield 一致）：

```json
{"id": "t-0", "x": 1}
```

控制面归一化：

```json
{"id": "t-0", "op": "execute", "args": {"x": 1}, "meta": {}}
```

| 字段 | 说明 |
|------|------|
| `id` | 任务身份；缺省则生成 UUID |
| `op` | 缺省 `"execute"`；**产品面仅支持 `execute`** |
| `args` | 业务字段；平面 JSON 的剩余键落入此 |
| `meta` | 预留；当前调度/执行不消费 |

新能力写在用户 `execute` 内，**不**靠扩展 op 插件表。

> 命名对照（Agentic Infra）：用户侧 ≈ TaskSpec 载荷；内部仍叫 `TaskExpr`，对外文档逐步对齐。

### 4.2 结果（`TaskResult`）

| 字段 | 说明 |
|------|------|
| `task_id` | 对应任务 id |
| `ok` | 是否成功 |
| `cancelled` | L2 取消（非业务失败） |
| `value` / `error` / `traceback` | 成功值或失败信息 |
| `worker` | 执行 worker id |
| `started_at` / `finished_at` | Unix 秒 |
| `infra_retries` | L2 基础设施重试次数（0 = 首次成功） |

---

## 5. 运行时拓扑

```text
                 plan.py
            （spawn --python -c bootstrap）
                       │ NDJSON TaskExpr
                       ▼
                 Driver（rank0 / 本地控制进程）
                       │ plan emit + least-loaded dispatch
                       │（直接 ask Worker，并行）
          ┌────────────┼────────────┐
          ▼            ▼            ▼
      slot actors  (N workers × P per-worker)
          │  each: WorkerActor + Python host
          └──── stdin/stdout JSON ────┘
                       │
                       ▼
              plan.py::execute(item)
                       │
                       ▼
              控制面 ResultSink（stdout 与/或 --sink）
```

每个从 `plan()` 出来的任务在 Driver 调度层对应一个 **`RunFuture`**（`wait` / `cancel` / `task_id`）。

### 5.1 本地 `-w N`

单进程：N 个 `WorkerActor`；本进程即 **Driver**（`Driver::run_plan`），无独立 DriverActor。

### 5.2 torchrun

| Rank | 角色 |
|------|------|
| 0 | **Driver**（plan + dispatch）+ 本机 `per_worker` 个槽位 Actor |
| >0 | 本机 `per_worker` 个槽位 Actor，等 `Shutdown` |

- 读环境：`RANK` / `WORLD_SIZE` / `MASTER_ADDR` / `MASTER_PORT`（及可选 `LOCAL_RANK`）。
- Pulsing 种子：`MASTER_ADDR:(MASTER_PORT+17)`，可用 `PERSISTING_PULSING_PORT` 覆盖，避免与 c10d 撞端口。
- 控制面**不**自己 spawn 进程；由 torchrun 拉齐 ranks。

### 5.3 调度参数

- **`--per-worker N`**（默认 1）：每个逻辑 worker / torchrun rank 上的**并发 Execute 槽**。
  - 实现：每槽一个 `WorkerActor` + 独立 Python host（Actor mailbox 仍串行，槽与槽之间并行）。
  - 池按 **slot-major** 展平：先铺满各 worker 的 slot0，再 slot1…，least-loaded 仍优先打散到不同 worker。
- **Least-loaded**：在展平后的槽位上选当前 in-flight 最少者；平手取更小下标。
- **`max_inflight`**：全局上限（默认 `workers × per-worker`）。
- 任务 **直接 `ask` 槽位 Actor**（不经 Driver 串行 `await Execute`）。
- **完成即回调**：`on_result` / sink 按完成序；outstanding ≤ `max_inflight`。
- Infra retry：失败则释放槽位再 acquire，避免粘在坏节点上。
- Job 级 `CancellationToken`：Ctrl-C → 停止接新任务 + 子 token 取消在途 acquire。

---

## 6. L3 执行缝

每个 **槽位** 持有一个长驻 `--python` 子进程（行协议 JSON）：

| cmd | 行为 |
|-----|------|
| `run_plan` | 加载 plan 模块（带 `argv`），调用 `execute(item)`；按路径缓存模块 |
| `shutdown` | 退出 host |

- Worker **不**解释业务语义；Driver **不**碰 Python。
- 失败时 traceback 编入 `TaskResult` 回传。
- 产品面 Executor 路由：**仅** `op=execute` → `PlanExecuteExecutor`。

---

## 7. Phase-1 合约实现

相对 Agentic Infra PPT，本阶段落地三条主契约（算法面保持极轻）。

### 7.1 RunFuture（调度原子）

- 实现：`future.rs`；进程内 `JoinHandle` + `CancellationToken`。
- **已支持**：`wait`（始终 join）、协作式 `cancel`（未启动 / 等待 acquire）。
- **在途 cancel**：Worker 共享 job `CancellationToken`；取消时 **kill Python host** 并返回 `cancelled`（下一任务会重新拉起 host）。
- **跨 rank**：每 rank 有独立 `JobControlActor`（与 Worker **分邮箱**）；rank0 Ctrl-C → 广播 `Cancel` → 远端 token cancel → 在途 host kill。不经过 Worker mailbox，避免被串行 `Execute` 挡住。
- **取消语义**：`wait` **不**把已成功结果改写成 `cancelled`；取消来迟则保留成功。
- Driver 在 `ask` 上同样 select cancel，避免 Ctrl-C 后干等 Python。

### 7.2 L2 基础设施重试

- Driver 在 **worker ask 失败**（节点丢、传输错）时重投；次数 `--retries`。
- **Sticky**：失败后优先同一槽位，便于命中该 Worker 的 `task_id → TaskResult` 缓存（同 worker 上丢失 reply 可幂等重取，不重跑 `execute`）。
- **不**解读 `execute` 返回的业务失败（那是 L3 Meta 语义重试，未做）。
- Infra 耗尽 → 该任务 `TaskResult` 失败（`ok=false`），**不** `bail` 整次 job；sibling 继续跑。
- `TaskResult.infra_retries` 可观测。
- **Sink 去重**：`JsonlFileSink` 打开时从已有 JSONL **seed** `task_id`；跨进程重复 append 跳过。Vortex Tee 同样可 seed。
- **Seen ⊆ durable**：先 reserve `task_id`，写盘/Vortex 失败则 **rollback seen**，避免永久丢账。
- **Live SkipSet**：dispatch 前 claim `task_id`（resume 种子 + 本 job 已派发）；同 id 不再打到另一 worker。`--check --limit` 用 SkipSet 跳过尾部任务。
- **Async sink writer**：完成路径 `await` enqueue（队列满则自然背压 Driver drain）；`join` 汇总 persist 失败并 fail job。persist 失败会 `SkipSet::remove`，便于后续 `--resume` 发现未落盘 id。
- **Tee 尽力而为**：某一路失败不阻断兄弟 sink（仍返回首个错误）。
- **Sticky-only after contact**：一旦对该槽发起过 `Execute` ask，infra 重试**只**打同一槽（`acquire_sticky`）；若该槽被 quarantine，**拒绝**改投他槽（避免跨槽 at-least-once 重跑 `execute`），任务记 infra 失败。
- **槽位 quarantine**：连续 infra ask 失败达到阈值后摘槽；**全槽 quarantine → acquire fail-fast**（任务 infra 失败，非整 job 死等）。DeathWatch（仅本地）在 Worker 终止时 `force_quarantine`。Watch 注册必须用 **slot-major 扁平下标**（`DistEnv::slot_flat_index`），不能用本 rank 的 `0..per_worker` 序数。
- **Pulsing 深化**：Worker `spawn_factory` + `SupervisionSpec`（失败重启上限）；**ResultCache 与槽位同生命周期**（`Arc` 跨重启共享）；统一 `resolve_named` / `ask_timeout`；JobControl 侧信道 cancel + 本地 DeathWatch。
- **仍须注意**：跨槽拒绝后该任务失败（非整 job bail）；算法若需更强保证应自备幂等或缩小副作用面。

### 7.3 唯一 sink（控制面落盘）

- 只有控制面写就绪结果；Executor **禁止**直写存储。
- `--sink DIR` → `ready.ndjson`（成功）+ `failures.ndjson`（失败/取消）；**逐条**在 `on_result` 时 append（非跑完批写）。
- Driver **按完成序**调用 `on_result`（非 plan 提交序）；有界 outstanding（≤ `max_inflight`，`FuturesUnordered` + `RunFuture`），完成即释放，避免大 plan 囤积 JoinHandle。
- 同 process 内按 `task_id` 跳过重复 append。
- **`--traj`**（feature `traj-sink`）：Tee 到 Vortex，`CaptureRecord{kind=compute.result|compute.failure}` → `TrajectoryAppend`；默认 `{sink}/traj` / agent=`compute` / session=sink 目录名。JSONL 仍是 `--resume` 账本。
- 未指定 `--sink` 时：默认仅 stdout NDJSON（开发视图，非耐久 L1）。
- **限制（已知）**：Vortex 侧跨进程未按 `task_id` 去重；resume 跳过已完成 id 可避免重复写。

### 7.4 Checkpoint（断点续跑）

- 账本 = `--sink` 下已有 `ready.ndjson` / `failures.ndjson` 的 `task_id`。
- `--resume`（须带 `--sink`）：Driver 跳过已终端 id，不重跑；`plan()` 仍会再 emit 一遍。
- 同目录 `checkpoint.json`：`ok/fail/cancelled/skipped/dispatched/updated_at`（节流写入）。
- 失败/取消默认也不重跑；要重跑请编辑 failures 或换 sink 目录。

### 7.5 与全图的差距（诚实清单）

| 主张 | Phase-1 |
|------|---------|
| TaskSpec / RunFuture / RunResult 对外语言 | 内部名仍 `TaskExpr` / 进程内 Future / `TaskResult` |
| 就绪 → L1 轨迹 | JSONL 占位 |
| 可中断 | 调度层 + kill Python host；跨 rank 经 JobControlActor 广播 |
| 可复现 / 位置无关 | 未钉死 |
| Meta 语义重试 | 未做 |
| 模板 fan-out / 配额 / 亲和 / 观测账本 | least-loaded + per-worker；无模板/亲和 |

---

## 8. 模块地图与语义原语

产品按**语义原语 / 接口**切开：每块有稳定 id、主模块、同文件合约测。
注册表见 [`blocks`](../../../crates/persisting-compute/src/blocks.rs)。

**测试策略**

- **单元 / 合约测**：放在原语所在源文件末尾的 `#[cfg(test)]`（与接口同文件）。
- **`tests/`**：只放跨模块**集成**（如 `tests/integration_local.rs`：fleet 完成序、resume skip、cancel、`--per-worker`、argv）。

| Block id | 合约 | 主模块 | 测试 |
|----------|------|--------|------|
| `task_wire` | TaskExpr / TaskResult | `task` | 同文件 |
| `python_env` | PYTHONPATH 合并 | `python_env` | 同文件 |
| `placement` | least-loaded · sticky · slot-major 命名 | `scheduler` · `dist` | 同文件 |
| `run_future` | wait / cancel 语义 | `future` | 同文件 |
| `idempotency` | 同 worker 结果缓存 | `result_cache` · `worker` | 同文件 |
| `sink` / `checkpoint` | 唯一 sink · resume skip · seed 去重 | `sink` · `checkpoint` | 同文件 |
| `observe` | 可选进度事件 | `observe` | 同文件 |
| `plan` / `execute` | plan emit · execute host · cancel kill | `plan` · `executor` | 同文件 |
| `worker` | Actor 缝合 · cache · 多 slot ask | `worker` | 同文件 |
| `driver` / `fleet` | 完成序 drain · skip · job cancel · 装配 | `driver` · `runtime` | `tests/integration_*.rs` |

### 8.1 文件职责

| 路径 | 职责 |
|------|------|
| `persisting-cli` → `Command::Compute` | 唯一 CLI 入口 |
| `cli.rs` | 参数 → `run_fleet` / sink / Ctrl-C |
| `blocks.rs` | 语义原语 id 注册表（文档对齐；非独立测套） |
| `driver.rs` | **Driver**：plan emit + least-loaded dispatch；完成序 `on_result` + 有界 inflight |
| `checkpoint.rs` | `--resume` 账本 + `checkpoint.json` 进度 |
| `future.rs` | `RunFuture` · `wait_all` |
| `result_cache.rs` | 同 worker infra 幂等缓存 |
| `scheduler.rs` | Least-loaded · sticky prefer |
| `observe.rs` | `--observe` 事件 |
| `sink.rs` | `ResultSink` · `JsonlFileSink` · `TeeSink` |
| `sink_traj.rs` | （feature `traj-sink`）`VortexResultSink` |
| `plan.rs` | bootstrap：`sys.argv` + `plan()` → NDJSON |
| `runtime.rs` | local / torchrun fleet 装配（`spawn_rank_slots` / `run_driver_loop` 共享） |
| `worker.rs` | `Execute` / `Shutdown` + cache + cancel token |
| `executor.rs` | PlanExecute host（可取消 kill） |
| `dist.rs` | torchrun 环境 → Pulsing seed · slot 命名 |
| `job_control.rs` | 旁路 cancel + 本地 DeathWatch |
| `pulsing_ext.rs` | 统一 resolve / ask_timeout / spawn_supervised |
| `skip.rs` / `sink_writer.rs` | live claim · 异步 persist |
| `check.rs` | `--check` / `--self-test` |
| `task.rs` | `TaskExpr` / `TaskResult` |
| `python_env.rs` | `PYTHONPATH` 合并 |

crate 公开面收窄：多数模块 `pub(crate)`；`lib` 只 re-export CLI / 集成测所需类型（`run_compute`、`RunOptions`、`SkipSet`、sink 主类型等）。

---

## 9. 校验与自测

| 模式 | 行为 |
|------|------|
| `cargo test -p persisting-compute` | 同文件单元 / 合约测 + `tests/integration_*.rs` |
| `--check` | python 探针 → plan emit → 确认 `execute` 可调用 → `run_local_fleet` 试跑 |
| `--self-test` | 写临时内置 `plan()`+`execute`，走完整 check（不依赖用户文件） |

---

## 10. 非目标与路线

### 10.1 刻意不做（保持薄）

- 多 op 产品面（`echo` / `call` / `py` 等已移除）
- 独立 Python `persisting.compute` 编排包
- 控制面内嵌用户解释器
- 自研进程启动器替代 torchrun
- 在 L2 堆 harness / 判分 / Agent 会话

### 10.2 下一阶段优先

1. **跨 worker 幂等**（已：sticky-only-after-contact + 同槽 result_cache + sink seed/rollback + 跨 rank cancel；quarantine 后该任务 infra 失败而非改投）
2. **`ResultSink` → Vortex**（真 L1 默认路径）
3. 命名对外对齐（TaskSpec / RunFuture）+ 更多红灯
4. （可选）Meta Executor 薄包装

---

## 11. 设计原则

1. **算法入口极轻**：两个函数 + 可选 argparse；本地 `for` 循环即真。
2. **扩规模换命令**：同一文件，`persisting compute` / `torchrun`。
3. **argv 一致**：`--` 后与 `python task.py …` 对齐。
4. **能力成本沉在骨架**：Pulsing / torchrun 负责分布；用户不学 actors。
5. **边界守住**：新产品能力优先进 `execute`，不膨胀 CLI 与 op 表。
6. **两类重试分开**：L2 管计算单元在不在；L3 Meta 管要不要再产一次。
7. **唯一 sink**：就绪 Result 只从控制面 append。
8. **竖切优先**：先打通可跑路径与合约形状，再补工厂能力（模板 / 配额 / 亲和）。

---

## 12. 相关文档

- Agentic Infra 主张：`deeplink.fabric/projects/agent.infra/report-agent-infra.typ`
- 轨迹 L1（Vortex）：[trajectory_storage.zh.md](trajectory_storage.zh.md)
- CLI 整体（engine 路径）：[cli_architecture.zh.md](cli_architecture.zh.md)
- 用法示例：[examples/compute/README.md](../../../examples/compute/README.md)
