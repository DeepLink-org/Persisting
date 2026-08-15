# Large-scale regression tests

This directory contains repository-level black-box regressions. Each scenario
has its own directory and one executable `run.sh` entry point. Scenarios launch
prebuilt real component binaries, open loopback listeners, and inspect durable
output, so they are intentionally separate from the quick unit-test gate. They
do not compile Rust implicitly.

Run every scenario:

```bash
just regression
```

Run one scenario directly:

```bash
tests/regression/gateway-echo/run.sh
```

Directories containing `.long-running` are skipped by the aggregate runner.
Run the randomized Gateway fuzz scenarios explicitly with:

```bash
just gateway-fuzz
```

The Gateway suite is split into `formats`, `forwarding`, `storage`, and
`network-policy`; each has its own nested `run.sh` and `just gateway-fuzz-*`
entry. See `gateway-fuzz/README.md` for the contract matrix.

Set `PERSISTING_KEEP_TEST_ARTIFACTS=1` to retain a scenario's temporary files
for diagnosis. The `gateway-echo` scenario records Python SDK results,
pChronicle canonical events, and their per-session comparison as separate
JSONL logs.
