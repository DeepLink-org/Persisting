# Persisting 自有 fixture（`local/`）

本目录 **不属于** [agentgateway](https://github.com/agentgateway/agentgateway) 上游测试集，由 Persisting 维护，用于 AG 未覆盖的场景（如 Codex 多轮、tool roundtrip、截断 SSE 片段）。

## 与 `fixtures/` 其余内容的关系

从 agentgateway 手动复制 `crates/agentgateway/src/llm/tests/` 时：

- 只覆盖 `fixtures/` 下除 **`local/`** 以外的文件；
- **不要** 删除或覆盖 `local/` 中的内容。

## 当前文件

| 路径 | 用途 |
|------|------|
| `requests/responses/codex_basic.json` | Codex 风格 Responses 请求（permissions + 多轮 input） |
| `requests/responses/codex_tool_roundtrip.json` | Responses tool call / tool output 往返 |
| `response/completions/stream_head.txt` | OpenAI completions SSE 前缀（流式解析单测） |
| `response/completions/stream_tool_call.txt` | 含 tool call delta 的 SSE |
| `response/completions/tool_call.json` | 非流式 tool call completions 响应 |

## 测试引用

- `src/conversion/responses_completions.rs`
- `src/conversion/stream.rs`、`responses_stream.rs`
- `src/storage/dialogue_extract/completions.rs`
- `tests/ag_fixture_tests.rs`（部分 capture case）
