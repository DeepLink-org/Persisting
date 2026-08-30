# pVisor benchmark

**Measures the process-level cost of a minimal host Run and of reading its
durable Run Bundle through `status --json` and `review --json`.**

Owns the smoke / nightly suites and the `pvisor-benchmark/v1` report schema.
Does not own Run lifecycle or isolation backends. Every sampled Run must finish
successfully and produce a completed, zero-exit-code bundle; a fast but
incomplete Run is rejected.

## Run

```bash
just benchmark-pvisor
just benchmark-pvisor nightly target/pvisor-benchmark/nightly

just benchmark-pvisor-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

`just benchmark-pvisor-compare` takes candidate then optional baseline. The
`smoke` suite uses 2 warmups and 10 samples for pull-request feedback. The
`nightly` suite uses 10 warmups and 50 samples for a more stable distribution.
Both write the same `pvisor-benchmark/v1` raw schema and Markdown report.

Baseline and candidate measurements are meaningful only when collected on the
same host with the same suite. Comparison marks a metric as a regression when
it crosses the configurable threshold (15% by default); the report is
informational unless the runner is explicitly given `--fail-on-regression`.

Unit-test the report contract without running the suite:

```bash
just test-pvisor-benchmark
```

## Links

- [pVisor design](../../docs/src/pvisor/design/index.md)
- [`persisting-pvisor`](../../crates/persisting-pvisor/README.md)
