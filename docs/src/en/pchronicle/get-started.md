# Explore your first Dataset

Learn three everyday commands with sample data, then choose a topic or try your own Dataset. The introductory walkthrough reads data; the exchange topic performs writes only in an isolated temporary workspace.

!!! tip "The pChronicle loop"

    **Open → summarize → ask → locate → share.** Start with the built-in
    walkthrough, then bring the same habits to real trajectory data.

## 1. Run the guided sample

Install the CLI first if needed (`pip install persisting`). No checkout, account, running service, or existing Dataset is required:

```bash
pchronicle onboard
```

The default walkthrough (`onboard basics` is equivalent) asks three questions:

| Question | Command you learn | What to notice |
| --- | --- | --- |
| What data is here? | `list` | Files and child Datasets at this path |
| How much happened? | `stats overview` | Trajectories, steps, and errors |
| Which session has the most steps? | `query --sql …` | A small result from a real read-only query |

The tutorial executes these commands for you and prints their results. In an
interactive terminal, press **Enter** to continue or **q** to quit. You do not
need to copy temporary commands into another terminal. The current CLI tutorial
text is Chinese; the commands and data are the same for both documentation languages.

!!! success "Ready for your own data"

    You have used `pchronicle <operation> <dataset> [options]`. You can list data,
    inspect a summary, and ask one question. The sample directory is deleted on
    exit, so do not reuse its printed path afterward.

## Learn one topic at a time

Every topic works without a path using built-in data. Start with the subject
you need; you do not have to complete the full tutorial first.

| I want to… | Run | What happens |
| --- | --- | --- |
| repeat the short introduction | `pchronicle onboard basics` | Three read-only operations |
| understand the vocabulary | `pchronicle onboard concepts` | Dataset, Source, Snapshot, and tables explained |
| inspect data readiness | `pchronicle onboard inspect` | Listing and health checks |
| get summaries without SQL | `pchronicle onboard analyze` | Overview and tool-use reports |
| write my own questions | `pchronicle onboard query` | Schema, steps, tool calls, and output formats |
| work across formats | `pchronicle onboard formats` | Query bundled ATIF, ACTF, and OpenAI examples together |
| locate particular records | `pchronicle onboard find` | Record coordinates and matching syntax |
| bring data in or take it out | `pchronicle onboard exchange` | Import/export in an isolated temporary workspace |
| browse through Web/API | `pchronicle onboard serve` | Configuration examples; no server is started |

For the complete curriculum, run `pchronicle onboard all`. Topics use the same
Enter/q controls as the introduction. Use `--no-pause` for unattended output;
piping or redirecting also disables prompts and produces Markdown:

```bash
pchronicle onboard --no-pause > introduction.md
pchronicle onboard query --no-pause
pchronicle onboard all --no-pause > full-walkthrough.md
```

## 2. Open a Dataset you own

First repeat the introduction against existing data:

```bash
pchronicle onboard ./trajectory-data
pchronicle onboard query ./trajectory-data
```

`basics`, `inspect`, `analyze`, `query`, and `find` accept an optional Dataset.
These lessons read your data; `formats` and `exchange` continue to use built-in
examples. Replace `./trajectory-data` below with a local path, object-store URI prefix, or
Dataset pin such as `@prod`:

```bash
pchronicle list ./trajectory-data
pchronicle stats overview ./trajectory-data
```

`list` lists files and child Datasets at the current directory level. `stats overview` gives
a stable summary before you write SQL. Use JSON when the result will feed a
script:

```bash
pchronicle list ./trajectory-data --format json
```

## 3. Ask one bounded question

Inspect the schema, then keep the first query small and reproducible:

```bash
pchronicle query ./trajectory-data --sql "DESCRIBE dataset.steps"
pchronicle query ./trajectory-data \
  --sql "SELECT session_id, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY session_id
         ORDER BY steps DESC"
```

Queries are read-only and constrained by explicit resource budgets. Use
`--format jsonl|csv` and `--output` when passing results to another tool.

!!! success "Checkpoint: the answer is repeatable"

    Record the Dataset path, query, and output format. Rerunning against unchanged
    data lets you compare results; the same path alone does not freeze future changes.

## 4. Locate the evidence behind an answer

Move from a summary to the matching steps or sessions:

```bash
pchronicle find ./trajectory-data --match "timeout" --format json
pchronicle find ./trajectory-data --session-id session-42
```

If an external ID repeats, include `--source` to keep the reference durable:

```bash
pchronicle find ./trajectory-data \
  --source nested/source.json --session-id session-42
```

For unfamiliar data, use `pchronicle list ./trajectory-data --errors report`;
use `--errors strict` in automation when partial results should fail the job.

## 5. Choose how to continue

- [Discover and query a Dataset in depth](guides/discover-and-query.md)
- [Open the local Web UI](guides/ui.md)
- [Import or export Runs](guides/exchange.md)
- [Serve a Dataset locally](guides/serve.md)
- [Capture a new Run with pVisor](../pvisor/guides/capture.md)
- [Learn the Dataset and Source model](concepts/index.md)

The walkthrough Dataset is temporary. Use your own path before moving to
exchange, serving, or production automation.
