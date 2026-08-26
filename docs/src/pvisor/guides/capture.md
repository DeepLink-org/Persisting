# Capture Agent Trajectories

Gateway capture is a pVisor Run driver. It is started and stopped with the Run;
there is no standalone Gateway command or daemon.

## Local walkthrough

```bash
cargo build --release -p persisting-pvisor --bin pvisor
cd examples/pvisor/04-gateway-llm-control
./run.sh
```

The example starts a loopback OpenAI-compatible model, executes its Agent with
`pvisor run`, and prints Gateway counters and the captured conversation.

## Run a real Agent

```bash
export DEEPSEEK_API_KEY=sk-...

pvisor run \
  --agent deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  --gateway-stream-markdown \
  -- claude
```

pVisor starts the embedded Gateway, injects proxy/base-URL values into the
child, waits for the child, flushes capture, and stops the Gateway. Each Run
writes an independent directory below `PERSISTING_RUN_HOME`.

Use `--gateway-stream-markdown` for a live human-readable projection. Select
`--record-format json --record-destination ./capture` for lightweight local
JSONL, or `--record-format lance --record-destination WAREHOUSE` for the full
pChronicle sidecar path. Dataset catalog, query,
analysis, import/export, and the read-only Web UI are provided by
[`pchronicle`](../../pchronicle/get-started.md).

### Event timestamps

Every newly persisted `EventRecord` carries both wall-clock fields:

- `timestamp`: an RFC3339 UTC timestamp;
- `timestamp_unix_ms`: the same observation time as Unix milliseconds.

Gateway timestamps request events when the request is accepted and response
events when the response is captured. The final Gateway capture sink also
backfills both fields for records from older producers, while pVisor runtime
events generate the pair together. The two values must agree within one
millisecond. Use `source + seq` as the ordering key; timestamps are for
wall-clock correlation and display, not ordering.

Clients must use an injected proxy or base URL to be observed. Direct sockets
can bypass the explicit proxy unless the selected executor provides an enforced
network boundary; inspect the Run Bundle for the effective isolation level.

Next: [explore the captured history](../../pchronicle/get-started.md) or read the
[Gateway implementation](../design/gateway.md).
