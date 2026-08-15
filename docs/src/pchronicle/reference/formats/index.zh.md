# 轨迹格式

| 格式 | 角色 | 规范 |
| --- | --- | --- |
| Events | Canonical HTTP-first event record | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | 规范化 session 与 tool-call projection | [RFC-0001](../../../rfcs/0001-storyline-format.md) |
| ACTF | JSON 轨迹交换格式 | [RFC-0004](../../../rfcs/0004-actf-format.md) |
| ATIF | pChronicle 支持的外部交换格式 | [Storyline mapping](../../../rfcs/0001-storyline-format.md) |
| AgenticMD | 面向人的 live 与 materialized view | [AgenticMD reference](../agenticmd.md) |

Canonical event 与派生 projection 的 fidelity 和 ownership 不同。选择持久化边界前请阅读
[轨迹存储](../../design/trajectory-storage.md)。
