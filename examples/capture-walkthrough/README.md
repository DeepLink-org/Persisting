# Capture 分步示例

```bash
cd examples/capture-walkthrough
./run.sh              # 一键跑通
./run.sh mock         # 分步 · 终端 A：Mock LLM
./run.sh check        # 校验 0001.md
./run.sh --help
```

**`run.sh` 从上到下 5 步**：构建 → Mock LLM → capture run → 等 markdown → 打印/replay/check。

产物：`store/demo-agent/walkthrough-001/0001.md`（YAML frontmatter + `<!-- persisting:block:{speaker} … -->` 块）

## 分步手动（两个终端）

```bash
# 终端 A
./run.sh mock

# 终端 B（需已 cargo build persisting-cli）
persisting capture run -o ./store -c proxy.yaml -f md -- python3 agent.py
./run.sh check
```

`capture run` 为进程内代理，退出后无需 `capture stop`。

## 文件

| 文件 | 作用 |
|------|------|
| **run.sh** | 入口脚本 |
| **agent.py** | 示例 Agent；capture 执行它，可换成你的代码 |
| **mock_llm.py** | 本地假 LLM（`run.sh` 或 `./run.sh mock` 启动） |
| **dialogue_fixture.py** | 固定两轮对话；agent / mock / check 共用 |
| **check.py** | 校验 frontmatter 与块内容 |
| **proxy.yaml** | 代理 19081 → Mock 19080 |

## 真实 LLM

用 [llm-proxy/deepseek.yaml](../llm-proxy/deepseek.yaml)，仍执行：

```bash
persisting capture run -o ./store -c your.yaml -f md -- python3 your_agent.py
```

文档：[cli_capture_command.zh.md](../../docs/src/design/cli_capture_command.zh.md)
