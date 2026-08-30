# Product terminology

pChronicle uses a small, consistent vocabulary in the CLI, Web UI, and
task-oriented documentation:

| Term | Meaning |
| --- | --- |
| **Dataset** | One Agent trajectory store identified by a **path** (local path or object-store URI). |
| **Source file** | One input file or storage source inside a Dataset. |
| **Run** | One stored Agent execution record. A Session ID identifies the record; an optional Run ID may group it with runtime activity. |
| **Step** | One ordered user, agent, system, or tool-related entry in a Run. |
| **Event** | A low-level recorded or reconstructed item linked to a Step. |
| **Result** | Rows returned by a query or analysis. |
| **Snapshot** | The write/read sync protocol after a path is opened: which Sources exist and which version each is pinned to for one operation. |

The UI uses **Recorded events** for events captured directly from a runtime and
**Reconstructed events** for events derived from imported run data. It never
labels reconstructed events as raw data.

The following terms are reserved for technical documentation and APIs:

- **Storyline** is the normalized storage and exchange model behind Runs and
  Steps.
- **Canonical Event** is the append-only recorded event model.
- **Directory** is the optional platform locator that resolves a name to a path
  and issues a ticket. CLI flags and HTTP paths may still say `catalog`.
- **Snapshot** (in APIs, sometimes still `DatasetCatalogSnapshot`) pins Source
  membership and versions. It is not the Directory listing.
- **projection**, **revision**, **fragment**, and **column page** describe
  storage and consistency mechanisms, not primary user workflows.

Aliases (`@name`), Warehouse mount names, and Directory library names are
locators. After resolution the engine only sees a path.

Older API paths and schema fields may retain these technical names for
compatibility. User-facing labels follow the simpler vocabulary above.

See the [Dataset interface](../concepts/index.md) and
[Discover and query](../guides/discover-and-query.md) for how these terms appear
in workflows.
