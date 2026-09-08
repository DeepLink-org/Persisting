# Dataset pins disk config redesign

## Goal

Replace the legacy user `config.toml` shape (`default_warehouse` + `aliases` /
`alias_*` tables) with a single nested `[pins.<name>]` model that matches the
`pchronicle dataset pin|…` CLI. No migration, no dual-read, no compatibility
shims.

## Decisions

| Topic | Choice |
| --- | --- |
| Default Dataset | Ordinary pin named `default` under `[pins.default]` |
| Credentials / endpoint / region | Nested optional fields on the same pin table |
| Old config keys | Hard fail via `deny_unknown_fields` |
| Built-in `@codex` / `@claude` / `@claude-code` | Not written to disk; synthesized in `dataset list` only |
| Env | Keep `PCHRONICLE_CONFIG`; drop `PCHRONICLE_SETTINGS` |

## Schema

```toml
[pins.default]
uri = "/absolute/local/warehouse"

[pins.prod]
uri = "s3://bucket/evals"
endpoint = "http://127.0.0.1:9000"
region = "us-west-2"
access_key = "..."
secret_key = "..."

[pins.team]
uri = "catalog://127.0.0.1:8081"
access_key = "USER_AK"
secret_key = "USER_SK"
```

### Field rules

- `uri` is required on every pin.
- `endpoint` / `region` only for `s3://`.
- `access_key` / `secret_key` must appear together; allowed for `s3://` and
  `catalog://`.
- `catalog://` rejects `endpoint` / `region` and requires credentials.
- `default` must resolve to a local directory (create on pin if missing).
- `dataset list` / `show` never print secrets.

### Rust model

```rust
struct LocalSettings {
    pins: BTreeMap<String, PinConfig>,
}

struct PinConfig {
    uri: String,
    endpoint: Option<String>,
    region: Option<String>,
    access_key: Option<String>,
    secret_key: Option<String>,
}
```

Internal helpers and symbols use `pin` / `pins` naming. Built-in path expanders
may keep function names that refer to vendor roots, but user-config paths do
not say `alias`.

## CLI behavior (unchanged surface)

- `dataset pin|set|show|list|rename|unpin` — same commands.
- Omitting `DATASET_URI` reads `pins.default`.
- `@default` and `@name[/suffix]` resolve through `pins`.
- Bare `@team` / `@team/` on `ls` lists Directory libraries; other commands
  still require `@team/<dataset>`.

## Errors

- Unknown top-level keys (including legacy `aliases`, `default_warehouse`,
  `alias_*`) → TOML parse error.
- Incomplete `access_key` / `secret_key` pair → load or write failure.
- Missing `default` when a command needs it → same user message pointing at
  `pchronicle dataset pin default <LOCAL_DATASET>`.

## Testing

- Round-trip pin/set/show/list/rename/unpin writes `[pins.*]` only.
- S3/catalog credentials live under `[pins.NAME]`, not side tables.
- Legacy sample config fails closed.
- Default pin still restricted to local directories.
- Built-in pins still appear in list JSON without being persisted.

## Out of scope

- Automatic migration from old files.
- Splitting secrets into a second file.
- Deduplicating every `pin default` / `set default` validation branch beyond
  what the new model naturally shares.
