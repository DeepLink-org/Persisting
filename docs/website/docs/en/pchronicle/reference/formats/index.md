# Run data formats

| Format | Role | Specification |
| --- | --- | --- |
| Events | Recorded HTTP-first event format | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | Internal normalized Run and tool-call model | [RFC-0001 § Wire schema](../../../rfcs/0001-storyline-format.md#wire-schema) |
| ACTF | JSON Run interchange | [RFC-0004 § JSON Pointer mapping](../../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping) |
| ATIF | Agent Run interchange | [RFC-0008 § JSON Pointer mapping](../../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping) |
| OpenAI Messages | Row-based training and evaluation corpus | [RFC-0009 § JSON Pointer mapping](../../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping) |
| Codex | Local Codex CLI/TUI session JSONL (`~/.codex/sessions/**/rollout-*.jsonl`). Decode-only. | — |
| Claude Code | Local Claude Code transcript JSONL (`~/.claude/projects/**/*.jsonl`). Decode-only. | — |
| AgenticMD | Human-readable live and materialized view | [AgenticMD reference](../agenticmd.md) |

Recorded events and reconstructed projections have different fidelity and ownership.
See [Run storage design](../../design/trajectory-storage.md) before choosing a
format as a persistence boundary.

Each format RFC is authoritative for that format's wire contract and its mapping
to Storyline. Mapping tables elsewhere are non-normative.
