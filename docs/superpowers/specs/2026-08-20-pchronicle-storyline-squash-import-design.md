# pChronicle Storyline Squash Import Design

## Status

Approved in conversation on 2026-08-20. This document defines the intended behavior before implementation.

## Context

`pchronicle import --output-format storyline` currently opens one `StorylineLanceStore` below each
input Source path. Importing a directory therefore reproduces the input hierarchy and places a complete
Lance Store at every leaf. A directory containing many ACTF files can produce dozens of nested
`CURRENT`, `generations`, and `objects.lance` trees even though the user requested one output Dataset.

Storyline output is a normalized Dataset, not a preservation format. Its import behavior should match a
Git-style squash: combine the decoded histories into one published snapshot and remove the physical
boundaries between the original Sources.

## Goals

- Make every `--output-format storyline` import produce exactly one Storyline Lance Store at the output
  root.
- Apply the same root-Store layout to directory, regular-file, and stdin inputs.
- Decode Sources incrementally and feed one bounded Storyline Store write instead of collecting the
  complete directory in memory.
- Preserve Storyline identities without prefixing, rewriting, or silently deduplicating them.
- Fail the whole import atomically when any Source is invalid or identities collide globally.
- Keep the import JSON response schema and `--output-format preserve` behavior compatible.

## Non-goals

- Do not add a `squash` subcommand or an opt-in squash flag.
- Do not add a compatibility flag for the old one-Store-per-Source Storyline layout.
- Do not add input-file provenance to the Storyline wire model, Lance schemas, or query tables.
- Do not migrate or rewrite existing imported Datasets.
- Do not make every source-format decoder record-streaming; a decoder may still materialize one Source.
- Do not add a total-directory byte or trajectory limit.
- Do not change TTAS, Queue, Search, or standalone `persisting-dlcapt`.

## User-facing contract

The existing command remains the complete interface:

```sh
pchronicle import \
  --format actf \
  --from INPUT \
  --output OUTPUT \
  --output-format storyline
```

For a directory, regular file, or stdin, `OUTPUT` is one Store:

```text
OUTPUT/
├── CURRENT
├── generations/
│   └── gen-.../
│       ├── runs.lance/
│       ├── steps.lance/
│       └── tool_calls.lance/
└── objects.lance/
```

No input-relative directories are created inside `OUTPUT`. Catalog discovery sees one physical Source
whose path is `"."`; consequently `_file_` is `"."` for every row in `dataset.runs`, `dataset.steps`,
and `dataset.tool_calls`. Commands that specify a Source must use `.` or omit the Source filter.

`--output-format preserve` remains the source-preserving mode. It retains the current relative file
layout and permits identities that are unique only within their physical Source.

## Architecture

### Two explicit output paths

`run_import` dispatches by output format after validating input and output arguments:

1. The preserve path continues to stage each input Source at its existing relative output path.
2. The Storyline path creates one `StorylineLanceStore` at the staging root and invokes one
   `replace_storyline_stream` operation for all input Sources.

The Storyline path must not call `StorylineLanceStore::open` once per input Source. A successful run has
one Store writer, one logical snapshot publication inside staging, and one Dataset publication from
staging to `OUTPUT`.

### Import Source adapter

Introduce a CLI-internal Source descriptor containing the physical input path when one exists, its
Dataset-relative diagnostic path, and the information needed for format detection. Directory scanning
continues to produce descriptors in the current stable order. Regular-file and stdin imports are adapted
to a one-element Source sequence so all Storyline inputs use the same pipeline.

A lazy iterator over those descriptors performs the following work for one Source at a time:

1. open and bounded-read the Source;
2. resolve the requested or detected exchange format;
3. decode and validate its Storyline documents;
4. collect existing unknown-field observations;
5. update checked aggregate counts;
6. validate global identities with diagnostic provenance;
7. yield documents to `replace_storyline_stream`;
8. release that Source's bytes and decoded document collection before opening the next Source.

The adapter yields `Result<StorylineDocument>` so an error discovered after earlier chunks have been
written aborts the Store operation. The Store's staged `CURRENT` is never published on that failure, and
the outer temporary Dataset is removed.

### Resource bounds

This design is Source-streaming, not necessarily JSON-record-streaming. Peak CLI memory consists
primarily of:

- one Source's input bytes and decoded Storyline documents;
- the Store's bounded normalization/write chunk;
- aggregate import metadata and unknown-field warnings;
- a global identity index containing IDs and compact references to diagnostic Source paths.

The identity index grows with the number of trajectories, but the full decoded contents of prior Sources
do not remain resident. An explicitly supplied `--max-input-bytes` applies independently to every file or
to stdin, as it does today. No aggregate directory limit is introduced.

## Identity and Source semantics

The squash preserves the decoded `run_id`, `document_id`, and `session_id` values exactly. It does not
prefix them with a path or generate replacement IDs.

The merged Store requires global uniqueness for both `document_id` and `session_id`. The Source adapter
tracks the first diagnostic Source path for each value before yielding a document. A repeated value,
whether it occurs in another Source or in the same Source, returns an invalid-input error containing:

- the identity field name;
- the conflicting value;
- the first Source's relative diagnostic path;
- the second Source's relative diagnostic path.

