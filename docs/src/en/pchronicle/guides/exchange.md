# Import and export Runs

Use import and export at the interoperability boundary. Import creates,
appends to, or replaces a Dataset; export reads complete Runs from an existing Dataset.
Import and export accept ATIF, ACTF, OpenAI Messages, Storyline JSON, and
record-level Compact JSONL.
Import also accepts decode-only Codex (`codex`) and Claude Code (`claude-code`)
session JSONL. Export refuses those two formats.

Compact JSONL keeps one JSON object per row without assigning trajectory
semantics. Use `--input-format compact-jsonl` or
`--output-format compact-jsonl`; see the [CLI reference](../reference/cli.md)
for the `--column` mapping and snapshot-sync restrictions. Records without a
usable `id` receive a stable `source_filename#line_number` identity; export
preserves original input bytes. Successful compact import also writes a leaf
`chronicle.manifest` at the dataset root so later discovery can avoid opening
Lance only to classify the tree ([RFC-0015](../../rfcs/0015-chronicle-manifest.md)).

## Import into a new Dataset

```bash
pchronicle import --from input.json \
 --to ./imported --input-format atif
```

The default create behavior refuses an existing target. Use `--append`
for an existing Storyline Dataset; duplicate `document_id` values receive a
`#N` suffix by default, or can be skipped with `--on-duplicate skip`. Use
`--replace` to stage the complete import and atomically replace an existing
local Dataset after confirmation; replacement requires interactive confirmation
or `--yes`. Object-store Dataset replace clears the destination prefix before writing
(not atomic; an interrupted replace may leave the target empty).
Regular files can be auto-detected. A
directory recursively imports `.json`, `.jsonl`, and `.ndjson` files while
preserving their relative paths in the default output. When `--input-format` is
omitted, each file is detected independently; JSON that is not a known
run data format is skipped with a warning:

```bash
pchronicle import --from ./corpus --to ./imported
pchronicle import --from ./codex-sessions --to ./codex-ds --input-format codex
pchronicle import --from ./claude-sessions --to ./claude-ds --input-format claude-code
```

The default output preserves input bytes. To normalize and squash all decoded
inputs into one Storyline Lance Store at the output root, select Storyline
output:

```bash
pchronicle import --from ./corpus --to ./normalized \
 --output-format storyline
```

## Export complete Runs

```bash
pchronicle export --from ./imported \
 --to restored.json --output-format atif
```

Narrow the export with file path and external identity when needed:

```bash
pchronicle export --from ./imported --to one.json --output-format actf \
 --source source.json --session-id session-42 --strict
```

`--strict` fails when the target format cannot preserve the original exchange
document. Output files are create-only unless overwrite is requested explicitly.

Import/export is not a storage migration protocol and arbitrary SQL rows are
not exportable Runs. For exact flags, see the
[`pchronicle` CLI reference](../reference/cli.md). See
[Run data formats](../reference/formats/index.md) for contracts and
[trajectory data and versions](../concepts/facts-and-projections.md)
for the internal layer boundary.
