# Storyline task / env / tool response Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Promote ACTF eval/budget/time/tool status and OpenAI env_state into first-class Storyline `task`, document/turn timestamps, turn `env`, and tool `kind`/`response`.

**Architecture:** Extend `storyline/v1` optional wire structs in `formats/storyline.rs`, map them in ACTF and OpenAI converters with 1:1 JSON Pointer authority, and project the same objects onto the existing three Lance tables as additive nullable columns.

**Tech Stack:** Rust 2021, serde/serde_json, Arrow/Lance Storyline store.

**Spec:** docs/superpowers/specs/2026-08-22-storyline-task-env-response-design.md

## Global Constraints

- Keep `schema_version` as `storyline/v1`; all new fields optional.
- Each source JSON Pointer has exactly one authoritative Storyline target.
- Do not map `system_prompt` / `user_content` / attempt `extra` / `meta`.
- Do not copy `/metrics` env_state keys into `env`.
- Do not modify TTAS, Queue, Search, or persisting-dlcapt.
- Do not commit unless the user asks.
- New Lance columns must decode as absent on older tables (`*_if_present`).

## File Map

- `crates/persisting-pchronicle/src/formats/storyline.rs` — wire structs and validation
- `crates/persisting-pchronicle/src/convert/actf.rs` — ACTF import/export
- `crates/persisting-pchronicle/src/formats/openai_corpus.rs` — OpenAI env mapping
- `crates/persisting-pchronicle/src/store/storyline/{model,rows,content}.rs` — Lance projection
- `docs/src/rfcs/0001-storyline-format.md`, `0004-actf-format.md`, `0009-openai-messages-format.md`

---

### Task 1: Storyline wire types

**Files:** `crates/persisting-pchronicle/src/formats/storyline.rs`

- [ ] Add `StorylineTask`, `StorylineEnv`, `StorylineTaskLlm`, `StorylineTaskResult`, `StorylineToolResponse`
- [ ] Add document `task` / `started_at` / `finished_at`, turn `env` / `finished_at`, tool `kind` / `response`
- [ ] Validate empty-task rejection, positive `k`, empty `kind` as missing
- [ ] Tests: JSON roundtrip of the new fields; empty objects omitted

### Task 2: ACTF mapping

**Files:** `crates/persisting-pchronicle/src/convert/actf.rs`

- [ ] Import result/budget/llm/k/timestamps/tool kind+response
- [ ] Lift `task_correct`/`correct`/`status`/`score` into `final_metrics`
- [ ] Stop recording mapped keys as unknown fields
- [ ] Export restores those keys from first-class fields
- [ ] Update `actf_noncanonical_source_fields_are_unknown_without_source_extra`

### Task 3: OpenAI env mapping

**Files:** `crates/persisting-pchronicle/src/formats/openai_corpus.rs` and tests

- [ ] Stable env keys → `/task/env`; step keys → response turn `/env`
- [ ] Equal later values consumed; unequal values become turn env
- [ ] Export writes env back onto rows/`meta_json.env_state`
- [ ] Update `openai_only_reports_unmapped_source_fields`

### Task 4: Lance three-table projection

**Files:** `store/storyline/model.rs`, `rows.rs`, `content.rs`

- [ ] runs: `task_json`, `started_at`, `finished_at` (+ source json)
- [ ] steps: `env_json`, `finished_at` (+ source json)
- [ ] tool_calls: `kind`, `response_json`
- [ ] Roundtrip through `split_storyline` / `reconstruct_storyline`

### Task 5: RFC docs

**Files:** `docs/src/rfcs/0001-storyline-format.md`, `0004-actf-format.md`, `0009-openai-messages-format.md`

- [ ] Wire tables and mapping rows matching the spec
