# RFC-0002: Events Format（`events`）

| Field | Value |
|---|---|
| **Status** | Draft |
| **Schema / format name** | `events`（逻辑文档）；物理存储常为 `events.lance` |
| **Date** | 2026-07-30 |
| **Component** | Gateway + pChronicle |
| **Implements** | `persisting-gateway` `CaptureRecord` / `EventRow` · `persisting-pchronicle` `formats/events.rs` |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [Capture 管线](../design/gateway.md) · [轨迹存储](../design/trajectory.md) |

---

## 摘要

**`events`** 是 Persisting 轨迹的 **canonical 事实流（SoT）**：按时间（`seq`）追加的底层记录，**优先刻画原始 HTTP（或等价传输层）交换**，而不是已经折叠好的对话轮次。

设计取向：

> 先保住「线上真实发生过什么」（方法、URL、头、正文、状态、时序），再由上层（Storyline / Markdown / ATIF）去解释「这对人意味着什么」。

因此：

| 层 | 角色 | 可否有损 |
|---|---|---|
| **`events`** | 原始交换日志（HTTP-first） | **SoT，目标可回放** |
| **`storyline`** | Normal / 互操作枢纽 | 可有损投影 |
| `agenticmd` / `openai_msg` / `atif` / `actf` | 外围视图 / 训练交换 | 可有损；带 ACTF provenance 时可无损恢复 ACTF 数据模型 |

`events` 进出其它 chronicle 格式时仍 **只经 storyline**；但 **回放、重放、审计、协议级恢复 MUST 读 events**，MUST NOT 依赖 storyline roundtrip。

本 RFC 规定：

1. HTTP-first 的定位与分层职责；
2. 逻辑事件 `EventRecord`、信封 `EventsDocument`、Lance `EventRow`；
3. `payload` 的 wire 字段约定（request / response）；
4. 与 Storyline 的投影关系（摘要字段只是加速，不是真相）。

---

## 动机

Agent 轨迹若只存「user/assistant 文本 + tool_calls」，会丢掉：

- 真实 URL / method / status / header（鉴权、路由、provider 切换）；
- 原始 body（含协议特有字段、tool schema、多模态 parts）；
- 流式分片、重试、取消、非 2xx、内部探测流量；
- 足以 **原样重放 HTTP** 或 **换协议解析器重解** 的材料。

Persisting Gateway 的主入口是代理流量。`events` 应对齐这一现实：**一行事件 ≈ 一次可定位的 HTTP 方向记录**（request 或 response），外加关联键（`session_id` / `call_id` / `trace_id`）。

对话摘要（`user_content` / `assistant_content`）可以写进 payload 作为 **可选索引/加速字段**，但：

- MUST NOT 取代 `http.request_body` / `http.response_body`（或等价 wire 字段）成为唯一正文；
- 当摘要与 wire body 冲突时，**以 wire body 为准**。

---

## 设计目标

| 目标 | 说明 |
|---|---|
| **HTTP-first SoT** | 默认按原始请求/响应保存，使回放与再解析成为一等能力 |
| **Replayable** | 在凭证策略允许的前提下，应能从 events 重建「对同一 endpoint 再发一次等价请求」所需信息 |
| **Re-derivable views** | Storyline / Markdown / ATIF 视为可从 events **重新投影**的视图 |
| **Correlation envelope** | 顶栏保留 `session_id` / `call_id` / `trace_id` 等关联键，不把故事语义塞进 wire |
| **Append-only** | `seq` 单调；不原地改写既有 wire payload（旁路列如 judge 除外） |
| **Hub via storyline** | 与其它外围格式互转 MUST 经 storyline；无损路径仍是读 events |

非目标：

- 不把 events 做成 ATIF step / Storyline Turn 的镜像；
- 不要求每条事件都是「对人友好的一句对话」；
- 不在本 RFC 规定 TLS 密钥、cookie 明文等敏感材料的强制落盘策略（见安全节）。

---

## 分层：原始交换 vs 故事投影

```text
                    ┌─────────────────────────────┐
  proxy / import →  │  events 流（HTTP wire）      │
                    └──────────────┬──────────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    ▼                             ▼
              记录 append                   触发 handlers
                    │                             │
                    ▼                             ▼
              events.lance                  storyline（hub）
              （SoT / replay）                     │
                                    ┌─────────────┼─────────────┐
                                    ▼             ▼             ▼
                               agenticmd       openai_msg      atif
                                    └──────── 落盘 / 物化 ──────┘
```

