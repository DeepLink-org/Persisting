# Storyline `/prompt` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. User asked to execute this spec in-session.

**Goal:** Map ACTF `system_prompt` / `user_content` onto Storyline document `/prompt` plus turn `/prompt` overlays, without changing `/msg`.

**Architecture:** Optional `{system, user}` object on the document and each turn. First non-empty pair is the document baseline; differing steps write a full-replace overlay. Lance stores the objects as `prompt_json` on `runs` and `steps`.

**Tech Stack:** Rust, serde, Storyline `storyline/v1`, ACTF convert, three-table Lance.

## Global Constraints

- `schema_version` stays `storyline/v1`
- `/turns/{t}/msg` stays `assistant_content.content`
- Prompt is not `/task`, `env`, or `extra`
- OpenAI / ATIF / AgenticMD / Events do not gain first-class prompt fields
- Attempt `extra` / `meta` stay residual
- Do not change TTAS, Queue, Search, or `persisting-dlcapt`
- Do not commit unless the user asks

---

### Task 1: Wire type + validation

**Files:** `crates/persisting-pchronicle/src/formats/storyline.rs`, `crates/persisting-pchronicle/src/model.rs`

Add `StorylinePrompt { system, user }`, document and turn optional `prompt`, validation, `effective_prompt`.

### Task 2: ACTF import/export

**Files:** `crates/persisting-pchronicle/src/convert/actf.rs`

Baseline + overlay algorithm; consume residuals; export from effective prompt.

### Task 3: Lance projection

**Files:** `crates/persisting-pchronicle/src/store/storyline/{model,rows,content}.rs`

`runs.prompt_json` and `steps.prompt_json`; missing columns decode as absent.

### Task 4: Downstream literals + RFCs

**Files:** Gateway / CLI / other `StorylineTurn` literals; RFC-0001, RFC-0004; `storyline-lance.md`

### Task 5: Verify

`cargo test -p persisting-pchronicle --lib` and targeted convert/store tests.
