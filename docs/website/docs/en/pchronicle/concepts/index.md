# pChronicle concepts

pChronicle is built around a **Dataset**, which is a path. Read these concepts
before querying history or integrating storage.

:::note When this section helps
Use this section after the first query when you need to explain where a result
came from, keep a stable Snapshot while investigating, or choose between
canonical records and a derived view.
:::

Follow these articles in order:

1. [Dataset, Source, and Snapshot](dataset-and-source.md) explains how a path
   is addressed, pinned, and read consistently.
2. [Recorded data, views, and versions](facts-and-projections.md) separates
   canonical facts from the views used to inspect and exchange them.

Storage mechanisms and current implementation belong to
[pChronicle Design](../design/index.md). Continue with
[common workflows](../guides/index.md) or the
[CLI reference](../reference/cli.md).

After this section you should be able to describe a result by its Dataset,
Source, and Snapshot instead of treating a query output as an untraceable file.
