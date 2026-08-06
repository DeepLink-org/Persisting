# 快速开始

五分钟走完一条主链路：装 CLI → 安全地跑一个 Agent → 批量编排 → 用 SQL 查轨迹。
假设你有 macOS 或 Linux。

## 1. 安装 CLI

```bash
# 稳定版 wheel：同时安装 persisting、pvisor、ppilot
pip install persisting

# 或安装 nightly wheel
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

## 2. 安全地运行一个 Agent

在当前项目目录里跑，工作区会被暂存而不是直接写入：

```bash
pvisor run --safe codex
```

`--safe` 把当前目录作为可复用 workspace 和 OverlayFS base，Agent 的修改写入每个 Run
独立的 stage，同时启用显式网络代理；结束后 Run 和 `run-bundle.json` 保存在
`PERSISTING_RUN_HOME`。默认 host executor 的隔离是
进程级的——它不阻止 Agent 访问项目目录外的宿主路径，代理也可以被直接 socket 绕过，
这些边界都会如实记录在 bundle 里。

```bash
pvisor review last     # 查看 bundle：文件变更、网络拦截、安全警告
pvisor apply last      # 接受修改
# 或
pvisor drop last       # 丢弃修改
```

## 3. 批量编排

写一个 `plan.py`：`plan()` 产出带稳定 `id` 的任务，`execute(item)` 处理每一项。

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    value = item["value"]
    return {"square": value * value}
```

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results
cat ./results/ready.ndjson
```

`--sink` 会启用 durable result journal 和 lease fencing：任务重试只回到原 slot，
业务错误不会被自动重试，崩溃后 reconciler 会修复两个 crash window。外部副作用请用
稳定 `id` 做幂等。

仓库里 `examples/ppilot/01-run/` 是同一模式的完整可运行版本，`just examples-ppilot`
可以直接跑通；`examples/ppilot/` 下还有 `produce` / `process` / `analysis` 三个变体。

## 4. 查询轨迹历史

上面的 result sink 保存任务结果，并不是轨迹存储。在 Persisting 源码目录中，可以直接
查询仓库自带的 ATIF 轨迹 fixture：

```bash
ppilot query crates/persisting-pchronicle/tests/fixtures/atif \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'
```

输入可以是 ATIF JSON、JSONL 或目录，也可以是 Lance Storyline store（本地目录或
`s3://` URI）；两条路径暴露相同的 `runs` / `steps` / `tool_calls` 表。pChronicle 示例会
用同一批 fixture 构建 Lance store，并比较查询结果：

```bash
just examples-pchronicle
```

## 其他能力

- [Tensor Memory（实验性）](guide/tensor-memory.md) — 张量下标与分层存储
- [Queue](guide/queue.md) — 持久事件流
- [Search](guide/search.md) — 文档索引与向量/混合检索

## 下一步

- [安装指南](installation.md) — 三个安装物详解
- [选择能力](guide/index.md) — 按目标选择工作流
- [设计文档](design/index.md) — 架构与内部实现
