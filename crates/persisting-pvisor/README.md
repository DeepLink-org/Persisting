# pVisor

**Portable Agent Execution Runtime** — the Persisting component that owns one
Agent Run execution.

```text
RunSpec → PVisor::submit → RunHandle → RunResult
                         ├── status
                         ├── events
                         └── cancel
```

Version 1 includes:

- stable Run, Attempt, capability, checkpoint, result, and event value types in
  `persisting-proto`;
- model-call and network-access request/decision contracts;
- an injectable access controller used by `persisting-capture`;
- a local process executor;
- Run lifecycle status and events;
- cancellation and wall-clock deadlines;
- bounded stdout/stderr capture;
- an event sink interface for the canonical trajectory store.

The local process executor is a compatibility executor. It reports
`HostProcess` isolation and supports audit-mode capabilities only. It does not
claim filesystem, network, syscall, checkpoint, or migration enforcement.
Those guarantees require later container or WASM executors behind the same
`RunExecutor` contract.

Batch expansion and fleet scheduling are intentionally outside this crate and
remain responsibilities of `persisting-compute`.

## Capture integration

Capture remains the HTTP/streaming compatibility data plane. It resolves model
routes, adapts protocols, forwards bytes, and extracts trajectory events. Before
opening a general network connection or sending an LLM request, it presents an
identity-bearing request to pVisor's `AccessController`.

```text
Agent request
    │
    ▼
Capture: decode + route
    │
    ├── NetworkAccessRequest ──► pVisor policy ──► allow / deny
    └── ModelCallRequest     ──► pVisor policy ──► allow / deny
    │
    ▼ allow
Capture: forward + stream + record trajectory
```

The default controller preserves existing Capture configuration semantics.
`serve_with_runtime_control` allows a Run-aware controller to be injected
without replacing Capture's protocol implementation.
