# pVisor benchmark

This benchmark measures the process-level cost users pay for a minimal host
Run and the cost of reading its durable Run Bundle through `status --json` and
`review --json`. Every sampled Run must finish successfully and produce a
completed, zero-exit-code bundle; a fast but incomplete Run is rejected.

Use the repository entry points:

```bash
just benchmark-pvisor
just benchmark-pvisor nightly target/pvisor-benchmark/nightly

just benchmark-pvisor-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

The `smoke` suite uses 2 warmups and 10 samples for pull-request feedback. The
`nightly` suite uses 10 warmups and 50 samples for a more stable distribution.
Both write the same `pvisor-benchmark/v1` raw schema and Markdown report.

Baseline and candidate measurements are meaningful only when collected on the
same host with the same suite. Comparison marks a metric as a regression when
it crosses the configurable threshold (15% by default); the report is
informational unless the runner is explicitly given `--fail-on-regression`.
