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
export PERSISTING_RUN_HOME=$HOME/.persisting/runs

pvisor run \
  --agent deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

可以把 `claude` 换成 `codex` 或其他使用代理/base URL 的程序。pVisor 会启动进程内
Gateway、注入子进程环境、等待子进程结束并关闭 Gateway。workspace 可以重复使用，
每次执行都会在 `PERSISTING_RUN_HOME` 下写入独立的 `run-<uuid>` 目录。

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

# 打开本地轨迹工作台。
persisting chronicle serve ./store

# 也可以直接打开包含 probing JSON 导出的目录。
persisting chronicle serve ./data

# 在同一个不可变 Catalog 快照中浏览多个 Dataset schema。
persisting chronicle serve --dataset live=./store \
  --dataset archive=s3://trajectory-bucket/archive
```

工作台只监听 loopback，以实时 Storyline 时间线展示轨迹，并可下钻对应的 canonical event
JSON；同时提供 HAR/OTLP 导出和面向整个目录的只读 SQL 工作区。

在只读查看模式下，工作台会自动识别所选目录中的 probing gateway step 数组和 ACTF
任务文档（`*.json`），无需先导入为 Lance 数据。

位置参数目录固定成为名为 `dataset` 的默认 SQL schema，与目录 basename 无关；稳定表为
`sources`、`runs`、`steps`、`tool_calls`、`events` 和 `trajectories`，旧的不带 schema
表名仍是兼容别名。可重复传入 `--dataset NAME=URI` 增加需要显式限定的 schema。每张数据表
都包含虚拟字段 `_file_`，既可精确匹配相对路径，也可用 `LIKE` 跨目录过滤：

```sql
SELECT _file_, COUNT(*) AS steps
FROM dataset.steps
WHERE _file_ LIKE 'cybergym_%.json'
GROUP BY _file_;
```

执行过 `gateway start` 或设置 `PERSISTING_CAPTURE_STORAGE` 后，`stats`、`replay`
和 `materialize` 可以省略 storage 参数。

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
