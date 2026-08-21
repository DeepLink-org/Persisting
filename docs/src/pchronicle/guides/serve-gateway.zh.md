# `pchronicle serve` 的 Gateway 转发、改写与捕获

`pchronicle serve` 可以启动本地 LLM Gateway，并可选择是否同时启动只读 Warehouse。Gateway 对每个请求
选择 upstream，按需改写模型和 wire protocol，再以客户端协议返回响应，同时把 canonical
capture events 追加到一个已挂载 Dataset。Warehouse Web UI 和 API 仍然保持只读。

当 Agent 或 SDK 已经能够调用 OpenAI、Anthropic 或 Gemini 兼容的 base URL，而你希望不
启动 pVisor Run 就捕获这些流量时，可以使用这个模式。如果 Gateway 需要与 Agent 执行共享
生命周期和隔离边界，应改用 [pVisor 捕获](../../pvisor/guides/capture.md)。

## 两份配置文件

Gateway 模式刻意分离存储配置和转发配置：

| 输入 | 负责的内容 |
| --- | --- |
| 传给 `--config` 的 `warehouse.toml` | 挂载 Dataset，以及默认捕获目标 |
| 传给 `--gateway` 的 `gateway.toml` | Gateway listener、模型路由、凭据、捕获级别和网络策略 |
| CLI 参数 | 选择写入 Dataset、本地 Gateway 状态目录、实时 Markdown 和前台调试 |

两份 TOML 中的未知字段都会被拒绝。Gateway 配置必须使用 TOML，不接受其他扩展名。

## 最小配置

创建 `warehouse.toml`：

```toml
default_dataset = "captures"

[[datasets]]
name = "captures"
uri = "./data/captures"
```

相对 Dataset 路径以 `warehouse.toml` 所在目录为基准解析。

创建 `gateway.toml`：

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
agent_id = "local-agent"
capture_level = "dialogue"

[[models]]
name = "deepseek-chat"
provider = "openai"
upstream = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[[models]]
name = "*"
forward = "deepseek-chat"
```

导出凭据，然后启动两个服务：

```bash
export DEEPSEEK_API_KEY=sk-...

pchronicle serve \
  --config warehouse.toml \
  --listen 127.0.0.1:8080 \
  --gateway gateway.toml \
  --gateway-stream-markdown
```

该命令会启动三个 loopback listener：

- `127.0.0.1:8080`：Warehouse Web UI 和只读 API；
- `127.0.0.1:8787`：LLM Gateway；
- `127.0.0.1:8788`：Gateway 状态和 session API。

省略 `--listen` 时只启动 Gateway listener 与 capture sink，不会创建 Warehouse HTTP
endpoint。

将 Agent 或 SDK 的 base URL 指向 `http://127.0.0.1:8787/v1`。例如：

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-persisting-session-id: example-session' \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

可以独立于 Warehouse 检查 Gateway：

```bash
curl http://127.0.0.1:8788/admin/status
curl http://127.0.0.1:8788/admin/sessions
```

## 请求生命周期

Gateway 将转发、改写和捕获组成一条请求管线：

```text
client request
  -> 识别客户端协议和模型
  -> 选择第一个命中的 models[] 路由
  -> 对配置的模型路由执行授权
  -> 按需改写模型和协议
  -> 构造 upstream URL 和认证信息
  -> 转发到 upstream
  -> 按需把响应翻译回客户端协议
  -> 记录请求以及完整或流式响应
```

Capture metadata 会区分客户端请求模型和实际 upstream 模型，并记录是否发生模型改写。
捕获不会取代转发：upstream error 会返回客户端，同时结束对应的 capture call。

## Gateway 顶层字段

| 字段 | 必填 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `listen` | 是 | — | LLM proxy listener。内嵌模式只接受 loopback 地址；端口 `0` 表示选择可用端口。 |
| `admin_listen` | 否 | `127.0.0.1:9876` | `/admin/status` 和 `/admin/sessions` 的 listener，同样只允许 loopback。 |
| `agent_id` | 否 | `default` | 写入捕获记录的 Agent identity；请求能够提供更具体身份时使用请求身份。 |
| `session_header` | 否 | `x-persisting-session-id` | 用于把请求归入 session 的 header。 |
| `capture_level` | 否 | `dialogue` | 保留请求与响应内容的级别：`summary`、`dialogue` 或 `full`。 |
| `debug` | 否 | `false` | 向状态目录写入 Gateway 诊断；日志可能包含有界的请求和响应 body。 |
| `models` | 是 | — | 有序模型路由列表。 |
| `network` | 否 | `mode = "public"` | 显式 forward-proxy 流量的策略。 |

