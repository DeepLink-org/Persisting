# Trajectory storage

The trajectory processing model is `StorylineDocument`. ATIF, ACTF, OpenAI
Messages, Storyline JSON, and AgenticMD are supported encodings; Codex and
Claude Code session JSONL are decode-only inputs. Their timestamps continue
to be parsed and preserved by the codecs.

`StorylineLanceStore` stores documents directly as `runs.lance`, `steps.lance`,
and `tool_calls.lance`, with a content layer for large fields. `CURRENT` pins a
published generation and exact Lance versions. Reconstructing a document uses
that pinned generation and validates referenced content.

Import supports create, append, and replacement. Local whole-Dataset replacement
stages output before swapping it into place; object-store whole-Dataset replacement
has different failure semantics, documented in the import guide. Store-level
publication uses writer leases and generation checks. Maintenance and deletion
must preserve the same publication and content-integrity guarantees.

Compact JSONL is a separate record storage path preserving original JSON bytes.
It does not infer Storyline Runs, Steps, or ToolCalls.

See [Storyline Lance](storyline-lance.md), [Snapshot](catalog.md),
[formats](../reference/formats/index.md), and [import/export](../guides/exchange.md).