| 问题 | 应落在 |
|---|---|
| 「当时请求的 path / body 是什么？」 | **events** |
| 「这一轮对用户说了什么？」 | storyline / agenticmd |
| 「能否拿去 SFT？」 | atif / openai_msg（经 storyline） |
| 「能否按 call_id 重放 upstream？」 | **events** |

与既有 Capture 表述的关系：协议差异可以在 **进入 Story 读模型之前**消化；但 **进入 events 之前不应过度消化**——解析器可以后置、可升级，原始字节/JSON body 应尽量留下。

---

## 术语

| 术语 | 含义 |
|---|---|
| **Wire record** | 一次 HTTP（或同类）方向的原始交换描述：method/url/headers/body/status/timing… |
| **EventRecord** | 逻辑事件 = 关联信封 + `kind` + wire-oriented `payload` |
| **CaptureRecord** | Gateway 采集管线内部兼容表示（演进目标与本 RFC 对齐） |
| **EventRow** | Lance 列存行：索引列 + `payload_json` |
| **Summary fields** | `user_content` / `assistant_content` / `model` 等可选派生字段 |
| **Replay** | 用 wire 字段重建并再次发出（或模拟）等价交换 |

---

## Schema：逻辑事件 `EventRecord`

编码：UTF-8 JSON object。`EventRecord` 由 pChronicle 唯一定义；Gateway 的
`CaptureRecord` 只是该类型的兼容别名，pVisor 生命周期事件也直接使用同一类型。
一行事件 MUST 包含 `seq`、`source`、`kind`、`payload`。

### 关联信封（顶栏）

| Field | Type | Status | Description |
|---|---|---|---|
| `schema_version` | integer | Required for new writes | pChronicle event schema version |
| `event_id` | string | Required for new writes | append 边界生成的稳定事件身份 |
| `run_id` | string | Required for new writes | 事件所属逻辑 Run；旧 capture row 读取时可缺省 |
| `attempt_id` | string | Optional | 生命周期事件所属 Attempt |
| `storyline_id` / `turn_id` | string | Optional | narrative 维度，与 Attempt 正交 |
| `timestamp_unix_ms` | integer | Required for new writes | 机器可比较的观测时间 |
| `producer` | string | Required for new writes | 产生该记录的组件 |
| `seq` | integer | Required | dataset / 会话内单调序号 |
| `source` | string | Required | 如 `persisting-proxy`、`persisting-gateway` |
| `kind` | string | Required | 见 kind 节；HTTP 方向优先 |
| `timestamp` | string | Optional | RFC3339（事件观测时间） |
| `session_id` | string | Optional | ≈ Storyline `story_id` |
| `agent_id` | string | Optional | |
| `call_id` | string | Optional | 一次调用关联键（request/response 配对） |
| `trace_id` | string | Optional | |
| `parent_call_id` | string | Optional | 调用树 |
| `parent_uuid` | string | Optional | |
| `subagent_id` / `parent_agent_id` / `branch` | string | Optional | 子代理 / 分支 |

顶栏 **SHOULD NOT** 承载完整对话文本；正文在 `payload` 的 wire 字段中。

### `payload`：HTTP-first wire 对象

推荐统一放在 `payload.http`（或顶层扁平兼容键，见兼容节）。**完整回放**所需字段：

#### Request 方向（`kind` = `http.request` 或兼容 `llm.request`）

| Field | Type | Status | Description |
|---|---|---|---|
| `http.method` | string | Required | `GET` / `POST` / … |
| `http.url` | string | Required | 完整 URL 或 path+query；与 `host` 可拆分 |
| `http.path` | string | Recommended | 规范化 path（便于索引） |
| `http.query` | object / string | Optional | |
| `http.headers` | object \| array | **Required**（记录时） | **完整请求头**（可脱敏）；与 body 同为回放必需 |
| `http.request_body` | string \| object \| null | Recommended | **原始请求体**（JSON 对象或 base64/utf8 字符串） |
| `http.body_encoding` | string | Optional | `json` \| `utf8` \| `base64` \| `sse-wire` … |
| `http.content_type` | string | Optional | |
| `timing.started_at` | string | Optional | |
| `timing.duration_ms` | number | Optional | |