共享 Gateway schema 还接受 `[overlay]`，但 `pchronicle serve` 不会创建或 apply 文件系统
overlay。Overlay 生命周期属于 [pVisor](../../pvisor/guides/execution.md)。

### 捕获级别

- `summary` 保存协议元数据和字节数，不保存用户或 assistant 文本；
- `dialogue` 保存用户和 assistant 对话，是默认值；
- `full` 还会保存解析后的请求和响应 body。只有在能够接受额外存储量和敏感内容暴露时才应
  使用。

## 模型路由

路由按文件顺序匹配，第一个命中的 `name` 生效。`name` 可以是精确模型名、`prefix*`、
`*suffix` 或兜底规则 `*`。

| 字段 | 含义 |
| --- | --- |
| `name` | 必填的匹配规则或精确目标模型名；不能重名。 |
| `provider` | `openai`、`anthropic`、`gemini`、`vertex`、`bedrock`、`azure`、`copilot` 或 `custom`；默认 `openai`。 |
| `upstream` | Upstream base URL；适用时应包含 API prefix。未设置 `forward` 时必填。 |
| `upstream_anthropic` | `/v1/messages` 使用的可选 Anthropic-compatible base；未设置时使用 `upstream`。 |
| `api_key_env` | 由 `pchronicle` 进程读取的环境变量，推荐使用这种凭据来源。 |
| `api_key` | 内联凭据。实现支持，但不应把 secret 提交到配置文件。 |
| `forward` | 另一个路由的精确名称；重写请求模型并使用目标路由的 upstream。 |

同一路由不能同时设置 `upstream` 和 `forward`。Forward 必须指向带有 upstream 的路由，且
不能形成多级链。如果配置或指定环境变量没有提供 key，Gateway 可以使用兼容的客户端认证
header；两者都没有时，请求会在转发前失败。

多 Provider 示例：

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
capture_level = "dialogue"

[[models]]
name = "claude*"
provider = "anthropic"
upstream = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[[models]]
name = "gpt*"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[[models]]
name = "gemini*"
provider = "gemini"
upstream = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
```

## 转发与模型改写

路由选定后，Gateway 会根据 `upstream` base 规范化有效请求路径。Upstream 以 `/v1` 结尾，
客户端请求 `/v1/chat/completions` 时，结果仍是一个 `/v1/chat/completions`，而不是
`/v1/v1/chat/completions`。Passthrough 请求会保留 query parameter。

Gateway 转发端到端 header，但移除 `Host`、`Content-Length`、hop-by-hop header、proxy
authentication 和客户端 LLM credential，然后按 Provider 写入 OpenAI Bearer、Anthropic
`x-api-key` 或 Gemini `x-goog-api-key`。Redirect 会返回客户端，不会在 Gateway 内部继续跟随。

`forward` 可以把客户端模型改写到一个精确目标路由：

```toml
[[models]]
name = "echo-upstream"
upstream = "http://127.0.0.1:19080/v1"

[[models]]
name = "*"
forward = "echo-upstream"
```

此时对 `client-model` 的请求会以 `"model":"echo-upstream"` 到达目标。Capture metadata
保留两种 identity 并标记改写。Forward target 必须定义 `upstream`，不能继续 forward，也不会
再次进行 pattern match。路由采用 first-match，因此具体 pattern 应放在宽泛 pattern 之前。

## 协议改写

Gateway 根据客户端 path 和目标路由选择 protocol bridge。普通响应和 SSE stream 都会转换：

| 客户端协议 | 目标路由 | Upstream 协议 |
| --- | --- | --- |
| Chat Completions | 非 Gemini | Chat Completions passthrough |
| Anthropic Messages | 设置了 `upstream_anthropic` | 原生 Messages passthrough |
| Anthropic Messages | 未设置 `upstream_anthropic` 的 OpenAI-compatible route | Chat Completions |
| OpenAI Responses | 原生 OpenAI 或 Azure OpenAI | Responses passthrough |
| OpenAI Responses | 其他 OpenAI-compatible upstream | Chat Completions |
| Chat Completions、Messages 或 Responses | `provider = "gemini"` | Gemini `generateContent` 或 `streamGenerateContent` |

需要转换时，Gateway 会改写 request path 和 body，再把 response 或受支持的 error envelope
渲染回客户端协议。Bridge 保留常见 message、tool call、usage、reasoning 和 streaming event，
但不承诺所有 Provider-specific extension 都能无损保留。客户端依赖这些字段时应使用
passthrough。

## 使用 Echo upstream 测试

仓库提供了一个确定性的 Rust Echo server，可以在没有 API key 和真实模型服务的情况下测试
真实 HTTP 转发。它支持 `/echo`、Chat Completions、Messages、Responses、Gemini 及对应的
streaming 形式；最后一条用户文本决定 assistant 输出。

从源码仓库启动：

```bash
just echo

