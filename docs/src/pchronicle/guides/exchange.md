# Import and export trajectories

Use import and export at the interoperability boundary. Import creates a new
Dataset; export reconstructs complete trajectories from one Catalog Snapshot.

## Import into a new Dataset

```bash
pchronicle import --from input.json \
  --output ./imported --format atif
```

The target is create-only. pChronicle refuses an existing target instead of
silently appending or replacing it. Regular files can be auto-detected; stdin
must be finite and explicit:

```bash
cat input.json | pchronicle import --from - --stream \
  --output ./imported --format storyline
```

After import, inspect the new boundary:

```bash
pchronicle status ./imported
pchronicle analysis overview ./imported
```

## Export complete trajectories

```bash
pchronicle export --from ./imported \
  --output restored.json --format atif
```

Narrow the export with Source-local identity when needed:

```bash
pchronicle export --from ./imported --output one.json --format actf \
  --source source.json --session-id session-42 --strict
```

`--strict` fails when the target format cannot preserve the original exchange
document. Output files are create-only unless overwrite is requested explicitly.

Import/export is not a storage migration protocol and arbitrary SQL rows are
not exportable trajectories. See [Trajectory formats](../reference/formats/index.md)
for contracts and [Facts, projections, and revisions](../concepts/facts-and-projections.md)
for the layer boundary.
