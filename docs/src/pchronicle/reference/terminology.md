# Product terminology

pChronicle uses a small, consistent vocabulary in the CLI, Web UI, and
task-oriented documentation:

| Term | Meaning |
| --- | --- |
| **Dataset** | A mounted collection of Agent run data. |
| **Source file** | One input file or storage source inside a Dataset. |
| **Run** | One stored Agent execution record. A Session ID identifies the record; an optional Run ID may group it with runtime activity. |
| **Step** | One ordered user, agent, system, or tool-related entry in a Run. |
| **Event** | A low-level recorded or reconstructed item linked to a Step. |
| **Result** | Rows returned by a query or analysis. |

The UI uses **Recorded events** for events captured directly from a runtime and
**Reconstructed events** for events derived from imported run data. It never
labels reconstructed events as raw data.

The following terms are reserved for technical documentation and APIs:

- **Storyline** is the normalized storage and exchange model behind Runs and
  Steps.
- **Canonical Event** is the append-only recorded event model.
- **Dataset Catalog** is the immutable internal snapshot used to discover and
  query mounted Datasets.
- **projection**, **revision**, **fragment**, and **column page** describe
  storage and consistency mechanisms, not primary user workflows.

Older API paths and schema fields may retain these technical names for
compatibility. User-facing labels follow the simpler vocabulary above.

