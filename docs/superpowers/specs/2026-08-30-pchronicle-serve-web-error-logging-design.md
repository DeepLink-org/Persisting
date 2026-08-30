# pChronicle serve + Web 日志与错误体系

日期：2026-08-30  
状态：已在对话中逐节确认，待用户审阅本文件后进入实现计划  
范围：Warehouse HTTP（`pchronicle serve` 的 Dataset UI/API）+ `pchronicle-web`  
排除：Gateway、Control、OpenTelemetry、浏览器 E2E、RFC 7807

## 目标

同一条失败能在三处对上：API JSON、Web 横幅、`serve` stderr。请求生命周期（启动、查询、失败）打 INFO。4xx 给可行动文案；5xx 对外脱敏、对内日志给出根因而不是一句 `internal server error`。

成功标准：

1. 默认 `pchronicle serve` 在 stderr 上能看到启动 INFO、每个 `/api` 请求结束 INFO、4xx WARN、5xx ERROR。
2. 5xx JSON 的 `request_id` 与响应头 `x-request-id`、ERROR 日志字段相同；ERROR 的 `root_cause` 等于 `anyhow::Error::root_cause()` 的 Display（截断后）。
3. 5xx JSON `message` 仍是 `internal server error`，不含路径、SQL、anyhow chain。
4. Web 横幅按 `code` 选题，展示可复制 `request_id`；5xx 不渲染根因。

## 现状（实现必须面对的断裂）

- `ApiError` 已有 `BoundaryCode`；`ApiError::internal` 打 `tracing::error!(error = ?error)`，但 CLI **没有安装 tracing subscriber**，默认 `serve` 上这些日志是静默的。
- `--log-level` 只过滤 CLI `DiagnosticWriter`，不管 Warehouse 请求。
- Web `api.rs::checked` 把失败收成 `HTTP {status}: {body}` 字符串；Workspace / Physical / Analysis 各写一套展示。
- 大量 `map_err(ApiError::internal)`；FTS diagnostic 在 `debug`；`physical.rs` 用 `message.contains("not found")` 猜类型。

## 错误契约

每条失败 HTTP 响应（含 compile 422）的 JSON 对象包含：

| 字段 | 规则 |
|---|---|
| `code` | 现有 `BoundaryCode` 的 snake_case。compile 继续用自己的字符串码（如 `unplannable`），同样带 `request_id`。 |
| `message` | 4xx：可行动人话。5xx：固定 `internal server error`。 |
| `request_id` | 见下节。 |

compile 422 现有字段 `field`、`engine_detail` 保留；`engine_detail` 已有 1500 字符截断，保持。中间件若发现 JSON 对象缺少 `request_id`，补上。

响应头一律设置 `x-request-id`，与 JSON 相同。

Content-Type 保持 `application/json`（不改为 `application/problem+json`）。

## request_id

- 默认：UUID v4 的 hex（无连字符）前 16 个字符，小写。
- 若请求头 `x-request-id` 长度为 1–64、仅 ASCII 可打印且无空白，则沿用；否则生成新的。
- 浏览器本轮不主动发送 `x-request-id`。

## 日志

只在 `run_serve` 安装 `tracing-subscriber`（crate 依赖从 dev-dependency 提升为正式依赖）。输出 stderr、人类可读一行。`--log-level` 映射为 tracing filter：`error` / `warn` / `info`（默认）/ `debug`。不读取 `RUST_LOG`。

Target：`pchronicle.serve`。

静态资源（非 `/api` 与 `/api/v1` 前缀）不打 INFO 完成行。

### 启动（INFO）

Warehouse 开始 listen 之后一条：listen 地址、挂载 Dataset 名列表、catalog `snapshot_id`（prepare 时已有则记；没有则省略该字段）。

### 请求完成（INFO）

每个 `/api` 或 `/api/v1` 请求结束一条，字段：`request_id`、`method`、`path`、`status`、`elapsed_ms`，以及 URI query string 截断到 **512 字节**（按 UTF-8 字符边界截断，超长加 `…`）。

POST `/api/query/evidence` 与 `/api/analysis/compile`（含 `/api/v1` 同路径）在 handler 内再记一条 INFO，同一 `request_id`，带已有的 Dataset 坐标（有则记）和截断到 512 字节的 SQL 或问题文本。

### 4xx（WARN）

同一 `request_id`、`code`、公开 `message`。若 anyhow 链存在更深 source，另附 `root_cause`（截断 512）。FTS diagnostic 从独立 `debug` 行并入这条 WARN（字段 `fts_errors`，多条用 `; ` 连接，总长截断 512）。

### 5xx（ERROR）

`ApiError::internal`（及等价入口）打 ERROR，正文为 `warehouse request failed`，**不得**使用 `internal server error` 作为日志正文。字段：

| 字段 | 内容 |
|---|---|
| `request_id` | 与 JSON 相同 |
| `code` | `internal`（除非链上 typed 边界把 HTTP 改成了非 500） |
| `root_cause` | `error.root_cause()` 的 Display，截断 512 字节 |
| `chain` | `format!("{error:#}")`，截断 2048 字节 |
| `handler` | 入口名：`explorer_runs`、`explorer_turns`、`query_evidence`、`compile_analysis`、`physical_preview` 等，与函数名一致的稳定短名 |

