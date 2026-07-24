# Compute 快速上手

用 **`persisting compute`** 跑一批独立任务：写一个 Python 文件，本地能跑，换命令即可加并行。

> 架构说明见 [Compute 架构](../design/compute_control_plane.zh.md)。示例：[`examples/compute/`](../../examples/compute/)。

---

## 你需要准备什么

- 已编译的 `persisting` CLI（含 `persisting-compute`）
- 本机可用的 `python3`（或 `--python` 指定解释器）

```bash
cargo build -p persisting-cli
export PERSISTING="$(pwd)/target/debug/persisting"
"$PERSISTING" compute --help
```

---

## 1. 写一个文件

只要两个函数：`plan()` 产出任务，`execute(item)` 处理一条。

```python
import argparse

def _parse_args(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("-n", "--n", type=int, default=10)
    return p.parse_args(argv)

def plan():
    args = _parse_args()
    for i in range(args.n):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return {"x2": item["x"] * 2}

if __name__ == "__main__":
    for xx in plan():
        print(execute(xx))
```

约定很简单：

- 每条任务是一个带 **`id`** 的 dict（缺 `id` 时控制面会生成 UUID）
- `execute` 收到的就是 `plan()` yield 出来的那条
- 业务参数写在 `--` **后面**，和直接 `python task.py …` 一样进 `sys.argv`

把参数解析放在 `plan()` / `execute()` **里面**（不要只在模块 import 时解析一次）。

---

## 2. 先本地验证

```bash
python3 task.py --n 2
persisting compute task.py --check -- --n 2
```

`--check`：检查环境、能否 emit、能否调用 `execute`，并小规模试跑。

---

## 3. 并行跑起来

```bash
# 本机 4 个 worker
persisting compute task.py -w 4 -- --n 100

# 单 worker、每 worker 4 个槽（更适合单机多核、任务偏 CPU）
persisting compute task.py -w 1 --per-worker 4 -- --n 100

# 多进程（真实 torchrun；同一文件不用改）
torchrun --nproc_per_node=8 -- persisting compute task.py -- --n 100
```

`--` 前面是 compute 自己的开关；`--` 后面原样转给脚本。

| 常用开关 | 作用 |
|----------|------|
| `-w N` | 本地 worker 数（非 torchrun 时） |
| `--per-worker N` | 每个 worker/rank 的并发槽（默认 1） |
| `--python PATH` | Python 解释器（也可用环境变量 `PERSISTING_PYTHON`） |
| `-E PATH` | 追加到 `PYTHONPATH` |
| `--retries N` | 基础设施重试次数（默认 2；不是业务失败重试） |
| `--results ndjson\|summary\|quiet` | 终端怎么打印结果（默认 `ndjson`） |

---

## 4. 落盘与中断后续跑

```bash
persisting compute task.py -w 4 --sink /tmp/run1 -- --n 1000

# 中断后接着跑：跳过 ready / failures 里已经有的 id
persisting compute task.py -w 4 --sink /tmp/run1 --resume -- --n 1000
```

`--sink DIR` 下会看到：

| 文件 | 内容 |
|------|------|
| `ready.ndjson` | 成功结果（一行一条） |
| `failures.ndjson` | 失败 / 取消 |
| `checkpoint.json` | 进度摘要（节流更新） |

说明：

- `--resume` **必须**同时带 `--sink`
- 已出现在账本里的 `id` 不会再跑（成功和失败都不重跑；要重跑请改 failures 或换目录）
- resume 时 `plan()` 仍会再扫一遍，只是已完成的 id 不会再派发

看进度还可以加：

```bash
persisting compute task.py -w 4 --sink /tmp/run1 --resume --observe -- --n 1000
```

`--observe` 必须写在 `--` **前面**；开启后默认少刷结果 NDJSON（需要两者都要时再加 `--results ndjson`）。

---

## 5. 可选：写入轨迹

若 CLI 编译了 `traj-sink` 相关能力：

```bash
persisting compute task.py -w 2 --sink /tmp/run1 --traj -- --n 4
```

JSONL 仍是 resume 账本；Vortex 轨迹是额外一份。读回可用 `persisting traj stats`（目录默认在 `{sink}/traj`）。

---

## 6. 内置自测

```bash
persisting compute --self-test
```

不依赖你的脚本，用来确认本机 CLI / Python 通路正常。

---

## 常见问题

**任务没跑 / id 重复？**  
同一 `id` 在一次 job 里只会派发一次。resume 时账本里已有的 id 也会被跳过。

**Ctrl-C 之后？**  
未开始的任务会标为取消；正在跑的 execute 会被打断。已成功写入 sink 的不会被改成取消。

**业务失败会不会自动重试？**  
不会。`--retries` 只覆盖「连不上 worker」这类基础设施问题。业务上要重试，请在 `execute` 里自己做，或清掉 failures 后再跑。

**和直接写 Ray / 自己 multiprocessing 比？**  
你只维护 `plan` + `execute`；并行、取消、落盘、续跑由 CLI 管。不适合把每一行数据都当成一个任务——任务粒度用文件 / 分片更合适。

---

## 下一步

- 示例脚本：[`examples/compute/plan_simple.py`](../../examples/compute/plan_simple.py)
- 运行时与调度：[Compute 架构](../design/compute_control_plane.zh.md)