#### Response 方向（`kind` = `http.response` / `http.response.stream` 或兼容 `llm.response*`）

| Field | Type | Status | Description |
|---|---|---|---|
| `http.status` | integer | Required | HTTP 状态码 |
| `http.headers` | object \| array | **Required**（记录时） | **完整响应头**（可脱敏） |
| `http.response_body` | string \| object \| null | Recommended | **原始响应体**或流式拼接线 |
| `http.body_encoding` | string | Optional | 含 `sse` / `sse-wire` |
| `http.truncated` | boolean | Optional | 是否流式 |
| `timing.ttft_ms` / `timing.duration_ms` | number | Optional | |


#### 连接与客户端（长连接 / 对端身份）

HTTP-first 回放不仅需要单次 request/response，还需要知道 **连接是否复用** 以及 **谁连上来**。记录时 MUST 尽量写入：

| Field | Type | Status | Description |
|---|---|---|---|
| `connection.http_version` | string | Recommended | 如 `HTTP/1.1`、`HTTP/2.0` |
| `connection.persistent` | boolean | Recommended | 是否按协议/头判定为 keep-alive / 长连接 |
| `connection.connection_header` | string | Optional | 原始 `Connection` 头（如 `keep-alive` / `close`） |
| `connection.keep_alive` | string | Optional | 原始 `Keep-Alive` 头（超时/max 等） |
| `connection.upgrade` | string | Optional | `Upgrade`（如 `websocket`） |
| `client.peer` | string | **Required**（代理路径） | 客户端 `ip:port`（`ConnectInfo`） |
| `client.peer_ip` | string | Recommended | |
| `client.peer_port` | integer | Recommended | |
| `client.pid` | integer | Optional | 本机 peer 反查进程 PID（best-effort） |
| `client.command` | string | Optional | 进程命令行 |
| `client.machine_fp` | string | Optional | 机器指纹 |
| `client.user_agent` | string | Optional | 从请求头抽取，便于无 PID 时对照 |

判定 `persistent` 的建议规则：

1. 若 `Connection: close` → `false`；
2. 若 `Connection` 含 `keep-alive` → `true`；
3. 否则：`HTTP/1.1+` 默认 `true`，`HTTP/1.0` 默认 `false`；
4. `Upgrade: websocket` 等升级连接单独用 `upgrade` 标明，不假装成普通 keep-alive 复用。

说明：

- 会话级 `session-meta.yaml`（`SessionClientMeta`）可继续存在；**事件级**仍 MUST 冗余写入 `client.*`，以便单条 event 可自描述回放。
- hop-by-hop 头（`Connection` / `Keep-Alive`）转发上游前可能被剥离，但 **events 记录 MUST 保留客户端原始值**。
- 上游侧长连接信息可从 response `headers` 的 `Connection`/`Keep-Alive` 再读；可另写 `connection.upstream_*`（Optional）。

#### 可选摘要 / 索引（非 SoT）

| Field | Type | Status | Description |
|---|---|---|---|
| `model` | string | Optional | 从 body/header 抽取，便于 Lance 列 |
| `protocol` | string | Optional | 如 `chat.completions`、`anthropic.messages` |
| `provider` | string | Optional | |
| `user_content` | string | Optional | 可见 user 摘要；冲突时以 request_body 为准 |
| `assistant_content` | string | Optional | 可见 assistant 摘要；冲突时以 response_body 为准 |
| `usage` | object | Optional | token 等；能从 body 再解析则视为缓存 |
| `_tlv` | object | Optional | 自 Markdown compact 注入 |

**Normative**：

1. 记录 HTTP 交换时，实现 MUST **同时**持久化：
   - **headers**（`payload.http.headers` 或兼容扁平键 `payload.headers`）；
   - **body**（在 `capture_level` 允许完整 body 时：`request_body` / `response_body` 或等价 `body`）；
   - **连接与客户端**：`connection.*`（至少能推导 `persistent`）与 `client.peer`（代理路径必填）。
2. headers 与 body 同为回放材料：缺 headers 的事件与缺 body 一样，MUST 视为 **degraded**（`payload.degraded = true`），MUST NOT 宣称可完整 HTTP 回放。
3. 敏感头（`Authorization` / `Cookie` / `Set-Cookie` / `X-Api-Key` 等）MUST 按红线脱敏或外置，并设置 `headers_redacted: true`；脱敏后仍 MUST 保留头名字面量（值为占位符），以便知道当时发过哪些头。
4. Storyline / 训练格式导出 MUST 容忍从 wire body + headers 再解析；MUST NOT 假定摘要字段永远存在。

