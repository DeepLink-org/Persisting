# ATIF benchmark corpus

The eight JSON documents in this directory are deterministic ATIF-v1.7 fixtures with 10–20
steps each. Together they cover plain dialogue, sparse optional fields, sequential and parallel
tool calls, observations linked by `source_call_id`, reasoning fields, Unicode, multimodal
messages, and repetitive long context.

Regenerate them from the repository root:

```bash
cargo run -p persisting-pchronicle --example generate_atif_corpus
```

The corpus is shared by `tests/atif_lance_corpus.rs` and the
`atif_storyline_lance` benchmark. Keep generation deterministic so benchmark results remain
comparable across revisions.