# 等价的已安装命令：
pchronicle echo --listen 127.0.0.1:19080 --encoding plain
```

将 Gateway route 指向 Echo server：

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
capture_level = "full"

[[models]]
name = "echo-upstream"
provider = "openai"
upstream = "http://127.0.0.1:19080/v1"

[[models]]
name = "*"
forward = "echo-upstream"
```

默认情况下，assistant 直接返回最后一条用户文本。可以让单个请求改为返回标准 Base64：

```bash
curl http://127.0.0.1:8787/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-persisting-echo-encoding: base64' \
  -d '{
    "model": "client-alias",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "hello"}]
  }'
```

这条 Messages 请求会被转换成 Chat Completions，模型改写成 `echo-upstream`，响应再转换回
Messages，其中的文本是 `aGVsbG8=`。加入 `"stream": true` 可以通过 SSE 测试同一条路径。
请求 header 接受 `plain` 或 `base64`；`--encoding` 设置服务器默认值。`pchronicle echo` 只允许
loopback listener，用于确定性的本地 Gateway 测试。

## 网络策略

`[network]` 控制 `CONNECT` 和 absolute-URI request 等显式 proxy 流量。配置在
`models[].upstream` 中的目标是 Gateway 自己拥有的路由，不是 Agent egress grant；应通过
模型路由列表限制 LLM surface。

可用模式包括：

```toml
[network]
mode = "no-network"
```

```toml
[network]
mode = "allowlist"
allowed_hosts = ["pypi.org", "files.pythonhosted.org", "*.github.com"]
```

`public` 是默认值；`no-network` 拒绝显式 proxy egress；`allowlist` 要求请求命中
`allowed_hosts` 或结构化 `[[network.rules]]`。显式 `[[network.deny_rules]]` 的优先级高于
allow。若策略必须成为 Agent 进程不可绕过的边界，应使用 pVisor；`pchronicle serve` 只能
控制客户端主动发送到 Gateway 的流量。

## Dataset 与状态目录选择

捕获目标按以下顺序选择：

1. `--gateway-dataset NAME`；
2. `warehouse.toml` 中的 `default_dataset`；
3. 只挂载了一个 Dataset 时，选择该 Dataset。

如果无法得到唯一 Dataset，启动会失败。最终名称必须对应静态 `[[datasets]]` mount。

Canonical events 直接追加到目标 Dataset。Gateway runtime state 与其分离，其中包括 session
index、debug log 和可选实时 AgenticMD projection：

- 本地 Dataset 未显式配置时，state directory 默认使用 Dataset path；
- `s3://...` 等 object-store Dataset 必须提供可写的本地 `--gateway-state DIRECTORY`；
- `--gateway-stream-markdown` 会在 state directory 中维护实时 AgenticMD projection。

即使使用本地 Dataset，也可以显式分离暂态 Gateway 文件：

```bash
pchronicle serve \
  --config warehouse.toml \
  --gateway gateway.toml \
  --gateway-state ./.pchronicle-gateway \
  --gateway-stream-markdown
```

## CLI 优先级与安全边界

- `--listen` 只配置 Warehouse；Gateway listener 始终来自 `gateway.toml`；
- `--debug`（也接受 `--gateway-debug`）即使在 `debug = false` 时也会开启 Gateway 调试；没有
  CLI 参数能够强制关闭配置中已开启的 debug；
- `--gateway-dataset`、`--gateway-state` 和 `--gateway-stream-markdown` 是组合层配置，不是
  Gateway TOML 字段；
- Warehouse、Gateway 和 admin listener 都必须是 loopback 地址；这些服务不提供
  authentication 或 authorization 边界；
- debug 和 `full` 捕获可能保留敏感的请求或响应内容，应相应保护状态目录和 Dataset。

## 查看新捕获

Gateway writer 排空后，events 会持久写入目标 Dataset。`serve` 的 projection supervisor
会发现 canonical 变化，更新确定的同级 Storyline Store，然后完整重建并原子切换 Warehouse
Catalog。projection 或 refresh 失败会有界重试并保留旧的可查询 Catalog；两者都不阻塞
durable capture write。`POST /api/catalog` 仍可用于显式手工刷新，但新捕获可见性不再依赖它。
收到 `SIGINT` 或 `SIGTERM` 时，
`pchronicle serve` 会停止两个服务，并在退出前完成 Gateway capture writer。

精确命令参数见 [`pchronicle` CLI 参考](../reference/cli.md)；刷新背后的存储模型见
[Dataset、Source 与 Snapshot](../concepts/dataset-and-source.md)。
