# AgenticMD run format

AgenticMD is pChronicle's human-readable debugging view: ordinary Markdown
body text with optional machine-locatable block headers. It is not a storage
protocol or a source of truth. System-generated files use `{session_id}.md`.
The reader stays tolerant of hand edits, missing fields, and unknown
extensions.

## 1. Document structure

```markdown
---
format: persisting
block: speaker+json+markdown
session_id: run-123
agent_id: coding
turn_count: 1
---

<!-- persisting:block:user {"type":"text","length":24,"source":"user","step_id":1,"call_id":"call-1"} -->
请检查这个仓库。

<!-- persisting:block:agent {"type":"text","length":27,"source":"agent","step_id":2,"call_id":"call-1"} -->
我先查看目录结构。
```

Frontmatter is an optional session summary. Block headers are optional
debugging metadata. System writes record the body's UTF-8 byte length so live
upsert can locate a block; readers may omit the length and parse to the next
block boundary. Plain Markdown with no block headers is read as a single
`system` debugging block.

## 2. Block headers

```text
<!-- persisting:block:{speaker} {json} -->
```

Newly written `source` values follow Storyline and are typically `user`,
`agent`, or `system`. Common JSON fields:

| Field | Meaning |
|---|---|
| `source` | Storyline turn source |
| `step_id` | Storyline turn order |
| `call_id` | Model-call identity, used for pairing and live upsert |
| `type`, `length` | Generator display and location hints; not a business schema |

Time, model, provider, token, tool, and subagent references may appear as
extension fields. Consumers should ignore unknown fields. Legacy `role`,
`seq`, `session`, and `agent` remain read aliases. A mismatch between the
speaker token and a JSON field no longer rejects the whole document.

## 3. Frontmatter

pChronicle defines and serializes frontmatter. Common fields include:

- `format`, `block`;
- `session_id`, `agent_id`, `model_name`, `provider`;
- `started_at`, `duration`, `turn_count`;
- `total_tokens`, `estimated_cost_usd`;
- `subagents` and optional `client` origin information.

Zero values, unknown values, and the entire frontmatter block may be omitted.
Nested objects and unknown fields are preserved. There is no mandatory
frontmatter schema independent of Storyline.

## 4. Live updates

When live Markdown is enabled, Gateway projects visible dialogue into
AgenticMD while writing canonical Lance events:

1. User blocks are written by `call_id`;
2. A streaming assistant updates in place with the same `call_id`;
3. Rewriting an assistant block must keep any following user blocks;
4. Internal probes, repeated history, and invisible thinking stay out of the
   body;
5. Images and other multimodal content use stable placeholders rather than
   inlined bulk base64.

These rules are a live-projection policy. A missing or failed AgenticMD file
does not change the canonical append result.

## 5. Relationship to Lance

Lance events own fidelity, replay, stats, and structured query. Internal
trajectory operations can rebuild AgenticMD from Lance, but the public
`pchronicle` CLI does not expose AgenticMD materialize or import subcommands.
AgenticMD does not compact or restore canonical events automatically. Public
exchange uses the formats supported by
[`pchronicle import/export`](cli.md).

## 6. Examples and implementation

- Gateway end-to-end quantitative example: `examples/pvisor/04-gateway-llm-control/`
- Lance/ATIF storage and analysis examples: `examples/pchronicle/`
- Format and view implementation: `crates/persisting-pchronicle/src/formats/`, `src/projection/`
- [pChronicle run storage](../design/trajectory-storage.md)
- [Run data formats and exchange boundaries](formats/index.md)
