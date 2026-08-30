# Gateway Python SDK regression

**Starts the real Gateway, pChronicle Warehouse, and deterministic Echo
upstream, then exercises Gateway through pinned official Python SDKs.**

Owns this black-box scenario and its per-session comparison logs. Does not
compile Rust and does not own Gateway or pChronicle.

| Client | Client protocol | Upstream path |
|---|---|---|
| OpenAI | Chat Completions | OpenAI Chat Completions through wildcard forwarding |
| OpenAI | Responses streaming | OpenAI Chat Completions through protocol rewriting |
| Anthropic | Messages | Native Anthropic Messages with a Base64 Echo response |
| Google Gen AI | Gemini | Native `generateContent` |

## Run

```bash
cargo build --release --locked -p persisting-pchronicle-cli --bin pchronicle
tests/regression/gateway-echo/run.sh
```

`run.sh` is intentionally only the portable scenario entry point. Each server
binds port zero and publishes its selected loopback address. Configuration,
SDK calls, durable queries, cleanup, and assertions live in `regression.py`
and its focused Python modules; process lifecycle and readiness helpers are
shared with `gateway-fuzz` through `tests/regression/gateway_harness.py`.

By default it uses `target/release/pchronicle` for both `pchronicle echo` and
`pchronicle serve`. Set `PERSISTING_PCHRONICLE_BIN` to test another prebuilt
binary.

The SDKs are installed by `uv` in an isolated environment from
`requirements.txt`; they are not dependencies of the Persisting wheel.

The scenario creates these logs before comparing them:

- `logs/client-results.jsonl`: normalized results observed by each Python SDK.
- `logs/events.jsonl`: canonical events queried from the pChronicle dataset.
- `logs/comparison.jsonl`: the per-session join and every comparison result.
- `logs/python-clients.log`: Python SDK stdout and stderr.

Logs are retained automatically on failure. Set
`PERSISTING_KEEP_TEST_ARTIFACTS=1` to retain them after a successful run.

## Links

- [Regression tests](../README.md)
- [Gateway architecture](../../../docs/src/pvisor/design/gateway.md)
- [`persisting-gateway`](../../../crates/persisting-gateway/README.md)
