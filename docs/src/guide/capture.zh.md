# 采集 Agent 轨迹

单次受管 Agent Run 使用 `persisting execute`。只有多个独立启动的客户端需要共享长期
Gateway 时，才使用 `persisting gateway serve`。

参考：[pVisor 命令](../design/cli-pvisor.md)、[History / Eval / Gateway 命令](../design/cli-history.md)、
[Gateway 架构](../design/gateway.md)。

## 构建

```bash
cargo build --release -p persisting-pvisor -p persisting-cli
export PATH="$(pwd)/target/release:$PATH"
```

## 本地示例

仓库提供 Mock 模型和两轮示例 Agent；定量示例会校验 upstream 请求数、Gateway counters
和 AgenticMD blocks：

```bash
cd examples/pvisor/04-gateway-llm-control
./run.sh
```

脚本会打印实际产物路径。Run ID 由 pVisor 分配，不要依赖固定 session 目录或
固定的 Markdown 文件名。

## 运行真实 Agent

直接指定上游路由并配置对应 API key：

```bash
export DEEPSEEK_API_KEY=sk-...

pvisor run \
  --workspace ./store/run \
  --agent deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

可以把 `claude` 换成 `codex` 或其他使用代理/base URL 的程序。pVisor 会启动进程内
Gateway、注入子进程环境、等待子进程结束并关闭 Gateway。

### 存储模式

| 参数 | 写入 | 适用场景 |
|---|---|---|
| `--gateway-stream-markdown` | live AgenticMD 投影 | 轻量、本地、人读优先 |
| `--chronicle-mode lance` | canonical 结构化事件 | 完整事件、评测和派生视图 |

Lance Run 可通过 `persisting history materialize` 生成 AgenticMD。

## 长期 Gateway

前台启动：

```bash
persisting gateway serve -o ./store \
  -c examples/pvisor/04-gateway-llm-control/configs/deepseek.toml -f markdown
```

启动 banner 会打印代理地址和环境变量；在启动 Agent 的终端中导出它们。后台模式：

```bash
persisting gateway start -o ./store \
  -c examples/pvisor/04-gateway-llm-control/configs/deepseek.toml -f markdown
persisting gateway status
persisting gateway list
persisting gateway stop
```

不要让 `pvisor run` 和独立 proxy 同时写同一个 storage。独立 proxy 不提供 pVisor 的
进程与 OverlayFS 生命周期。

## 查看轨迹

```bash
# 发现 Agent 目录下的全部 Story。
persisting history stats ./store/<agent-id> --detail

# 回放一个 Run 目录。
persisting history replay ./store/<agent-id>/<run-id>

# 从 canonical Lance events 生成可读视图。
persisting history materialize ./store \
  --agent-id <agent-id> \
  --root-session-id <run-id> \
  --session-id <session-id>
```

执行过 `gateway start` 或设置 `PERSISTING_CAPTURE_STORAGE` 后，`stats`、`replay`、
`materialize` 和 `truncate` 可以省略 storage 参数。

## 目录布局

```text
store/
├── .capture/                 # Gateway runtime 元数据和失败记录
└── agent-id/
    └── run-id/
        ├── events.lance/     # --chronicle-mode lance
        ├── run-id.md         # --gateway-stream-markdown 或 materialize 后
        └── agent-<id>.md     # 可选 subagent Story
```

系统生成的 AgenticMD 使用按 session 命名的文件和接近 Storyline 的块字段。读取器也兼容
旧块头及普通 Markdown，因为它是调试视图而不是存储协议。

## 常见问题

| 问题 | 检查 |
|---|---|
| Agent 无法连接 Gateway | 使用 `pvisor run` 注入的子进程，或导出独立 proxy banner 中的变量 |
| Codex 绕过代理 | 使用 banner 打印的 `-c openai_base_url=...` |
| Lance 输出没有 Markdown | 启用 `--gateway-stream-markdown` 或执行 `history materialize` |
| 采集事件失败 | 查看 `.capture/dead_letter.jsonl`，再使用 `history replay-dead-letter` |
| pVisor 提示已有 owner | 停止 proxy、等待 live Run 结束，或更换 storage |

独立 dlcapt 有自己的配置和存储模型；需要它时查看
`crates/persisting-dlcapt/README.md`。
