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

Use `--gateway-stream-markdown` for a live human-readable projection and
`--chronicle-mode lance` for canonical structured events. Dataset catalog,
query, analysis, import/export, and the read-only Web UI are provided by
[`pchronicle`](../../pchronicle/get-started.md).

Clients must use an injected proxy or base URL to be observed. Direct sockets
can bypass the explicit proxy unless the selected executor provides an enforced
network boundary; inspect the Run Bundle for the effective isolation level.

Next: [explore the captured history](../../pchronicle/get-started.md) or read the
[Gateway implementation](../design/gateway.md).
