# Capture 分步示例

```bash
cd examples/capture-walkthrough
./run.sh              # 一键跑通
./run.sh mock         # 分步 · 终端 A：Mock LLM
./run.sh check        # 校验最近生成的 AgenticMD
./run.sh --help
```

**`run.sh` 从上到下 5 步**：构建 → Mock LLM → `pvisor run` → 等 AgenticMD → 打印/replay/check。

产物：`store/demo-agent/<run-id>/<run-id>.md`（YAML frontmatter + `<!-- persisting:block:{speaker} … -->` 块）。Run ID 由 pVisor 分配。

## 分步手动（两个终端）

```bash
# 终端 A
./run.sh mock

# 终端 B（需已构建 pvisor）
pvisor --config run.toml --workspace ./store/run -- python3 agent.py
./run.sh check
```

`pvisor run` 使用进程内 Gateway，子进程退出后自动停止。

## 文件

| 文件 | 作用 |
|------|------|
| **run.sh** | 入口脚本 |
| **agent.py** | 示例 Agent；pVisor 执行它，可换成你的代码 |
| **mock_llm.py** | 本地假 LLM（`run.sh` 或 `./run.sh mock` 启动） |
| **dialogue_fixture.py** | 固定两轮对话；agent / mock / check 共用 |
| **check.py** | 校验 frontmatter 与块内容 |
| **proxy.toml** | 代理 19081 → Mock 19080 |

## 真实 LLM

用 [llm-proxy/deepseek.toml](../llm-proxy/deepseek.toml)，仍执行：

```bash
pvisor --config your-run.toml --workspace ./store/run -- python3 your_agent.py
```

文档：[Capture 快速上手](../../docs/src/guide/capture.zh.md) · [pVisor 命令](../../docs/src/design/cli-pvisor.md) · [Traj 命令](../../docs/src/design/cli-traj.md)
