# ATIF benchmark corpus

The eight JSON documents in this directory are deterministic ATIF-v1.7 fixtures with 10–20
steps each. Together they cover plain dialogue, sparse optional fields, sequential and parallel
tool calls, observations linked by `source_call_id`, reasoning fields, Unicode, multimodal
messages, and long context.

The corpus is shared by `tests/atif_lance_corpus.rs` and the
`atif_storyline_lance` benchmark. These are reviewed compatibility fixtures rather than a
generated performance corpus. The runnable examples retain their structure but replace message
text with deterministic samples from repository source code.