`run_id` is preserved but is not a new squash collision key. Existing Storyline validation continues to
govern any other identity invariants.

The diagnostic path exists only during import. It is used in decoder, validator, identity-collision, and
unknown-field messages, but is not written into Storyline origin metadata or Lance tables. After a
successful squash, the original Source boundary cannot be recovered through `_file_`.

## Atomicity and failure behavior

The current create-only publication model remains authoritative:

1. Validate that `OUTPUT` names a new local Dataset path.
2. Create a temporary staging directory beside `OUTPUT`.
3. Build and verify the single Storyline Store at the staging root.
4. Sync the staging directory.
5. Publish with the existing no-replace atomic rename.
6. Sync the output parent and disarm cleanup.

Any read, detection, decoding, validation, collision, Store, indexing, sync, or publication failure aborts
the command. Before the final rename, cleanup removes staging and `OUTPUT` does not exist. Existing output
paths are never overwritten. Intermediate Lance versions written inside staging before a late error are
unreachable and disappear with staging cleanup.

## Import response

The serialized `ImportResponse` schema does not change:

- `dataset_uri` is the published root Store path;
- `sources` is the number of logical input Sources successfully consumed, not the number of physical
  Sources in the result;
- `trajectories` is the total number of merged Storyline documents;
- `input_bytes` is the checked sum of input Source byte counts;
- `output_format` remains `storyline-lance`;
- for regular-file and stdin imports, optional `source_path` and `format` retain their current input
  metadata meaning;
- directory imports continue to omit the single-Source-only `source_path` and `format` fields.

No `squashed` response field is added. The requested output format fully determines the behavior.

## Compatibility

This is an intentional layout change for newly created Storyline outputs. Code that appends an input
relative path to `OUTPUT` to find a nested Store must instead open `OUTPUT` itself. SQL that assumed the
original path in `_file_` must use `.` or remove the Source predicate.

Existing nested Storyline Datasets remain readable because catalog discovery is unchanged. They are not
automatically migrated. Users that need multiple physical Storyline Stores can run separate imports with
separate output paths. Users that need original file boundaries in one Dataset should select
`--output-format preserve`.

## Alternatives considered

### Import-integrated squash — selected

Decode each Source and feed one root Store operation. This performs the least I/O, exposes one atomic
result, and makes the command's output match its Dataset-level destination.

### Per-Source staging followed by a merge pass

This would reuse the old writer path, then reopen and combine every temporary Store. It doubles much of
the storage I/O, consumes extra temporary space, and adds another failure phase without preserving any
user-visible value.

### Separate `squash` subcommand

This is composable but requires users to first create the complex layout they want to eliminate and then
run a second command. It also creates avoidable policy questions about deleting or retaining the input
Dataset.

## Test strategy

Implementation follows RED then GREEN. Update tests that currently require nested Storyline Stores and
add focused coverage for the new contract:

1. A directory containing multiple supported Sources creates `OUTPUT/CURRENT` and no nested `CURRENT`;
   catalog discovery returns one Source named `.`, and SQL observes every imported trajectory with
   `_file_ = '.'`.
2. Regular-file and stdin Storyline outputs both create the same root-Store layout.
3. Mixed supported input formats continue to decode in stable scan order and produce correct aggregate
   counts and unknown-field warnings.
4. Cross-Source duplicate `document_id` and duplicate `session_id` cases each fail with both relative
   paths and leave no output.
5. Same-Source duplicate identity cases use the same diagnostic contract.
6. A malformed later Source, after enough earlier documents to flush a Store chunk, still leaves no
   published Dataset.
7. Import JSON remains schema-compatible and reports logical Source, trajectory, and byte totals.
8. Preserve-mode tests continue to assert original relative paths and Source-local identity behavior.
9. Existing old-layout discovery fixtures remain readable, demonstrating that no reader migration is
   required.

Focused verification commands:

```text
cargo test -p persisting-pchronicle-cli
cargo fmt -p persisting-pchronicle-cli -- --check
cargo clippy -p persisting-pchronicle-cli --all-targets -- -D warnings
```

Any broader check that encounters unrelated dirty-worktree failures is reported separately and does not
expand this task into excluded subsystems.

## Documentation changes

Update the following user-facing surfaces that currently describe or imply one Store per Source:

- pChronicle CLI help for `--output-format storyline`;
- `crates/persisting-pchronicle-cli/README.md`;
- `docs/src/pchronicle/reference/cli.md`;
- exchange guides or examples that inspect the old nested layout.

Documentation must show the root-Store tree, define logical input `sources` versus the single physical
Source, state `_file_ = "."`, describe global identity collision failure, and point users to `preserve`
when Source boundaries matter.

## Acceptance criteria

The change is complete when all of the following are true:

- every successful Storyline import shape publishes one queryable Store at `OUTPUT`;
- directory imports no longer reproduce input paths as nested Lance Stores;
- merged rows are queryable through Source `.`;
- duplicate document or session identity failures name both input paths and publish nothing;
- late decode or write failures publish nothing;
- preserve output and existing nested-Store reads remain compatible;
- import response fields retain their documented schema and meanings;
- focused tests, formatting, and Clippy pass;
- CLI documentation describes the new behavior without promising retained provenance.
