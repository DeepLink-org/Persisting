# Agent CLI 设计草案

> 状态：讨论中。`persisting` 是 Agent 执行的 shim：负责启动或接入 Agent、记录运行轨迹、回放轨迹，并作为通用 Agent 网关的管控入口。

## 定位

```text
Agent / Agent framework
        │
        ▼
persisting agent
        │
        ├── 执行控制
        ├── 轨迹捕获与持久化
        ├── 轨迹回放
        └── 网关管控
```

用户不需要先理解 Lance、Markdown、事件流、代理协议或内部 session。它们是 shim 的实现细节。

## 命令面

```text
persisting agent
├── execute       执行一个 Agent
├── bexecute      批量执行 Agent
├── capture       捕获已有 Agent 的轨迹
├── replay        回放已捕获的轨迹
└── gateway       管理通用 Agent 网关
```

这五个命令覆盖 Agent 的完整运行闭环，不在一级命令面暴露独立的 `traj`、`search`、`compute` 或存储管理命令。

## 公共约定

### Store 与 Run

每一次 `execute`、`bexecute` 或 `capture` 都创建或写入一个 **Run**。Run 是回放和诊断的基本单位。

```bash
persisting --store .persisting agent execute -- <COMMAND>
persisting agent replay <RUN>
```

- Store 由全局 `--store <DIR>` 或 `PERSISTING_STORE` 决定。
- 每个新 Run 有全局唯一 ID，并可选带人类可读名称。
- Agent、主会话、Subagent、事件和轮次均是 Run 内部结构。
- 命令成功后将 Run ID 输出到 stdout；进度、日志和警告输出到 stderr。

### 运行引用

```text
run_01J...         全局唯一 Run ID
@latest            当前 Store 最近一次 Run
name@latest        名为 name 的最近一次 Run（如后续保留命名）
```

### 输出

- 默认：面向终端的人读输出。
- `--output json`：单个结构化结果。
- `--output jsonl`：事件流或批量结果。
- `--quiet`：只输出稳定的 Run ID 或结果数据。

## `execute`

执行一个 Agent，并自动捕获其运行轨迹。

```bash
persisting agent execute -- claude
persisting agent execute --name coding -- claude --prompt "review this repository"
persisting agent execute --run nightly-review -- python agent.py
```

职责：

1. 创建 Run。
2. 配置或启动本地捕获链路。
3. 启动目标命令，并注入需要的环境变量或网关地址。
4. 等待子进程退出，写入 Run 终态。
5. 输出 Run ID、状态、持续时间和简短用量摘要。

`execute` 是默认的黄金路径。对用户而言，“执行”必然包含“记录”。

## `bexecute`

批量执行同一种 Agent 工作负载；每个 item 产生一个独立 Run，并共享批次级调度、并发和恢复状态。

```bash
persisting agent bexecute tasks.jsonl --command "python agent.py"
persisting agent bexecute tasks.jsonl --workers 4 --command "claude --prompt-file {input}"
persisting agent bexecute tasks.jsonl --resume
```

最小输入约定：每行一个 JSON object，必须有稳定的 `id`；其余字段可被模板引用。

```json
{"id":"repo-a","input":"/work/repo-a"}
{"id":"repo-b","input":"/work/repo-b"}
```

职责：

- 有界并发执行。
- 每个 item 的稳定 ID 对应一个 Run。
- 支持 `--resume`，跳过已有终态 Run。
- 输出批次摘要；使用 `--output jsonl` 时每行输出一个 Run 的终态。

`bexecute` 不试图定义 Agent 算法或替代用户的任务框架；它只提供 Agent 运行的批量 shim。

## `capture`

接入一个已经运行或由用户自行启动的 Agent，记录其轨迹但不负责其生命周期。

```bash
persisting agent capture --config proxy.toml
persisting agent capture --gateway http://127.0.0.1:19081
persisting agent capture import claude --since 7d
```

初期可包含两种模式：

- **live**：启动前台采集代理，等待外部 Agent 连入。
- **import**：将 IDE、网关或 JSONL 历史记录归一为 Run。

`execute` 用于“我希望 Persisting 启动 Agent”；`capture` 用于“Agent 已由别处启动，我只希望 Persisting 记录它”。

## `replay`

读取一个 Run，按时间线重建其可读轨迹；默认不重新调用模型、不重新执行工具。

```bash
persisting agent replay run_01J...
persisting agent replay @latest
persisting agent replay run_01J... --events
persisting agent replay run_01J... --format markdown
```

默认输出应包括：用户输入、模型响应、工具调用及结果、Subagent 边界、错误和时间信息。

高级选项：

- `--events`：输出 canonical event stream。
- `--format markdown|json|jsonl`：选择输出格式。
- `--follow`：对仍在运行的 Run 持续输出新增事件。

未来若需要“重新执行”能力，应另行设计 `--reexecute` 或独立命令；不能让 `replay` 同时表示读取历史和再次产生副作用。

## `gateway`

通用 Agent 网关的生命周期、路由和观测入口。

```bash
persisting agent gateway serve --config gateway.toml
persisting agent gateway start --config gateway.toml
persisting agent gateway status
persisting agent gateway stop
persisting agent gateway routes
```

网关职责：

- 接收 Agent 到模型/工具服务的请求。
- 根据配置路由到上游。
- 为 `execute` 和 `capture` 提供统一的流量接入点。
- 生成可关联到 Run 的捕获事件。
- 暴露健康状态、活跃连接和路由信息。

网关不负责查询历史 Run；历史读取始终使用 `replay`。

## 命令边界

| 命令 | 谁启动 Agent | 是否创建/写入 Run | 主要用途 |
|---|---:|---:|---|
| `execute` | Persisting | 是 | 单次执行并记录 |
| `bexecute` | Persisting | 是，每 item 一个 | 批量执行并记录 |
| `capture` | 用户/外部系统 | 是 | 接入或导入已有 Agent 轨迹 |
| `replay` | 无 | 否，只读 | 查看或流式读取历史轨迹 |
| `gateway` | 无；管理网关进程 | 间接 | 统一流量入口与管控 |

## 非目标

- 不把底层存储格式、索引参数、TTAS、参数传输或 KV Cache 暴露为 Agent CLI 的主命令。
- 不在首版中实现任意 Agent runtime 或 Agent 编排 DSL。
- 不让 `replay` 默认产生网络、模型或工具副作用。

## 当前命令的迁移方向

| 当前命令 | 目标命令 |
|---|---|
| `traj capture -- <CMD>` | `agent execute -- <CMD>` |
| `traj proxy` | `agent capture` 或 `agent gateway serve`，取决于是否承担通用路由 |
| `traj proxy start/status/stop` | `agent gateway start/status/stop` |
| `traj import` | `agent capture import` |
| `traj replay` / `traj stats` | `agent replay` |
| `compute` | `agent bexecute`（仅覆盖批量 Agent 执行的部分） |

## 待决策

1. `bexecute` 是否保留这个短名，还是采用 `batch-execute` 并提供 `bexecute` 别名？
2. `capture` 的 live proxy 是否直接复用 `gateway`，还是保留轻量、仅记录的本地代理？
3. Run 的命名策略：自动 ID、`--run <NAME>`，还是二者同时支持？
4. `bexecute` 的任务输入和命令模板语法应采用 JSONL、YAML，还是兼容现有 compute plan？
5. 是否需要 `agent replay --interactive`，用可确认的方式重放工具调用或模型请求？
