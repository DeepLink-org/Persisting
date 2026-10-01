# persisting-gateway

**pChronicle Agent protocol gateway: LLM HTTP forwarding and canonical
trajectory capture.**

Owns the application-level path from Agent/LLM HTTP exchanges to trajectory
events: protocol recognition and adaptation, upstream selection, run/session/
story/call correlation, canonical pChronicle event emission, WAL coordination,
and live human-readable projections.

Does not own the proxy data plane or the canonical trajectory storage format.
[`persisting-overlaynet`](../persisting-overlaynet/README.md) owns proxy
transport, access enforcement, and generic sink dispatch.
[`persisting-pchronicle`](../persisting-pchronicle/README.md) owns schemas,
persistence, reading, replay, conversion, and derived views.

Capture runs through `pchronicle serve --gateway-config`. Gateway is an internal
library; external execution components can also integrate it.

This crate implements `persisting-overlaynet::OverlaySink`. Protocol rendering
and capture share one in-memory `LlmRequestEventPayload` (`llm/v1`). Provider
wire formats are never chained through Chat Completions as an intermediate
protocol. Storyline is a derived trajectory view and is not part of the online
protocol-conversion path.

## Develop

```bash
just test persisting-gateway
# or: just test-crate capture
just test-capture-fixtures
just echo
```

`just echo` starts the loopback-only `pchronicle echo` upstream used by Gateway
benchmarks and regressions. It does not start Gateway itself.

## Links

- [Gateway forwarding, rewriting, and capture](../../docs/src/en/pchronicle/guides/serve-gateway.md)
- [`persisting-overlaynet`](../persisting-overlaynet/README.md)
- [`persisting-pchronicle`](../persisting-pchronicle/README.md)
