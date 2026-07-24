# Compute 编排

> **状态**: 稳定 — `plan()` + `execute()` 模型，本地并行和 torchrun。

有时候你需要对大量输入运行同一个函数——超参搜索、评估批处理、数据处理流水线。你可以写个 `for` 循环。但接着你需要并行。然后需要断点。然后需要中断后续跑。然后需要多机扩展。

Persisting Compute 用一个约束给你这一切：写一个 Python 文件，两个函数——`plan()`（做什么）和 `execute(item)`（怎么做）。

---

## 你的第一个任务

一个 Compute 任务就是一个 Python 文件。`plan()` yield 任务 dict。`execute(item)` 处理一条并返回结果。

```python
import argparse

def _parse_args(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("-n", "--n", type=int, default=10)
    return p.parse_args(argv)

def plan():
    """产出任务。每条必须有 'id'。"""
    args = _parse_args()
    for i in range(args.n):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    """处理一条任务。返回 dict。"""
    return {"x2": item["x"] * 2}

# 作为普通脚本也能跑
if __name__ == "__main__":
    for xx in plan():
        print(execute(xx))
```

约定很简单：

- `plan()` yield dict——每条必须有 `id`（缺则生成 UUID）
- `execute(item)` 收到 `plan()` yield 出来的那条
- 参数放在 `--` 后面——和 `python task.py --n 2` 一样

---

## 从循环到并行

先本地验证，再扩展：

```bash
# 能跑吗？
python3 task.py --n 2
persisting compute task.py --check -- --n 2

# 4 个本地 worker
persisting compute task.py -w 4 -- --n 100

# 或者少 worker、每个 worker 多并发
persisting compute task.py -w 1 --per-worker 4 -- --n 100
```

同一个文件不改就能多进程扩展：

```bash
torchrun --nproc_per_node=8 -- persisting compute task.py -- --n 100
```

和自己写 multiprocessing 的关键区别：**任务只派发一次，跨 worker、跨重启都是。**

---

## 扛住中断

加上 `--sink`，结果就持久化了：

```bash
persisting compute task.py -w 4 --sink /tmp/run1 -- --n 1000
```

sink 目录包含：

| 文件 | 内容 |
|------|------|
| `ready.ndjson` | 成功结果，一行一条 JSON |
| `failures.ndjson` | 失败或取消的任务 |
| `checkpoint.json` | 进度摘要 |

如果任务被中断（Ctrl-C、节点故障），resume 从断点继续：

```bash
persisting compute task.py -w 4 --sink /tmp/run1 --resume -- --n 1000
```

Resume 跳过已在 `ready.ndjson` 或 `failures.ndjson` 中的任何 `task_id`。`plan()` 会再跑一遍，但已完成的任务不会再派发。要重跑失败，清空 `failures.ndjson` 或用新 sink 目录。

进度监控：

```bash
persisting compute task.py -w 4 --sink /tmp/run1 --resume --observe -- --n 1000
```

---

## 调度如何工作

Compute 运行时遵循简单模型：

```
plan()  ──流式产生──►  Driver  ──派发──►  Workers (Python 子进程)
                          │                      │
                          │  sticky: 任务一旦     │  每个 worker 是一个
                          │  打到某个 worker，    │  长驻 Python 子进程
                          │  重试也只打它          │
                          │                      │
                          ▼                      ▼
                      SkipSet                execute(item)
                   (已完成 id)                    │
                                                 ▼
                                            ResultSink
                                       (ready.ndjson + failures)
```

- **SkipSet**: Resume 后 `plan()` 仍 yield 全部任务，但 Driver 跳过已在 sink 中的 id。
- **Sticky 派发**: 如果 worker 执行中失败，重试打到同一个 worker（避免在不同 worker 上的 at-least-once 重执行）。
- **Quarantine**: 如果 worker 反复失败，被隔离——不再派新任务给它。
- **ResultCache**: 如果 worker 重启，Driver 可取回已算完的结果，不重跑 `execute`。

---

## 常用选项

| 选项 | 说明 |
|------|------|
| `-w N` | 本地 worker 数 |
| `--per-worker N` | 每 worker 并发槽数（默认 1） |
| `--python PATH` | Python 解释器 |
| `--retries N` | 基础设施重试（worker 连不上，不是业务失败） |
| `--results ndjson\|summary\|quiet` | 输出格式 |
| `--sink DIR` | 持久化结果 |
| `--resume` | 跳过已完成任务 id |
| `--observe` | 显示进度 |
| `--traj` | 同时写入 Lance 轨迹 |

---

## 常见问题

**任务没跑 / id 重复？**  
同一 `id` 一次 job 只派一次。Resume 跳过 sink 已有 id。

**Ctrl-C 之后？**  
未开始的任务标记取消。正在跑的 `execute` 被打断。已写入的结果保留。

**`--retries` 覆盖业务失败吗？**  
不。重试只针对基础设施（worker 连不上）。业务重试在 `execute` 里自己做。

**什么时候用 vs Ray / multiprocessing？**  
当你想要 `plan()` + `execute()` 但不想引入分布式框架时。适合文件/分片级别的任务。不适合逐行派发。

---

## 下一步

- [Compute 架构](../design/compute.md) — Driver、调度器、sink、quarantine
- [示例脚本](https://github.com/DeepLink-org/Persisting/tree/main/examples/compute)
