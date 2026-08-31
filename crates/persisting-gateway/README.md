# persisting-gateway

**pVisor's built-in Agent protocol driver: LLM HTTP forwarding plus canonical
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

Capture remains the user-facing capability. It runs through `pvisor run` or
`pchronicle serve --gateway-config`. Gateway is an internal pVisor driver and a
reusable crate, not a peer product or standalone service.

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

- [Gateway architecture](../../docs/src/pvisor/design/gateway.md)
- [Capture trajectories](../../docs/src/pvisor/guides/capture.md)
- [Gateway forwarding, rewriting, and capture](../../docs/src/pchronicle/guides/serve-gateway.md)
- [`persisting-overlaynet`](../persisting-overlaynet/README.md)
- [`persisting-pchronicle`](../persisting-pchronicle/README.md)
