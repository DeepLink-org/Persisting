# Dataset pins config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace user `config.toml` with nested `[pins.<name>]` and align all CLI/settings code and tests; no legacy compatibility.

**Architecture:** One `LocalSettings { pins }` map of `PinConfig` tables. Resolve `@name` and default from that map. Reject unknown TOML keys.

**Tech Stack:** Rust (`persisting-pchronicle-cli`), TOML via serde, existing CLI tests.

## Global Constraints

- No dual-read of `aliases` / `default_warehouse` / `alias_*`.
- No `PCHRONICLE_SETTINGS`.
- Secrets never printed by list/show.
- `just test persisting-pchronicle-cli` is the primary validation.

---

### Task 1: Settings model + resolve/write path

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/settings.rs`
- Modify: `crates/persisting-pchronicle-cli/src/lib.rs` (call sites)
- Modify: `crates/persisting-pchronicle-cli/src/server/catalog.rs` (parse naming)

- [ ] Replace `LocalSettings` with `pins: BTreeMap<String, PinConfig>`
- [ ] Implement pin/set/show/list/rename/unpin against `PinConfig`
- [ ] Point `resolve_default_warehouse` at `pins.default`
- [ ] Rename internal `alias_*` helpers to `pin_*` where they touch user config
- [ ] Drop `LEGACY_SETTINGS_ENV`

### Task 2: Tests

**Files:**
- Modify: `crates/persisting-pchronicle-cli/src/tests.rs`
- Modify: `crates/persisting-pchronicle-cli/tests/local_warehouse.rs`

- [ ] Assert on-disk `[pins.prod]` shape for S3/catalog pins
- [ ] Legacy `default_warehouse` / `aliases` samples hard-fail
- [ ] Keep default / lifecycle / catalog ls coverage green

### Task 3: User-facing docs

**Files:**
- Modify: `docs/src/en/pchronicle/reference/cli.md`
- Modify: `docs/src/zh/pchronicle/reference/cli.md`
- Modify: design/catalog notes if they imply old keys

- [ ] Document example `[pins.*]` config.toml
- [ ] Remove any `default_warehouse` / `aliases` wording

### Task 4: Verify

- [ ] `just test persisting-pchronicle-cli` (or targeted nextest filters if full suite is too long)
