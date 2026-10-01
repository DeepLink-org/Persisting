# Reproducible examples

Run `just examples-pchronicle` (or `just examples`) from the repository root.
The deterministic CLI examples cover Dataset lifecycle, analysis, cross-Dataset
SQL, storage performance, format roundtrips, and direct OpenAI/ACTF queries.

Each example manages its own `.work/` directory and prints its output for
inspection. The suite builds release executables and requires Cargo, Python 3,
`jq`, and standard POSIX tools.

See [pChronicle guides](../pchronicle/guides/index.md) and the repository's
`examples/pchronicle/` and `examples/data/` directories.