中间件对 5xx 仍打 INFO 完成行，**不再重复 ERROR**。

`--log-level error` 压掉启动与请求 INFO，只留 5xx ERROR。默认 `info` 能看到启动、每次查询、每次失败。

### 后台无 request 的失败

`current_catalog_for_runs` 刷新失败：保持 WARN，使用 `{error:#}`，并补 `root_cause`；**不伪造** `request_id`。Acceleration 索引构建失败同样：ERROR 带 `root_cause` + `chain`，无 `request_id`。

## 错误轨迹与分类

轨迹：

```
Lance / DataFusion / FTS / 文件系统
  → anyhow（.context）
    → handler map_err
      → ApiError JSON（对外）+ tracing（对内）
```

分类规则（禁止用 `message.contains("not found")` 作为新逻辑；`physical.rs` 的 `map_inspect` 改为按 typed 边界或明确错误类型，找不到则 500 + 日志根因）：

1. 若 anyhow 链上存在 `CliBoundaryError`（或 HTTP 侧同等 typed 边界），按其 `BoundaryCode` 映射 HTTP，公开 `message` 用该边界的 message。
2. 现有明确 4xx 保持不变：只读 SQL 校验、`FTS unavailable` → `invalid_request`、compile `unplannable` / `CompileError` → 422、run 缺失 → `not_found`、catalog snapshot 冲突 → `conflict`、输出超限 → `resource_exhausted`。
3. 其余未知失败 → 500 + 对外脱敏 + ERROR 根因。

`ApiError::internal` 只通过调用方传入的 `request_id` 打 ERROR（axum extractor `RequestId`，handler 写成 `ApiError::internal(request_id, error)`）。中间件不补打 5xx ERROR。若某条 500 忘了传 id，中间件仍会把 `request_id` 写入 JSON 和响应头，ERROR 里该字段为空字符串——测试覆盖现有 handler 均传入。

## Web

`pchronicle-web/src/api.rs` 解析失败为：

```text
ApiFailure {
  status: u16,
  code: String,          // 无法解析时为空
  message: String,
  request_id: Option<String>,
  field: Option<String>,
  engine_detail: Option<String>,
  raw: String,
}
```

网络发送失败：`code = "unavailable"`，无 `request_id`。非 JSON 体：`message = "HTTP {status}: {body}"`。

`WorkspaceNotice` 由 `ApiFailure` 生成，Runs/Catalog、Physical、Analysis 共用。Physical 不再把原始字符串直接渲染成一行。

标题与下一步（界面语言保持英文，与现有 UI 一致）：

| `code` | 标题 | 下一步 |
|---|---|---|
| `invalid_request` | This request isn't valid | 使用服务端 `message` |
| `not_found` | Nothing matched | 使用 `message` |
| `conflict` | This view is out of date | Refresh the catalog and try again |
| `unsupported` | This isn't supported | 使用 `message` |
| `unplannable` | This isn't supported | 使用 `message`；可展开 `engine_detail` |
| `resource_exhausted` | The result is too large | Narrow the query or lower the row limit |
| `unavailable` | The server isn't reachable | Check that pchronicle serve is still running |
| `internal` | Something went wrong | The server log for this request ID has the cause |
| 空或其他 | Request failed | 使用 `message` |

横幅在存在 `request_id` 时始终展示且可复制。折叠「Show technical details」只含 `raw` 和（若有）`engine_detail`。**不渲染 5xx 根因。**

`CompileFailure` 增加 `request_id: Option<String>`（serde default）。Assistant `query_sql` 工具失败字符串包含 `request_id`（有则）。

## 测试

CLI（`just test persisting-pchronicle-cli` 中现有 server 测试模块扩展）：

- 400/500 JSON 含 `request_id`，且与 `x-request-id` 相同。
- 500 体为 `code=internal`、`message=internal server error`，序列化文本不含注入的密钥路径（沿用 `/secret/backend` 用例）。
- 用测试 subscriber 捕获 ERROR：`root_cause` 等于 innermost Display；日志正文含 `warehouse request failed` 不含作为正文的 `internal server error`。
- `FTS unavailable` 仍 400 `invalid_request`。
- 合法传入的 `x-request-id` 被沿用；非法（含空白、过长）则生成新 id。
- query evidence INFO：超过 512 字节的 SQL 在日志字段中被截断。

Web（`cargo test --bin pchronicle-web`，`pchronicle-web` 目录）：

- 解析带 `code`/`request_id` 的 JSON。
- 非 JSON 走 fallback。
- `internal` 横幅标题为 Something went wrong，含 `request_id`，不含密钥。
- `resource_exhausted` 映射到缩小查询文案。

## 文档

- `docs/src/pchronicle/guides/serve.md` 与 `serve.zh.md`：默认 INFO 请求日志、`--log-level`、用横幅上的 `request_id` 对 stderr。
- CLI `--log-level` 帮助文本若仍只提 DiagnosticWriter，改为同时覆盖 `serve` Warehouse tracing。

## 非目标

- 不改 Gateway / Control 日志。
- 不引入 OpenTelemetry 或 `tower-http` TraceLayer（自写 axum 中间件）。
- 不把 5xx 根因送进浏览器。
- 不把 unknown-field 默认上限、Storyline schema 版本、FTS offload 纳入本工作。
