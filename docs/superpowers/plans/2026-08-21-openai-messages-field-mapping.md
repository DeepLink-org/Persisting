# OpenAI Messages Field Mapping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the pChronicle OpenAI Messages adapter map every supported source field into Storyline/ATIF, ignore known optional empty values, and retain only genuinely unmapped values in `unknown_fields`.

**Architecture:** Decode each owned OpenAI row through a destructive projection: mapping code removes every consumed field from the row, while known optional empty fields are discarded. The residual row is then the sole input to unknown-field capture. OpenAI export is canonical and reconstructs cumulative messages plus rows from Storyline semantics; it does not preserve the source document's physical layout.

**Tech Stack:** Rust 2021, `serde_json::Value`, Storyline/ATIF conversion types, pChronicle unknown-fields support, Cargo unit and integration tests.

**Spec:** `docs/superpowers/specs/2026-08-20-openai-messages-field-mapping-design.md`

## Global Constraints

- Map only fields named by the spec into existing Storyline/ATIF fields; do not use `extra` as a source metadata bucket.
- Treat known optional `null`, empty arrays, and empty objects as absent; preserve empty `content` because it is a mapped message value.
- Put every remaining source member in `unknown_fields` with its complete JSON value.
- Preserve the existing embedded-text tool-call parser and prefer structured `tool_calls` when present.
- Guarantee logical Storyline roundtrip, not byte identity, row order, snapshot-window shape, object-key order, or null-versus-missing identity.
- Do not add a consumed-path registry, row carrier, wire/schema field, Lance column, or generic unknown-fields protocol.
- Do not modify TTAS, Queue/Sampler, Search, or standalone `persisting-dlcapt`.
- Preserve unrelated dirty-worktree changes; stage only task-owned files or hunks.

---

