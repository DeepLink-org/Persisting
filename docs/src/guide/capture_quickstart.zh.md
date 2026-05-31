# Capture 快速上手

本页帮你在 **5–10 分钟内**跑通 `persisting traj`：用 `traj capture` / `traj proxy` 采集 Agent 对话，用 `traj stats` 等查看 Markdown 轨迹。

> 架构与概念见 [Capture 架构设计](../design/capture_design.zh.md)；命令参数见 [Traj 命令](../design/cli_trajectory_command.zh.md) 与 [Capture 子命令](../design/cli_capture_command.zh.md)。

---

## 你将得到什么

运行 `traj capture` 后，Persisting 会：

1. 在本地启动 **LLM 反向代理**（透明转发到上游模型）；
2. 把每次请求/响应写入 **Vortex 事件日志**（`-f vortex` 时；机器可读、可回放）；
3. 默认同时 **live 更新 Markdown**（`-f md`，人类可读，可 `tail -f`）；
4. 子进程退出后打印摘要，并写入对账文件 `.capture/reconcile.json`。

支持的实时采集客户端：**Claude Code**、**OpenAI Codex**（经代理）。Cursor 当前不支持。

---

## 0. 准备 CLI

Capture 由 Rust CLI 提供，需从源码构建：

```bash
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
cargo build -p persisting-cli -p persisting-engine
```

使用本地构建的二进制（macOS / Linux 示例）：

```bash
export PERSISTING="$(pwd)/target/debug/persisting"
# macOS：让 CLI 加载刚编译的 engine dylib
export PERSISTING_ENGINE_LIB="$(pwd)/target/debug/libpersisting_engine.dylib"
# Linux 将 .dylib 换成 libpersisting_engine.so
```

验证：

```bash
"$PERSISTING" --help
"$PERSISTING" traj --help
```

---

## 1. 最快路径：零 API Key 示例

仓库自带 **Mock LLM** 示例，无需真实模型密钥：

```bash
cd examples/capture-walkthrough
./run.sh
```

脚本会依次：构建 CLI → 启动 Mock LLM → `traj capture` 执行示例 Agent → 打印 `0001.md` 并校验。

产物路径：

```text
examples/capture-walkthrough/store/demo-agent/walkthrough-001/0001.md
```

想分步理解流程（两个终端）：

```bash
# 终端 A
./run.sh mock

# 终端 B
persisting traj capture -o ./store -c proxy.toml -f md -- python3 agent.py
./run.sh check
```

---

## 2. 真实模型：Claude Code / Codex

### 2.1 准备代理配置

复制并编辑示例配置（以 DeepSeek 为例）：

```bash
export DEEPSEEK_API_KEY=sk-...
```

配置见 [`examples/llm-proxy/deepseek.toml`](../../examples/llm-proxy/deepseek.toml)：

- `listen`：Capture 代理监听地址（默认 `127.0.0.1:19081`）
- `agent_id`：轨迹目录名（如 `deepseek-proxy`）
- `[[models]]`：上游路由（OpenAI / Anthropic 双协议）

### 2.2 用 `traj capture` 包装 Agent

**Claude Code**（自动注入网关环境变量）：

```bash
persisting traj capture \
  -o ./store \
  -c examples/llm-proxy/deepseek.toml \
  -f md \
  -- claude
```

**OpenAI Codex**：

```bash
persisting traj capture \
  -o ./store \
  -c examples/llm-proxy/deepseek.toml \
  -f md \
  -- codex
```

**自研脚本**（走 OpenAI SDK / `HTTP_PROXY`）：

```bash
persisting traj capture -o ./store -c your.toml -f md -- python3 your_agent.py
```

常用选项：

| 选项 | 含义 |
|------|------|
| `-o DIR` | 轨迹 store 根目录（默认 `.persisting/capture`） |
| `-c FILE` | 代理 TOML（`listen`、`models`、upstream） |
| `-f md` | **仅 Markdown**（默认，live upsert 到 `{session}.md`） |
| `-f vortex` | **仅 Vortex**（`events.vortex`）；需要人读视图时事后 `traj materialize` |
| `--debug` | 将代理请求打到 stderr 与 `.capture/debug.log` |
| `--` 之后 | 要执行的子命令 |

`traj capture` 是 **进程内代理**：子进程退出后代理自动关闭，**不需要** `traj proxy stop`。

---

## 3. 长期开发：`traj proxy` / `traj proxy start`

当你要在**多个终端**里反复启动 Agent、共用同一代理与 store 时，用 `serve`（前台）或 `start`（后台），而不是每次 `traj capture`。

### 3.1 三种运行形态怎么选

