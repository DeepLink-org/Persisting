# pPilot

**Durable Run Orchestrator and analysis CLI.**

pPilot is a first-class Persisting component alongside pVisor and pChronicle:

- pPilot plans, schedules, resumes, and reconciles many Runs;
- pVisor owns execution and the lifecycle of each Run/Attempt;
- pChronicle owns canonical Run history and derived views.

pPilot consumes Run contracts and results and is the user-facing entry point
for querying trajectory data. It does not own Agent protocol adaptation,
execution drivers, or trajectory storage formats; those query implementations
remain in pChronicle.

```bash
# The public binary is feature-gated so library-only builds stay lightweight.
cargo build -p persisting-ppilot --features cli --bin ppilot

ppilot run plan.py --workers 8 --sink ./results
ppilot self-test

ppilot query ./storyline-store \
  --sql "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source"
ppilot query ./trajectories.ndjson --source atif --sql-file analysis.sql
```

`ppilot query` registers the same `runs`, `steps`, and `tool_calls` tables for
Storyline Lance and ATIF inputs. It accepts one read-only SQL statement and
writes JSONL rows to stdout. Use `--sql-file -` to read SQL from stdin.