### 内存批 `EventsDocument`（非正式文件格式）

| Field | Type | Status | Description |
|---|---|---|---|
| `format` | string | Required | `"events"` |
| `events` | array | Required | `EventRecord[]` |
| `session_id` / `agent_id` | string | Optional | 文档级默认 |

### 物理行 `EventRow`（Lance）

| Column | 说明 |
|---|---|
| `seq` / `timestamp` / `kind` / `source` | 索引 |
| `session_id` / `agent_id` / `call_id` / `trace_id` / `parent_call_id` | 关联过滤 |
| `model` | 可选反规范化 |
| `payload_json` | **完整 EventRecord/CaptureRecord JSON（含 wire）** |

---

## `kind` 约定

### 推荐（HTTP-first）

| kind | 含义 |
|---|---|
| `http.request` | 一次 outbound/inbound HTTP 请求观测 |
| `http.response` | 对应响应（非流或已聚合） |
| `http.response.stream` | 流式响应片段或草稿聚合 |
| `http.cancel` | 取消 / 中断 |
| `session.started` / `session.ended` | 会话生命周期（可无 http 块） |
| `note` | 运维注解 |

### 兼容别名（现有 Capture）

| 现有 kind | 视为 |
|---|---|
| `llm.request` | `http.request` 的语义别名（payload 仍应尽量含 path/body） |
| `llm.response` | `http.response` |
| `llm.response.stream` | `http.response.stream` |

新写入 SHOULD 使用 `http.*`；读路径 MUST 同时接受 `llm.*`。  
`llm.*` **不**意味着可以只存对话摘要——即便使用别名，仍按 HTTP-first 要求保存 wire。

---

## 回放与恢复（Replay）

从一对共享 `call_id` 的 request/response 事件，消费者 SHOULD 能恢复：

1. **HTTP 重放**：method + url + headers′ + body → 得到新的 response（headers′ 经红线剥离）；
2. **协议再解析**：换用新的 Anthropic/OpenAI/… 解析器从 body 重建 messages / tool_calls；
3. **视图重建**：重新投影 Storyline / Markdown / ATIF；
4. **时序重建**：按 `seq` / timestamp 恢复并发与流式顺序。

无法回放的常见原因（实现 MUST 在文档/元数据中可诊断）：

- body 因 `capture_level` 被省略（`degraded`）；
- 敏感头被剥离且无替代凭证注入通道；
- 仅存 SSE 摘要未存 wire 帧；
- 只从 storyline 合成的「假 events」（无原始 wire）。

---

## 安全与红线

HTTP-first **不**等于无条件落盘密钥。

| 类别 | 默认建议 |
|---|---|
| `Authorization` / API key / cookie | 红线：哈希、删除或外置 secret store；回放时再注入 |
| 用户内容 / 工具结果 | 按产品保留策略 |
| 多模态像素 | 可旁路对象存储；events 内留引用与 hash |

`payload.http.headers_redacted: true`（可选）表示头已经过红线处理。

---

## Wire 形态与探测

| 形态 | 地位 |
|---|---|
| **Lance `events.lance/`** | **唯一一等存储 / 采集格式（SoT）** |
| JSONL / JSON / `EventsDocument` 序列化 | **非正式**：仅调试导出或测试夹具；**不是** chronicle 互操作格式 |

pChronicle：`into_storyline` / `convert` **不接受** events 的字符串输入；从 Lance 加载为内存 `EventRecord` 后再 `events_to_storyline`。

探测：

| 方式 | 规则 |
|---|---|
| 路径 | 仅 `events.lance`（或 basename 含 `event` 的 `.lance`） |
| 内容 | **不**把 CaptureRecord 形 JSON 判定为 `events` |

别名：`events` / `lance` / `bin` / `events.lance`。


---

## 与 Storyline 的关系

```text
events (HTTP wire) ──interpret──► storyline ──synthesize──► events′ (often degraded)
```

| 方向 | 期望 |
|---|---|
| events → storyline | 从 wire（及可选摘要）折叠 Turn/Call；填写 `event_seqs` |
| storyline → events′ | 合成最小交换；**通常 degraded**，除非 storyline 显式携带完整 wire（不默认） |

