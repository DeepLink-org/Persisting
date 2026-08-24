# 运行数据格式

| 格式 | 角色 | 规范 |
| --- | --- | --- |
| Events | HTTP-first 记录事件格式 | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | 内部规范化 Run 与 tool-call 模型 | [RFC-0001 § Wire schema](../../../rfcs/0001-storyline-format.md#wire-schema) |
| ACTF | JSON Run 交换格式 | [RFC-0004 § JSON Pointer 映射](../../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping) |
| ATIF | Agent Run 交换格式 | [RFC-0008 § JSON Pointer 映射](../../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping) |
| OpenAI Messages | 面向训练与评测的 row-based 语料 | [RFC-0009 § JSON Pointer 映射](../../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping) |
| Codex | 本地 Codex CLI/TUI 会话 JSONL（`~/.codex/sessions/**/rollout-*.jsonl`）。仅解码。 | — |
| Claude Code | 本地 Claude Code 会话 JSONL（`~/.claude/projects/**/*.jsonl`）。仅解码。 | — |
| AgenticMD | 面向人的 live 与 materialized view | [AgenticMD reference](../agenticmd.md) |

记录事件与重建 projection 的保真度和所有权不同。选择持久化边界前请阅读
[Run 存储设计](../../design/trajectory-storage.md)。

每份格式 RFC 都是该格式 wire contract 及其 Storyline 映射的权威来源；其他位置不再维护
重复映射表。