| 形态 | 命令 | 代理生命周期 | 典型场景 |
|------|------|--------------|----------|
| **一次性** | `traj capture` | 随子进程退出而结束 | 单次任务、CI、脚本包装 |
| **前台长期** | `traj proxy` | 当前终端阻塞，直到 `Ctrl+C` | 本地开发、看日志、调试 |
| **后台守护** | `traj proxy start` | 脱离终端，需 `traj proxy stop` | 长期挂着代理，多终端共用 |

三者共用同一套 TOML（`-c`）、store（`-o`）与 `-f` 格式。**同一 `-o` 目录不能同时**跑 `traj proxy`（或 `traj proxy start`）与 `traj capture`。

### 3.2 `traj proxy`（前台）

```bash
persisting traj proxy \
  -o ./store \
  -c examples/llm-proxy/deepseek.toml \
  -f md
```

启动后会打印 **使用说明**（与下文一致），包括：

- `proxy=http://…` — LLM 反向代理地址（对应 TOML 里 `listen`）
- `admin=http://…` — 管理接口（`admin_listen`，供 `traj proxy status` 查询）
- `store` / `agent_id` / `format`

若目录里已有 `daemon.env.json`（上次 `traj proxy start` 或 `traj capture` 写入的 API key 快照），会先提示 `applied daemon env snapshot`。

**在另一个终端**把 Agent 指到代理（以 `listen = 127.0.0.1:19081` 为例）：

```bash
export HTTP_PROXY=http://127.0.0.1:19081 HTTPS_PROXY=http://127.0.0.1:19081
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
export OPENAI_BASE_URL=http://127.0.0.1:19081/v1
export ANTHROPIC_BASE_URL=http://127.0.0.1:19081

# Claude Code（读 ANTHROPIC_BASE_URL / HTTP_PROXY）
claude

# Codex 不读 OPENAI_BASE_URL，需显式改 config：
codex -c 'openai_base_url="http://127.0.0.1:19081/v1"'
```

说明：

- `HTTP_PROXY` / `HTTPS_PROXY`：走代理的客户端会把 LLM HTTPS 流量经 Capture 转发并采集。
- `NO_PROXY`：避免本机 `127.0.0.1` 被误走代理环路。
- `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL`：OpenAI / Anthropic SDK 直连 Capture 网关路径。
- **Codex** 只认自己的 `config.toml`，必须用 `-c openai_base_url=…`（`traj capture` 会自动注入，`traj proxy` 时需自己写）。

每次新连上的 Agent 会话在 store 里通常是 **扁平 session 目录**（`{agent_id}/{session_id}/`），与 `traj capture` 的 `run-{timestamp}/` 布局不同；Markdown 可直接 `tail -f`：

```bash
tail -f store/deepseek-proxy/*/*.md
```

结束：在 `traj proxy` 终端按 **`Ctrl+C`**。

### 3.3 `traj proxy start`（后台守护进程）

与 `serve` 相同能力，但以子进程在后台运行：

```bash
persisting traj proxy start \
  -o ./store \
  -c examples/llm-proxy/deepseek.toml \
  -f md
```

输出示例：`traj proxy started: pid=… proxy=… admin=…`，以及与前台 `traj proxy` 相同的环境变量与运维提示。

| 命令 | 作用 |
|------|------|
| `persisting traj proxy status` | 当前活跃连接与会话（调 admin API） |
| `persisting traj proxy list` | 历史会话、token、成本估算 |
| `persisting traj proxy stop` | 停止后台代理 |

`list` / `status` / `stop` 可省略 `-o`：自动用最近一次 `start` 的目录，或环境变量 `PERSISTING_CAPTURE_STORAGE`。

加 `--debug` 时，守护进程 stderr 写入 `{store}/.capture/daemon.log`，并开启请求调试（类似 `traj capture --debug`）。

### 3.4 与 `traj capture` 的对比

| | `traj capture` | `traj proxy` / `traj proxy start` |
|--|---------------|-----------------------------------|
| 环境注入 | **自动**为子进程设置 `HTTP_PROXY`、`OPENAI_BASE_URL` 等 | 需在**新终端**手动 `export`（见上） |
| Codex | 自动追加 `-c openai_base_url=…` | 需手写 `-c` |
| 目录布局 | `run-{timestamp}/` + 对账 `reconcile.json` | 扁平 `{session_id}/` |
| 结束方式 | 子进程退出即停 | `Ctrl+C`（`traj proxy`）或 `traj proxy stop`（后台） |

若只想跑一轮对话，仍推荐 **`traj capture`**；长期多终端开发用 **`traj proxy`** 或 **`traj proxy start`**。

---

## 4. 输出目录长什么样

一次 `traj capture` 的典型布局：

