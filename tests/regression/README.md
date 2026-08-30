# Large-scale regression tests

**Repository-level black-box regressions against prebuilt real component
binaries.**

Each scenario has its own directory and one executable `run.sh` entry point.
Scenarios launch those binaries, open loopback listeners, and inspect durable
output, so they are intentionally separate from the quick unit-test gate. They
do not compile Rust implicitly.

Does not own Gateway, pChronicle, or SDK implementations. Directories containing
`.long-running` are skipped by the aggregate runner.

## Run

```bash
just regression
tests/regression/gateway-echo/run.sh
just gateway-fuzz
```

The Gateway fuzz suite is split into `formats`, `forwarding`, `storage`, and
`network-policy`; each has its own nested `run.sh` and `just gateway-fuzz-*`
entry. See [`gateway-fuzz/README.md`](gateway-fuzz/README.md) for the contract
matrix.

Set `PERSISTING_KEEP_TEST_ARTIFACTS=1` to retain a scenario's temporary files
for diagnosis. The `gateway-echo` scenario records Python SDK results,
pChronicle canonical events, and their per-session comparison as separate
JSONL logs.

## Links

- [`gateway-echo`](gateway-echo/README.md)
- [`gateway-fuzz`](gateway-fuzz/README.md)
- [Gateway architecture](../../docs/src/pvisor/design/gateway.md)
- [`persisting-gateway`](../../crates/persisting-gateway/README.md)
