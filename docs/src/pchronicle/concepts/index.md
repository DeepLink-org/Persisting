# Dataset

**Dataset is a path.** It is the Agent trajectory store pChronicle opens: a
local directory or file, or an object-store URI prefix. Aliases (`@name`) are
locators; after resolution the engine only sees the path.

A Dataset can be written as:

- a local directory or file (`./local/path`);
- an object-store URI prefix (`s3://bucket/prefix`);
- a user alias that resolves to either location (`@alias-name`).

## Addressing a Dataset

Bare values are paths or URIs. A leading `@` explicitly selects an alias:

```text
prod       local relative path ./prod
@prod      Dataset alias named prod
```

This keeps command behavior stable when a directory and an alias have the same
name. Create and inspect aliases with `pchronicle alias`; choose the local
Dataset used when an argument is omitted with `pchronicle default`.

## What commands expose

pChronicle discovers the supported run data inside the Dataset and
normalizes compatible fields into query tables such as `runs`, `steps`, and
`tool_calls`. Each read command uses an internally consistent view, even if the
underlying location changes while the command is running.

That is the complete user model needed by the CLI. Each read command pins a
Snapshot of the path. Storage discovery, version pinning, facts, projections,
and revisions are implementation and data contract details; consult
[Design](../design/index.md) only when those boundaries matter to your
integration.

Continue with [common workflows](../guides/index.md), the
[product terminology](../reference/terminology.md), or the
[CLI reference](../reference/cli.md).
