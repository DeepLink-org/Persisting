# pChronicle CLI

Standalone command-line interface for browsing, querying, importing, exporting,
and serving pChronicle trajectory Datasets.

The current implementation provides `pchronicle ls` (also available as
`pchronicle list`), `pchronicle status`, bounded read-only `pchronicle query`,
Source-local `pchronicle find`, and create-only local `pchronicle import` for
ATIF, OpenAI Messages, and ACTF files. Import also accepts a finite stdin stream
when `--from - --stream --format FORMAT` is explicit. `pchronicle export`
selects complete Trajectories from one immutable Catalog Snapshot and writes
ATIF, OpenAI Messages, ACTF, or Storyline JSON to a new local file or finite
stdout stream. Other commands are present in the command tree and return a
clear not-yet-implemented error until their respective product increments land.

Small ATIF, OpenAI Messages, and ACTF Datasets for trying the commands live in
[`../../examples/data`](../../examples/data).
