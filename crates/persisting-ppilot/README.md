# pPilot

**Durable Run production at scale.**

pPilot plans, schedules, resumes, and reconciles many independent Runs. pVisor
owns each Run and its workspace; pChronicle owns durable leases, fencing,
Attempt state, trajectory storage, Dataset discovery, query, and exchange.

```bash
cargo build -p persisting-pvisor --bin pvisor
cargo build -p persisting-pchronicle-cli --bin pchronicle
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
production report. Both commands invoke the standalone `pvisor` binary for
each Run and embed a job-scoped Supervisor; there is no separate Supervisor
service to deploy. For `run --sink`, pPilot starts one authenticated,
loopback-only `pchronicle control` child and uses its versioned client protocol;
pPilot does not link pChronicle, Lance, Arrow, or DataFusion. Use
`--pchronicle-binary PATH` or `PERSISTING_PCHRONICLE_BIN` when `pchronicle` is
not installed beside `ppilot` or available on `PATH`. Similarly, use
`--pvisor-binary PATH` or `PERSISTING_PVISOR_BIN` for pVisor.
The default pVisor build does not link Lance/DataFusion. Durable Attempt and
trajectory writes use the same lightweight `pchronicle control` process
protocol; pPilot itself still does not link pVisor.

The CLI intentionally contains no Dataset catalog, query, conversion, or
analysis commands. Use `pchronicle` for those operations.