规范性：

1. 互转其它外围格式 MUST 经 storyline。
2. 宣称「可回放」时 MUST 基于非 degraded 的原始 events。
3. 折叠器 SHOULD 优先解析 `http.request_body` / `http.response_body`，摘要字段仅作快路径。

---

## 兼容现有 Capture payload

今日实现常见扁平键（仍合法）：

| 现有键 | HTTP-first 对应 |
|---|---|
| `headers` | `http.headers`（**记录 MUST**） |
| `path` | `http.path` / `http.url` |
| `body` | request 的 `http.request_body` 或 response 的 `http.response_body` |
| `status` | `http.status` |
| `model` / `protocol` / `provider` | 摘要/索引 |
| `user_content` / `assistant_content` | 摘要 |

读路径 MUST 接受扁平键；写路径 SHOULD 逐步迁移到 `payload.http.*`，或同时写两套直到迁移完成。

---

## 示例 A：HTTP-first 请求事件

```json
{
  "seq": 0,
  "source": "persisting-proxy",
  "kind": "http.request",
  "timestamp": "2026-01-01T00:00:00Z",
  "session_id": "sess-1",
  "agent_id": "agent-a",
  "trace_id": "tr-1",
  "call_id": "c1",
  "payload": {
    "http": {
      "method": "POST",
      "url": "https://api.openai.com/v1/chat/completions",
      "path": "/v1/chat/completions",
      "headers": {
        "content-type": "application/json",
        "authorization": "<redacted>",
        "connection": "keep-alive",
        "keep-alive": "timeout=60, max=1000",
        "user-agent": "claude-cli/1.0"
      },
      "headers_redacted": true,
      "content_type": "application/json",
      "body_encoding": "json",
      "request_body": {
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": "ping" }]
      }
    },
    "connection": {
      "http_version": "HTTP/1.1",
      "persistent": true,
      "connection_header": "keep-alive",
      "keep_alive": "timeout=60, max=1000"
    },
    "client": {
      "peer": "127.0.0.1:54321",
      "peer_ip": "127.0.0.1",
      "peer_port": 54321,
      "pid": 4242,
      "command": "claude --print",
      "user_agent": "claude-cli/1.0"
    },
    "model": "gpt-4o",
    "protocol": "chat.completions",
    "user_content": "ping"
  }
}
```

---

## 示例 B：Storyline → Events 映射（合成多为 degraded）

key = events 字段，value = 在 Storyline 上求值的 JSONPath。  
**注意**：从 storyline 合成的 body 通常只是对话投影，不是原始 HTTP wire；应设 `payload.degraded = true`，除非另有 wire 旁路。

```json
{
  "format": "events",
  "session_id": "$.story_id",
  "agent_id": "$.agent.id",
  "events": [
    {
      "seq": null,
      "source": null,
      "kind": null,
      "timestamp": "$.turns[0].timestamp",
      "session_id": "$.story_id",
      "agent_id": "$.agent.id",
      "call_id": "$.turns[0].calls[0].call_id",
      "trace_id": "$.turns[0].calls[0].trace_id",
      "parent_call_id": "$.turns[0].calls[0].parent_call_id",
      "payload": {
        "degraded": null,
        "http": {
          "method": null,
          "url": null,
          "path": null,
          "headers": null,
          "request_body": "$.turns[0].calls[0].messages",
          "body_encoding": null
        },
        "model": "$.turns[0].calls[0].model",
        "protocol": "$.turns[0].calls[0].protocol",
        "user_content": "$.turns[0].user.text"
      }
    },
    {
      "seq": null,
      "source": null,
      "kind": null,
      "timestamp": "$.turns[0].timestamp",
      "session_id": "$.story_id",
      "agent_id": "$.agent.id",
      "call_id": "$.turns[0].calls[0].call_id",
      "trace_id": "$.turns[0].calls[0].trace_id",
      "parent_call_id": "$.turns[0].calls[0].parent_call_id",
      "payload": {
        "degraded": null,
        "http": {
          "status": null,
          "headers": null,
          "response_body": "$.turns[0].assistant.content",
          "body_encoding": null,
          "truncated": null
        },
        "model": "$.turns[0].calls[0].model",
        "assistant_content": "$.turns[0].assistant.text",
        "usage": "$.turns[0].calls[0].metrics"
      }
    }
  ]
}
```