```text
store/
├── .capture/
│   ├── sessions.json          # 会话索引
│   ├── reconcile.json         # run 结束 md ↔ Vortex 对账
│   ├── dead_letter.jsonl      # 采集失败事件（可 replay）
│   └── events.wal.jsonl       # 未 ack 事件 WAL（异常退出时）
└── {agent_id}/
    └── run-20260528-120000-123456789/
        ├── run-20260528-120000-123456789.md   # 主 Agent 对话
        ├── agent-{subagent-id}.md             # 子 Agent（若有）
        └── events.vortex                        # Vortex 事件日志
```

**阅读轨迹**：直接打开 `*.md`，或实时跟踪：

```bash
tail -f store/deepseek-proxy/run-*/run-*.md
```

---

## 5. 查看与运维：`traj`

Capture **生产**数据；`persisting traj`（别名 `trajectory`）用于 **离线读取 / 统计 / 修复**。

若已 `traj proxy start` 或设置了 `PERSISTING_CAPTURE_STORAGE`，`stats` / `replay` / `materialize` / `truncate` 可 **省略** `<STORAGE>` 参数。

```bash
# 统计（`-f md` 采集时直接读 Markdown；`-f vortex` 可先 materialize）
persisting traj stats ./store/deepseek-proxy/run-*/ --detail

# 重放为 JSON 行
persisting traj replay ./store --agent-id deepseek-proxy --session-id run-20260528-120000-123456789

# 从 Vortex 全量重建 Markdown（对账不一致或 md 损坏时）
persisting traj materialize ./store \
  --agent-id deepseek-proxy \
  --root-session-id run-20260528-120000-123456789 \
  --session-id run-20260528-120000-123456789
```

也可直接传 **run 目录** 或 `*.md` 路径，`traj` 会自动推断 `agent_id` / `session_id`。

---

## 6. 事后导入（可选）

补录 Claude Code 本地 JSONL（非实时路径）：

```bash
persisting traj import ./store \
  --provider ide \
  --since-days 7 \
  --project "$(pwd)"
```

`--dry-run` 仅统计；多会话匹配时用 `--session-id` 指定。详见 [Traj import 说明](../design/cli_capture_command.zh.md#5-traj-import)。

---

## 7. 常见问题

### 对账不一致

run 结束后查看 `.capture/reconcile.json`。若 md 与 Vortex 不一致：

```bash
persisting traj materialize ./store --agent-id <id> --root-session-id <run-id> --session-id <story-id>
```

### 采集失败事件

```bash
persisting traj replay-dead-letter -o ./store -f md
```

### `traj capture` 提示已有守护进程

同一 `output_dir` 下不能同时跑 `serve`/`start` 与 `run`。先 `traj proxy stop`，或换 `-o` 目录。

### `traj stats` 显示 0 轮 / 块数很少

确认路径指向 **run 目录**或主 `run-*.md`，不要只传 `store/{agent_id}/` 根目录。Capture run 下真实 Markdown 在 `run-{timestamp}/` 内。

### `serve` / `start` 下 Agent 连不上代理

1. 确认 `serve` 或 `start` 终端已打印 `proxy=http://…` 且无报错。
2. 在 **新终端**按启动横幅 `export` 环境变量（`traj capture` 会自动注入，`serve` 不会）。
3. Codex 需额外：`codex -c 'openai_base_url="http://127.0.0.1:PORT/v1"'`（端口与 TOML `listen` 一致）。
4. 检查是否误在同一 `-o` 上同时跑了 `traj capture`。

### 调试代理流量

```bash
persisting traj capture -o ./store -c your.toml -f md --debug -- claude
# 同时查看 store/.capture/debug.log
```

---

## 8. 命令速查

| 命令 | 用途 |
|------|------|
| `traj capture` | **推荐**：包装一次 Agent 命令，进程内代理 |
| `traj proxy` | 前台长期代理 |
| `traj proxy start` / `stop` | 后台守护进程 |
| `traj proxy list` | 会话列表、token、成本估算 |
| `traj proxy status` | 运行中连接与会话 |
| `traj import` | 从 IDE JSONL 事后导入 |
| `traj replay-dead-letter` | 重放失败采集事件 |
| `traj stats` | 统计 / `--detail` 逐轮 |
| `traj replay` | 事件 JSON 重放 |
| `traj materialize` | Vortex → Markdown 全量重建 |

---

## 下一步

- [Capture 架构设计](../design/capture_design.zh.md) — 产品定位、数据流、设计原则
- [轨迹 Markdown 格式](../design/trajectory_tlv_format.zh.md) — TLV 块与 frontmatter
- [Trajectory 命令](../design/cli_trajectory_command.zh.md) — `traj` 完整参数
- [分步示例代码](../../examples/capture-walkthrough/README.md) — Mock LLM 与校验脚本
