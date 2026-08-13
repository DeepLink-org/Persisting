# pChronicle example Datasets

Small deterministic Datasets used by the pChronicle CLI examples and tests.
Each child directory is an independent Dataset that can be passed directly to
`pchronicle ls`, `pchronicle status`, or `pchronicle query`. Its file can also
be used as the input to `pchronicle import`.

| Dataset | Exchange format | Contents |
|---|---|---|
| `atif/` | ATIF v1.7 | One support Trajectory with three Steps and one tool call |
| `openai-messages/` | OpenAI Messages JSON | Two compact training Runs |
| `actf/` | ACTF v1.0 | One code-repair attempt with two Steps |

For example:

```bash
pchronicle query examples/data/atif \
  "SELECT session_id, COUNT(*) AS steps FROM dataset.steps GROUP BY session_id"

pchronicle import --from examples/data/atif/support-ticket.json \
  --output /tmp/imported-support-ticket

pchronicle export --from /tmp/imported-support-ticket \
  --output /tmp/exported-support-ticket.json --format atif
```
