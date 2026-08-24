# Import and export trajectories

Use import and export at the interoperability boundary. Import creates a new
Dataset; export reads complete trajectories from an existing Dataset.
Import and export accept ATIF, ACTF, OpenAI Messages, and Storyline JSON.
Import also accepts decode-only Codex (`codex`) and Claude Code (`claude-code`)
session JSONL. Export refuses those two formats.

## Import into a new Dataset

```bash
pchronicle import --from input.json \
  --to ./imported --input-format atif
```

The target is create-only. pChronicle refuses an existing target instead of
silently appending or replacing it. Regular files can be auto-detected. A
directory recursively imports `.json`, `.jsonl`, and `.ndjson` files while
preserving their relative paths in the default output. When `--input-format` is
omitted, each file is detected independently; JSON that is not a known
trajectory format is skipped with a warning:

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

A validated, non-empty canonical Event Store is detected before JSON scanning
and always creates Storyline Lance:

```bash
pchronicle import --from ./run/events.lance --to ./run/storyline
```

This mode accepts local and object-store URIs, never mutates the source, and
refuses an existing destination. Its JSON result reports `format: "events"`,
`output_format: "storyline-lance"`, and `fact_rows`; it omits `input_bytes`.
Explicit `--output-format preserve` and JSON exchange `--input-format` values are
invalid for canonical events.

In the squashed Dataset, `_file_` is `.` for all normalized rows:

```bash
pchronicle query ./normalized \
  --sql 'SELECT _file_, COUNT(*) AS runs FROM dataset.runs GROUP BY _file_'
```

`document_id` and `session_id` must be globally unique across the inputs. A
collision fails the complete import and names both original paths. Successful
Storyline output does not retain those paths as query provenance; use the
default preserve output when file boundaries matter.

ATIF `.jsonl` and `.ndjson` inputs decode every non-empty record. Symbolic
links found while walking a directory are skipped; an explicitly named link to
a regular file retains single-file behavior. The directory is published
atomically only after every input and the selected physical output succeed.
Stdin must be finite and explicit:

```bash
cat input.json | pchronicle import --from - \
  --to ./imported --input-format openai-messages
```

After import, inspect the new boundary:

```bash
pchronicle status ./imported
pchronicle analysis overview ./imported
```

## Export complete trajectories

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
not exportable trajectories. See [Trajectory formats](../reference/formats/index.md)
for contracts and [data contracts and revisions](../concepts/facts-and-projections.md)
for the internal layer boundary.
