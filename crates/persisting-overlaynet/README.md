# persisting-overlaynet

**Network interception and egress-policy data planes for trajectory capture.**

Owns the proxy data plane: request classification, HTTP `CONNECT`,
absolute-URI forwarding, access enforcement through
[`persisting-agentctl`](../persisting-agentctl/README.md), request accounting,
shared proxy header safety, and dispatch to one caller-supplied `OverlaySink`.

Does not own LLM protocol adaptation, upstream selection, session correlation,
capture events, WAL, or pChronicle writes.
[`persisting-gateway`](../persisting-gateway/README.md) implements `OverlaySink`
for those.

The explicit-proxy profile is deliberately marked `cooperative`: policy
decisions over intercepted requests are not non-bypassable enforcement.
`no-network` and `allowlist` mean "for traffic that reached this proxy";
direct sockets and clients that remove proxy variables remain ambient.

## Develop

```bash
just test persisting-overlaynet
```

## Links

- [`persisting-gateway`](../persisting-gateway/README.md)
- [`persisting-agentctl`](../persisting-agentctl/README.md)
