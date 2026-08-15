# Trajectory formats

| Format | Role | Specification |
| --- | --- | --- |
| Events | Canonical HTTP-first event record | [RFC-0002](../../../rfcs/0002-events-format.md) |
| Storyline | Normalized session and tool-call projection | [RFC-0001](../../../rfcs/0001-storyline-format.md) |
| ACTF | JSON trajectory interchange | [RFC-0004](../../../rfcs/0004-actf-format.md) |
| ATIF | External interchange supported by pChronicle | [Storyline mapping](../../../rfcs/0001-storyline-format.md) |
| AgenticMD | Human-readable live and materialized view | [AgenticMD reference](../agenticmd.md) |

Canonical events and derived projections have different fidelity and ownership.
See [Trajectory storage](../../design/trajectory-storage.md) before choosing a
format as a persistence boundary.
