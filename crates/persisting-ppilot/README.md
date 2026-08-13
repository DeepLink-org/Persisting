# pPilot

**Durable Run production at scale.**

pPilot plans, schedules, resumes, and reconciles many independent Runs. pVisor
owns each Run and its workspace; pChronicle owns trajectory Dataset discovery,
query, analysis, and exchange.

```bash
cargo build -p persisting-ppilot --bin ppilot

ppilot run plan.py --workers 8 --sink ./results
ppilot run plan.py --workers 8 --sink ./results \
  --control-uri s3://my-bucket/ppilot-control

ppilot produce production.py --output ./runs --parallelism 8 \
  --cluster-network-limit 10mbps
```

`run` executes a `plan()` / `execute(item)` workload with bounded concurrency,
checkpoint/resume, infrastructure retry, and a durable result journal.

`produce` consumes a Python planner (or compatibility JSON manifest), creates
one independent pVisor workspace per emitted Run, and writes a durable
production report. Both commands embed a job-scoped Supervisor; there is no
separate Supervisor service to deploy.

The CLI intentionally contains no Dataset catalog, query, conversion, or
analysis commands. Use `pchronicle` for those operations.
