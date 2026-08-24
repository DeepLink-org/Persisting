# 轨迹格式

| 格式 | 角色 | 规范 |
| --- | --- | --- |
| Events | Canonical HTTP-first event record | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | 规范化 session 与 tool-call projection | [RFC-0001 § Wire schema](../../../rfcs/0001-storyline-format.md#wire-schema) |
| ACTF | JSON 轨迹交换格式 | [RFC-0004 § JSON Pointer 映射](../../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping) |
| ATIF | Agent 轨迹交换格式 | [RFC-0008 § JSON Pointer 映射](../../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping) |
| OpenAI Messages | 面向训练与评测的 row-based 语料 | [RFC-0009 § JSON Pointer 映射](../../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping) |
| Codex | 本地 Codex CLI/TUI 会话 JSONL（`~/.codex/sessions/**/rollout-*.jsonl`）。仅解码。 | — |
| Claude Code | 本地 Claude Code 会话 JSONL（`~/.claude/projects/**/*.jsonl`）。仅解码。 | — |
| AgenticMD | 面向人的 live 与 materialized view | [AgenticMD reference](../agenticmd.md) |

Canonical event 与派生 projection 的 fidelity 和 ownership 不同。选择持久化边界前请阅读
[轨迹存储](../../design/trajectory-storage.md)。

每份格式 RFC 都是该格式 wire contract 及其 Storyline 映射的权威来源；其他位置不再维护
重复映射表。