### Task 1: Make field projection authoritative for unknown capture

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:32-274`
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:875-1362`
- Test: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:1364-1614`

**Interfaces:**
- Consumes: `StorylineDocument`, `StorylineTurn`, `StorylineToolCall`, `StorylineTimestamp`, and the existing `StorylineUnknownFields::insert` API.
- Produces: `rows_to_storyline(session_id, records: &mut [(usize, Value)], relative_path) -> InputResult<StorylineDocument>`; residual rows whose mapped fields have been removed; `normalized_metrics` containing all approved row and env-state keys.

- [ ] **Step 1: Replace the old residual expectations with a failing field-partition test**

Replace `openai_step_id_without_formal_carrier_is_unknown` and update `openai_noncanonical_source_fields_are_unknown_without_source_extra` so the fixture includes all mapping classes and asserts the exclusive partition:

```rust
#[test]
fn openai_maps_known_fields_and_only_keeps_unmapped_values() {
    let input = json!({"session_steps": [{
        "dataset_type": "TEST",
        "id": "event-1",
        "session_id": "session-1",
        "step_id": 1,
        "job_id": "job-7",
        "agent_model": "model-3",
        "created_at": 1_785_578_400.25,
        "reward": 0.75,
        "step_reward": -0.25,
        "is_terminal": true,
        "is_truncated": false,
        "is_session_completed": true,
        "is_trainable": false,
        "env_id": "session-1",
        "messages": [{
            "role": "user",
            "content": "inspect",
            "name": null,
            "refusal": null,
            "tool_call_id": null,
            "tool_calls": null
        }],
        "response": {
            "role": "assistant",
            "content": "done",
            "name": null,
            "refusal": null,
            "tool_call_id": null,
            "tool_calls": null
        },
        "meta_json": {
            "source": "fixture",
            "group_id": "group-1",
            "env_state": {
                "session_id": "session-1",
                "requested_model": "model-3",
                "llm_step_index": 1,
                "total_tokens": 3,
                "total_latency_ms": 12.75,
                "ttft_ms": 2.5,
                "request_id": "request-1"
            }
        },
        "blob_manifest": [],
        "chosen_response": null,
        "vendor_row": {"kept": true}
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "source.json").unwrap();
    let story = &stories[0];
    let fields = &story.unknown_fields.sources["openai-msg"].fields;

    assert_eq!(story.agent.id, "fixture");
    assert_eq!(story.run_id.as_deref(), Some("job-7"));
    assert_eq!(story.agent.model_name.as_deref(), Some("model-3"));
    assert_eq!(story.turns[1].metrics.as_ref().unwrap()["is_terminal"], true);
    assert_eq!(story.turns[1].metrics.as_ref().unwrap()["is_session_completed"], true);
    assert_eq!(story.turns[1].metrics.as_ref().unwrap()["total_tokens"], 3);
    assert_eq!(story.turns[1].latency_ms, Some(12));
    assert_eq!(story.turns[1].ttft_ms, Some(2));

    assert_eq!(fields["/session_steps/0/dataset_type"], "TEST");
    assert_eq!(fields["/session_steps/0/id"], "event-1");
    assert_eq!(fields["/session_steps/0/vendor_row"], json!({"kept": true}));
    assert_eq!(fields["/session_steps/0/meta_json/group_id"], "group-1");
    assert_eq!(
        fields["/session_steps/0/meta_json/env_state/request_id"],
        "request-1"
    );

    for mapped in [
        "/session_steps/0/step_id",
        "/session_steps/0/is_terminal",
        "/session_steps/0/is_truncated",
        "/session_steps/0/is_session_completed",
        "/session_steps/0/is_trainable",
        "/session_steps/0/env_id",
        "/session_steps/0/messages/0/role",
        "/session_steps/0/messages/0/content",
        "/session_steps/0/messages/0/name",
        "/session_steps/0/messages/0/refusal",
        "/session_steps/0/messages/0/tool_call_id",
        "/session_steps/0/messages/0/tool_calls",
        "/session_steps/0/response/role",
        "/session_steps/0/response/content",
    ] {
        assert!(!fields.contains_key(mapped), "mapped field leaked: {mapped}");
    }
}
```

- [ ] **Step 2: Run the field-partition test and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle openai_maps_known_fields_and_only_keeps_unmapped_values -- --nocapture
```

Expected: FAIL because `step_id`, status fields, empty message options, and the complete `meta_json` are still captured as unknown, and the four row status flags are absent from metrics.

- [ ] **Step 3: Change parsing to mutate residual rows instead of classifying them twice**

Change the group loop to retain one mutable row collection:

```rust
for (session_id, mut records) in groups {
    let story_index = stories.len();
    let mut story = rows_to_storyline(&session_id, &mut records, &relative_path)?;
    capture_openai_unknowns(&mut story, &relative_path, &root_unknown, &records)?;
    story.unknown_key_counts = validate_unknown_fields_with(
        &story.unknown_fields,
        UnknownFieldLimits::default(),
        normalize_openai_pointer,
    )?;
    for (ordinal, _) in records {
        carriers.push(CarrierBinding {
            story_index,
            pointer: format!("/session_steps/{ordinal}"),
        });
    }
    stories.push(story);
}
```

Delete `is_canonical_openai_row_field`. Reduce `capture_openai_unknowns` to copying root unknowns and the members still present in each residual row. Keep specialized traversal only for partially consumed `messages`, `response`, `meta_json`, and `meta_json.env_state`, so their remaining direct members retain paths such as `/messages/0/name` and `/meta_json/env_state/request_id`; this traversal must not decide canonical ownership.

Add schema-aware empty handling:

```rust
fn is_known_optional_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn discard_known_optional_empty(object: &mut Map<String, Value>, key: &str) {
    if object.get(key).is_some_and(is_known_optional_empty) {
        object.remove(key);
    }
}
```

Use it only for source-schema optional keys: `response`, `name`, `refusal`, `tool_call_id`, `tool_calls`, `blob_manifest`, `chosen_response`, `rejected_response`, `ground_truth_answer`, and `reference_answer`. Do not apply it to arbitrary vendor keys or `content`.

- [ ] **Step 4: Consume row, message, tool-call, and meta fields during mapping**

Change the function signature to accept `records: &mut [(usize, Value)]`, then sort the owned residual rows in place with:

```rust
records.sort_by_key(|(_, row)| row.get("step_id").and_then(Value::as_i64));
```

For each row, read and remove `session_id`, positive integer `step_id`, `agent_id`, run/model/timestamp fields, row metrics, valid aliases, and known optional empty fields. Use `agent_id` first, then `meta_json.source`, then the selected model as the Storyline agent ID. Parse `meta_json` whether it is a JSON string or object, remove `source`, approved aliases, and metric-whitelist members, then reinsert only the residual meta object when it is non-empty. If parsing fails, reinsert the original `meta_json` unchanged.

For each recognized message object:

1. clone the complete `content` for Storyline before removing `content`;
2. remove a recognized `role` after it selects the Storyline source;
3. parse valid structured `tool_calls`, remove `id`, `type=function`, `function.name`, and `function.arguments`, and retain any remaining tool-call members at their original positions;
4. remove a linked role=`tool` `tool_call_id` after building an observation/result;
5. remove explicit `reasoning_content` after mapping it;
6. discard only the approved optional empty fields;
7. leave non-empty `name`, malformed tool calls, unlinked IDs, and vendor members in the residual message.

Update `normalized_metrics` to include this exact row list:

```rust
const ROW_METRIC_FIELDS: &[&str] = &[
    "reward",
    "step_reward",
    "is_terminal",
    "is_truncated",
    "is_session_completed",
    "is_trainable",
];
```

and this exact env-state list:

```rust
const ENV_METRIC_FIELDS: &[&str] = &[
    "prompt_tokens", "completion_tokens", "total_tokens",
    "request_bytes", "response_bytes", "output_bytes", "output_chunk_count",
    "upstream_latency_ms", "gateway_overhead_ms", "total_latency_ms", "ttft_ms",
    "retry_count", "status_code", "finish_reason", "truncate_reason",
    "error_type", "error_text", "client_cancelled", "upstream_cancelled",
    "synthetic_stop", "is_truncated", "is_session_completed", "max_steps",
    "is_stream", "payload_sampled", "created_at", "completed_at",
];
```

Row metrics win over same-named env metrics. Remove an equal env duplicate; retain a differing duplicate in the residual env object.

- [ ] **Step 5: Map refusal text and verify GREEN**

Update `openai_refusal_only_response_is_a_known_output` to require:

```rust
assert_eq!(stories[0].turns[1].message, "I cannot help with that.");
assert!(!stories[0].unknown_fields.sources["openai-msg"]
    .fields
    .contains_key("/session_steps/0/response/refusal"));
```

Run:

```bash
cargo test -p persisting-pchronicle openai_maps_known_fields_and_only_keeps_unmapped_values -- --nocapture
cargo test -p persisting-pchronicle openai_refusal_only_response_is_a_known_output -- --nocapture
```

Expected: both PASS.

- [ ] **Step 6: Commit the field projection**

Stage only task-owned changes and commit:

```bash
git add -p crates/persisting-pchronicle/src/formats/openai_corpus.rs
git commit -m "fix(pchronicle): map known OpenAI corpus fields"
```

---

### Task 2: Import first-row context and derive stable turn IDs

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:875-1106`
- Test: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:1673-1944`
- Test: `crates/persisting-pchronicle/src/tests.rs:453-495`

**Interfaces:**
- Consumes: mutable residual rows and mapped-message helpers from Task 1.
- Produces: context turns marked `is_copied_context=true`; user/agent turn IDs derived from source `step_id`; one current interaction per row.

- [ ] **Step 1: Add a failing context and step-ID test**

```rust
#[test]
fn openai_imports_first_row_context_once_and_offsets_step_ids() {
    let input = json!({"session_steps": [
        {
            "session_id": "s",
            "step_id": 1,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "prior question"},
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "first answer"}
            ],
            "response": null
        },
        {
            "session_id": "s",
            "step_id": 2,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "prior question"},
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "first answer"},
                {"role": "user", "content": "second question"},
                {"role": "assistant", "content": "second answer"}
            ],
            "response": null
        }
    ]});

    let story = parse_openai_msg_corpus_value(&input, "context.json")
        .unwrap()
        .remove(0);
    assert_eq!(story.turns.len(), 6);
    assert_eq!(story.turns.iter().map(|turn| turn.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(story.turns[0].source, "system");
    assert_eq!(story.turns[0].message, "policy");
    assert_eq!(story.turns[1].message, "prior question");
    assert_eq!(story.turns[0].is_copied_context, Some(true));
    assert_eq!(story.turns[1].is_copied_context, Some(true));
    assert_eq!(story.turns[2].message, "first question");
    assert_eq!(story.turns[3].message, "first answer");
    assert_eq!(story.turns[4].message, "second question");
    assert_eq!(story.turns[5].message, "second answer");
}
```

- [ ] **Step 2: Run the context test and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle openai_imports_first_row_context_once_and_offsets_step_ids -- --nocapture
```

Expected: FAIL because the current importer emits only four current-interaction turns and assigns sequential IDs without first-row context.

- [ ] **Step 3: Build context before consuming current interactions**

For the first sorted row, identify the selected output and the final user preceding it. Convert every recognized message before that user into one context turn. Preserve full content, explicit reasoning, structured tool calls, tool observations, and role mapping through this constructor:

```rust
fn openai_context_turn(
    id: i64,
    source: String,
    message: Value,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StorylineToolCall>>,
    observation: Option<Value>,
) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: None,
        timestamp: None,
        source,
        message,
        reasoning_content,
        reasoning_effort: None,
        tool_calls,
        observation,
        metrics: None,
        model_name: None,
        llm_call_count: None,
        is_copied_context: Some(true),
        latency_ms: None,
        ttft_ms: None,
        extra: None,
    }
}
```

For every row with source `step_id = n`, assign:

```rust
let user_turn_id = context_count + 2 * step_id - 1;
let agent_turn_id = context_count + 2 * step_id;
```

Reject a non-positive step ID before multiplication. Use checked integer arithmetic and return `InputIssue::invalid("OpenAI corpus step_id overflows Storyline turn id")` if the formula overflows. Continue rejecting duplicate step IDs. Do not check continuity.

- [ ] **Step 4: Consume repeated snapshots without emitting duplicate turns**

Process the known message fields of every row so `role`, `content`, valid tool calls, linked tool IDs, and approved empty optionals are removed from the residual even when the message belongs to repeated history. Only first-row leading context and each row's final current user/output create turns.

Run:

```bash
cargo test -p persisting-pchronicle openai_imports_first_row_context_once_and_offsets_step_ids -- --nocapture
cargo test -p persisting-pchronicle corpus_preserves_run_group_and_user_agent_turns -- --nocapture
```

Expected: both PASS.

- [ ] **Step 5: Update zero-based legacy fixtures to the positive source-step contract**

In `crates/persisting-pchronicle/src/tests.rs`, change the OpenAI fixture `step_id` values in `openai_msg_preserves_user_and_llm_turns` and `convert_openai_msg_storyline_roundtrip_messages` from `0` to `1`. Add assertions that the two generated turn IDs are `1` and `2` when there is no context.

Run:

```bash
cargo test -p persisting-pchronicle openai_msg_preserves_user_and_llm_turns -- --nocapture
cargo test -p persisting-pchronicle convert_openai_msg_storyline_roundtrip_messages -- --nocapture
```

Expected: both PASS.

- [ ] **Step 6: Commit context and ID mapping**

```bash
git add -p crates/persisting-pchronicle/src/formats/openai_corpus.rs crates/persisting-pchronicle/src/tests.rs
git commit -m "feat(pchronicle): import OpenAI message context"
```

---

### Task 3: Replace physical recovery with canonical logical export

**Files:**
- Modify: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:274-874`
- Test: `crates/persisting-pchronicle/src/formats/openai_corpus.rs:1538-2054`
- Test: `crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs:1-90`

**Interfaces:**
- Consumes: context markers, mapped turns, metrics, and unknown residuals from Tasks 1-2.
- Produces: canonical `session_steps` rows with cumulative `messages`, current `response`, inverse step IDs, mapped metadata, and existing unknown-field carriage.
- Produces: `encode_context_turns(&[StorylineTurn]) -> Result<Vec<Value>>`, `storyline_interactions(&[StorylineTurn]) -> Result<Vec<(&StorylineTurn, &StorylineTurn)>>`, `encode_agent_response(&StorylineTurn) -> Result<Value>`, `encode_agent_history(&StorylineTurn) -> Result<Vec<Value>>`, and `openai_step_id(&StorylineDocument, i64, usize, &StorylineTurn, &StorylineTurn) -> Result<i64>`.

- [ ] **Step 1: Replace exact-source tests with a failing logical-roundtrip test**

```rust
#[test]
fn openai_logical_roundtrip_preserves_mapped_storyline_fields() {
    let input = json!({"session_steps": [
        {
            "session_id": "s",
            "step_id": 1,
            "agent_model": "model-1",
            "is_terminal": false,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "one"}
            ],
            "response": {"role": "assistant", "content": "first"}
        },
        {
            "session_id": "s",
            "step_id": 2,
            "agent_model": "model-1",
            "is_terminal": true,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "first"},
                {"role": "user", "content": "two"}
            ],
            "response": {"role": "assistant", "content": "second"}
        }
    ]});

    let first = parse_openai_msg_corpus_value(&input, "logical.json").unwrap();
    let encoded = recover_openai_msg_files(&first).unwrap().remove(0).document;
    let second = parse_openai_msg_corpus_value(&encoded, "logical.json").unwrap();
    assert_eq!(second, first);
    assert_eq!(encoded["session_steps"][0]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(encoded["session_steps"][1]["messages"].as_array().unwrap().len(), 4);
    assert_eq!(encoded["session_steps"][1]["step_id"], 2);
}
```

- [ ] **Step 2: Run the logical roundtrip and verify RED**

Run:

```bash
cargo test -p persisting-pchronicle openai_logical_roundtrip_preserves_mapped_storyline_fields -- --nocapture
```

Expected: FAIL because current recovery requires `step_id` in residual templates, preserves source output locations, and does not encode first-row context as cumulative history.

- [ ] **Step 3: Remove source-template recovery machinery**

Delete `OpenaiSourceRowTemplate`, `openai_source_row_templates`, `insert_openai_template_value`, `recover_openai_source_row`, `patch_openai_tool_calls`, and `remap_openai_pointer`. Remove `OpenaiEncodingMode::Recovery`; retain one canonical encoder for both projection and same-source recovery.

Keep `recover_openai_msg_files` grouping stories by OpenAI `source_document_id`/origin path, but have it call the canonical encoder. After canonical rows exist, restore source-format residual pointers directly with `PointerWrite::InsertOnly`; mapped fields are absent from residual by construction, so they cannot compete with canonical values.

- [ ] **Step 4: Encode context and cumulative messages**

For each Storyline, split the leading `is_copied_context == Some(true)` turns from interaction turns. Encode context roles as `system`, `user`, `assistant`, or `tool`. Iterate the remaining turns as required user/agent pairs:

```rust
let context_len = story
    .turns
    .iter()
    .take_while(|turn| turn.is_copied_context == Some(true))
    .count();
let context_count = i64::try_from(context_len)?;
let context = &story.turns[..context_len];
let interactions = storyline_interactions(&story.turns[context_len..])?;
let mut history = encode_context_turns(context)?;
for (interaction_index, (user, agent)) in interactions.into_iter().enumerate() {
    let step_id = openai_step_id(
        story,
        context_count,
        interaction_index,
        user,
        agent,
    )?;
    let mut messages = history.clone();
    messages.push(json!({"role": "user", "content": user.message}));
    let response = encode_agent_response(agent)?;
    rows.push(json!({
        "session_id": story.session_id,
        "step_id": step_id,
        "messages": messages,
        "response": response,
    }));
    history.push(json!({"role": "user", "content": user.message}));
    history.extend(encode_agent_history(agent)?);
}
```

`openai_step_id` uses the inverse ID formula for OpenAI-origin stories: `n = (agent.id - k) / 2`, then verifies user ID `k + 2n - 1` and agent ID `k + 2n`. For Storylines projected from another format it returns `interaction_index + 1` without inventing source metadata.

- [ ] **Step 5: Encode mapped row fields and env-state metrics**

Populate row `job_id`, `agent_id`, `agent_model`, `created_at`, reward/status fields, and a canonical `meta_json.env_state` object from the matching Storyline fields. Put the approved env metrics back under `env_state`; do not export arbitrary metrics as OpenAI metadata. Encode structured tool calls with `type=function`, JSON-string arguments, observations/results, and explicit reasoning when present.

Update tests that formerly asserted byte/model-exact source restoration:

- `openai_recovery_preserves_message_output_location_and_embedded_tool_encoding` must assert semantic message/tool-call values instead of the original output location;
- `openai_recovery_preserves_nondefault_source_fields_without_fabrication` must assert unmapped values remain available through `unknown_fields` and mapped values survive reparse;
- `openai_recovery_keeps_known_message_shape_and_argument_semantics` must compare reparsed Storyline semantics;
- Lance roundtrip tests must compare canonical encoded output or reparsed Storylines, not the original JSON model.

- [ ] **Step 6: Run logical and fixture roundtrips**

```bash
cargo test -p persisting-pchronicle openai_logical_roundtrip_preserves_mapped_storyline_fields -- --nocapture
cargo test -p persisting-pchronicle --test import_roundtrip_fixtures -- --nocapture
cargo test -p persisting-pchronicle corpus_import_and_recovery_roundtrip_through_lance --features lance-store -- --nocapture
```

Expected: all PASS.

- [ ] **Step 7: Commit canonical export**

```bash
git add -p crates/persisting-pchronicle/src/formats/openai_corpus.rs crates/persisting-pchronicle/tests/import_roundtrip_fixtures.rs
git commit -m "refactor(pchronicle): canonicalize OpenAI message export"
```

---

### Task 4: Verify CLI warnings, SQL projection, and the real corpus

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs:1347-1424`
- Test: `crates/persisting-pchronicle-cli/src/tests.rs`
- Verify: `data/cybergym_0729001.json`

**Interfaces:**
- Consumes: completed OpenAI mapping and canonical encoder from Tasks 1-3.
- Produces: user-visible warnings that contain only unmapped fields; evidence for the 46 MB corpus counts and queryable status metrics.

- [ ] **Step 1: Update the failing CLI warning test**

In `import_counts_shared_openai_root_unknown_once_across_sessions`, keep the root warning assertion and replace the old nullable-message warning expectations with:

```rust
for mapped in [
    "/step_id",
    "/is_terminal",
    "/is_truncated",
    "/is_session_completed",
    "/is_trainable",
    "/messages/*/role",
    "/messages/*/content",
    "/messages/*/name",
    "/messages/*/refusal",
    "/messages/*/tool_call_id",
] {
    assert!(!stderr.contains(mapped), "mapped OpenAI field warned: {mapped}\n{stderr}");
}
assert!(stderr.contains("source=openai-msg key=/vendor_root occurrences=1"));
```

Add one non-empty `vendor_row` field to each row and assert its normalized warning count is `2`.

- [ ] **Step 2: Run the CLI warning test and verify behavior**

```bash
cargo test -p persisting-pchronicle-cli import_counts_shared_openai_root_unknown_once_across_sessions -- --nocapture
```

Expected: PASS after Tasks 1-3; before updating assertions, the old test fails because nullable known fields no longer warn.

- [ ] **Step 3: Run all targeted OpenAI tests**

```bash
cargo test -p persisting-pchronicle openai -- --nocapture
cargo test -p persisting-pchronicle-cli openai -- --nocapture
cargo test -p persisting-pchronicle --test conversion_semantics -- --nocapture
cargo test -p persisting-pchronicle --test import_roundtrip_fixtures -- --nocapture
```

Expected: all PASS.

- [ ] **Step 4: Build the release CLI and import the real corpus**

```bash
cargo build -p persisting-pchronicle-cli --release
check_dir="$(mktemp -d /tmp/pchronicle-openai-map.XXXXXX)"
./target/release/pchronicle import \
  --format openai-messages \
  --from data/cybergym_0729001.json \
  --output "$check_dir/dataset" \
  --output-format storyline \
  >"$check_dir/import.json" \
  2>"$check_dir/import.stderr"
```

Check that `import.stderr` does not contain mapped warning keys:

```bash
if rg 'key=/session_steps/\*/(step_id|messages/\*/(role|content|tool_calls)|response/)' "$check_dir/import.stderr"; then
  exit 1
fi
```

- [ ] **Step 5: Query corpus counts and mapped status fields**

```bash
./target/release/pchronicle query "$check_dir/dataset" \
  'SELECT COUNT(*) AS trajectories FROM dataset.runs' --format jsonl
./target/release/pchronicle query "$check_dir/dataset" \
  'SELECT COUNT(*) AS turns FROM dataset.steps' --format jsonl
./target/release/pchronicle query "$check_dir/dataset" \
  'SELECT COUNT(*) AS tool_calls FROM dataset.tool_calls' --format jsonl
./target/release/pchronicle query "$check_dir/dataset" \
  "SELECT COUNT(*) AS completed FROM dataset.steps WHERE source = 'agent' AND metrics_json LIKE '%\"is_session_completed\":true%'" \
  --format jsonl
```

Expected: `trajectories=8`, `turns=964`, `tool_calls=461`, and `completed=4`.

- [ ] **Step 6: Run formatting, lint, and focused regression checks**

```bash
cargo fmt -p persisting-pchronicle -p persisting-pchronicle-cli -- --check
cargo clippy -p persisting-pchronicle -p persisting-pchronicle-cli --all-targets -- -D warnings
cargo test -p persisting-pchronicle
cargo test -p persisting-pchronicle-cli
```

Expected: targeted crates pass. Report unrelated failures without expanding into excluded subsystems.

- [ ] **Step 7: Commit CLI acceptance coverage**

```bash
git add -p crates/persisting-pchronicle-cli/src/tests.rs
git commit -m "test(pchronicle): verify OpenAI mapping warnings"
```
