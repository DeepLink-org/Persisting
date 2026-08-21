# Trajectory formats

| Format | Role | Specification |
| --- | --- | --- |
| Events | Canonical HTTP-first event record | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | Normalized session and tool-call projection | [RFC-0001 § Wire schema](../../../rfcs/0001-storyline-format.md#wire-schema) |
| ACTF | JSON trajectory interchange | [RFC-0004 § JSON Pointer mapping](../../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping) |
| ATIF | Agent trajectory interchange | [RFC-0008 § JSON Pointer mapping](../../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping) |
| OpenAI Messages | Row-based training and evaluation corpus | [RFC-0009 § JSON Pointer mapping](../../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping) |
| AgenticMD | Human-readable live and materialized view | [AgenticMD reference](../agenticmd.md) |

Canonical events and derived projections have different fidelity and ownership.
See [Trajectory storage](../../design/trajectory-storage.md) before choosing a
format as a persistence boundary.

Each format RFC is authoritative for that format's wire contract and its mapping
to Storyline. Mapping tables elsewhere are non-normative.
