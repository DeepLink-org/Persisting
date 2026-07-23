# Persisting compute

一个文件：

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

```bash
python3 task.py --n 2
persisting compute task.py -w 4 --per-worker 1 -- --n 2
# --per-worker 1（默认）：每 worker 一槽；耗时差大时优先加 -w
# --per-worker N：每 worker/rank N 个槽（各带独立 Python host，真并行）
persisting compute task.py -w 1 --per-worker 4 -- --n 2
persisting compute task.py -w 4 --retries 2 --sink /tmp/run1 -- --n 2
# 中断后续跑（跳过 ready/failures 里已有 task_id）；看进度：/tmp/run1/checkpoint.json
persisting compute task.py -w 4 --sink /tmp/run1 --resume --observe -- --n 1000
# 同时写入 Vortex 轨迹（JSONL 仍用于 resume）
persisting compute task.py -w 2 --sink /tmp/run1 --traj -- --n 4
# 读回：persisting traj stats /tmp/run1/traj --agent-id compute --session-id run1
# 可观测：stderr 上 `[obs]` 行（默认同时关掉结果 NDJSON 刷屏）
# 注意：`--observe` 必须写在 `--` 前面
persisting compute task.py -w 2 --observe -- --n 2
persisting compute task.py -w 2 --observe --observe-file /tmp/obs.ndjson -- --n 2
persisting compute task.py -w 2 --observe --results ndjson -- --n 2   # 结果 + obs 都要
torchrun --nproc_per_node=8 -- persisting compute task.py -- --n 2
persisting compute task.py --check -- --n 2
```

设计说明见 [docs/src/design/compute_control_plane.zh.md](../../docs/src/design/compute_control_plane.zh.md)。

- `execute` 收到的就是 `plan()` yield 出来的 dict
- `--` 后参数进入 `sys.argv`
- `--sink` 由控制面唯一落盘 `ready.ndjson` / `failures.ndjson`
- Ctrl-C 取消未启动的 RunFuture（Phase-1）
