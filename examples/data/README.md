# pChronicle example Datasets

**Small deterministic Datasets used by the pChronicle CLI examples and tests.**

Format directories (`atif/`, `actf/`, `openai-messages/`) are independent single-format
Datasets for `pchronicle serve` mounts and per-format import/query. `corpus/` is the
**flat** multi-format Dataset used by built-in analysis examples and tests (shallow
Directory discovery only registers loose JSON when the mount root has no child dirs).

| Dataset | Exchange format | Contents |
|---|---|---|
| `atif/` | ATIF v1.7 | One support Trajectory with three Steps and one tool call |
| `openai-messages/` | OpenAI Messages JSON | Two compact training Runs |
| `actf/` | ACTF v1.0 | One code-repair attempt with two Steps |
| `corpus/` | mixed (flat) | Same three Sources as above, one directory, no nesting |

## Use

```bash
pchronicle query examples/data/atif \
  --sql "SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id"

pchronicle stats overview examples/data/corpus

pchronicle import --from examples/data/atif/support-ticket.json \
  --to /tmp/imported-support-ticket

pchronicle export --from /tmp/imported-support-ticket \
  --to /tmp/exported-support-ticket.json --output-format atif

pchronicle serve \
  --listen 127.0.0.1:8080 --open \
  atif=examples/data/atif \
  openai=examples/data/openai-messages \
  actf=examples/data/actf
```

Each positional `NAME=DATASET` value becomes one Warehouse mount.

## Links

- [pChronicle examples](../pchronicle/README.md)
- [Import and export](../../docs/src/pchronicle/guides/exchange.md)
- [Local read-only Dataset server](../../docs/src/pchronicle/guides/serve.md)
