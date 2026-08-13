# pChronicle CLI

Standalone command-line interface for browsing, querying, importing, exporting,
and serving pChronicle trajectory Datasets.

The current implementation provides `pchronicle ls` (also available as
`pchronicle list`), `pchronicle status`, and bounded read-only `pchronicle
query`. Other commands are present in the command tree and return a clear
not-yet-implemented error until their respective product increments land.

Small ATIF, OpenAI Messages, and ACTF Datasets for trying the commands live in
[`../../examples/data`](../../examples/data).