合成：`kind` ← `http.request` / `http.response`；`degraded` ← `true`（默认）；`seq`/`source` 由写入器分配。

口语对照：

| events 字段 | Storyline 路径 |
|---|---|
| `events.0.call_id` | `turns.0.calls.0.call_id` |
| `events.0.payload.http.request_body` | `turns.0.calls.0.messages` |
| `events.1.payload.http.response_body` | `turns.0.assistant.content` |
| `events.0.payload.http.method` / `url` / `headers` | （无）→ null / 合成默认 |

---

## 示例 C：Events → Storyline（从 wire 再解析）

按 `call_id` 分组后，优先：

| Storyline 字段 | 来源 |
|---|---|
| `calls[0].event_seqs` | `$[*].seq` |
| `calls[0].call_id` | `$[0].call_id` |
| `calls[0].protocol` | `$[0].payload.protocol` 或从 path 推断 |
| `calls[0].messages` | 解析 `$[?(@.kind=~/request/)].payload.http.request_body`（兼容 `.body`） |
| `user.text` | 摘要 `user_content` **或** 从 request_body 提取 |
| `assistant.text` | 摘要 `assistant_content` **或** 从 response_body / SSE 提取 |
| `calls[0].model` | `payload.model` 或 body.model |

---

## 校验规则（Normative）

实现 MUST：

1. 拒绝缺少 `seq` / `source` / `kind` / `payload` 的事件；
2. 保持 `seq` 单调追加；
3. 与其它外围格式互转走 storyline hub；
4. 记录 HTTP 事件时持久化 **headers + body**（或显式标记 `degraded`）；缺一不可宣称可回放。

实现 SHOULD：

1. 为同一 HTTP 交换的 request/response 共享 `call_id`；
1b. 代理路径写入 `client.peer` 与 `connection.persistent`（及原始 Connection/Keep-Alive）；
2. 写 `http.*` kind，同时读兼容 `llm.*`；
3. 从 events 投影 storyline 时填充 `event_seqs`；
4. 对红线头做可检测的脱敏标记。

---

## 与现有文档的关系

| 文档 | 关系 |
|---|---|
| [轨迹存储](../design/trajectory.md) | Lance SoT；本 RFC 强调 SoT 内容应是 HTTP wire |
| [Gateway 管线](../design/gateway.md) | 生产 events；Story 边界在 **之后** 解释 |
| [RFC-0001 Storyline](0001-storyline-format.md) | 有损 Normal 视图 / hub |
| [轨迹 Markdown 格式](../design/trajectory-format.md) | 人读投影，不是 SoT |

实现现状：`CaptureRecord` 已具备 `path`/`body`/`status` 等扁平键；本 RFC 将其 **提升为明确的 HTTP-first 契约**，并引入 `http.*` kind 与 `payload.http` 嵌套作为演进目标。

---

## 未决问题

1. **强制 `payload.http` 嵌套 vs 长期扁平键**：迁移窗口与双写策略。
2. **流式 SSE**：存聚合字符串、原始帧数组，还是外置 blob + hash。
3. **seq 作用域**：per-session vs per-run-dataset。
4. **非 HTTP 传输**（WebSocket / gRPC）：是否用 `http.*` 类比的 `transport.*` 扩展。
5. **默认 `capture_level`**：是否默认 `full` wire 才能称为生产 SoT。
6. **连接 ID**：是否在代理层分配稳定 `connection.id` 以跨越多次 keep-alive 请求关联（超出现有 peer 粒度）。

---

## Changelog

| Version | Date | Notes |
|---|---|---|
| Draft | 2026-07-30 | 初稿：EventRecord / EventRow / wire、与 Storyline hub |
| Draft | 2026-07-30 | **定位修正为 HTTP-first SoT**：原始请求/响应可回放；摘要字段降为可选；引入 `http.*` kind 与 `payload.http` |
| Draft | 2026-07-30 | **headers 与 body 同为记录 MUST**；缺 headers 亦为 degraded |
| Draft | 2026-07-30 | 增补 **长连接 `connection.*` + 客户端 `client.*`（peer/pid/command）** 记录要求 |
| Draft | 2026-07-30 | **events 仅 Lance**：JSON/JSONL 降为调试导出，非一等 wire |
