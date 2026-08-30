# persisting-overlaynet

**Network interception and egress-policy data planes for pVisor.**

Owns the proxy data plane: request classification, HTTP `CONNECT`,
absolute-URI forwarding, access enforcement through
[`persisting-agentctl`](../persisting-agentctl/README.md), request accounting,
shared proxy header safety, and dispatch to one caller-supplied `OverlaySink`.

Also owns the libkrun VM driver: a non-bypassable virtio-net path whose
in-process smoltcp stack serves DHCP and synthetic DNS, then terminates and
re-originates policy-authorized IPv4 TCP. Gateway capture is reachable through
the guest's virtual router; Gateway and ordinary VM egress share the Attempt
controller, metrics, and bandwidth buckets.

Does not own LLM protocol adaptation, upstream selection, session correlation,
capture events, WAL, or pChronicle writes.
[`persisting-gateway`](../persisting-gateway/README.md) implements `OverlaySink`
for those. pVisor owns Run configuration, executor selection, and the recorded
interception profile.

The explicit-proxy profile is deliberately marked `cooperative`: policy
decisions over intercepted requests are not non-bypassable enforcement.
`no-network` and `allowlist` mean "for traffic that reached this proxy";
direct sockets and clients that remove proxy variables remain ambient.
`InterceptionProfile::vm_smoltcp()` records the VM's implemented non-bypassable
TCP/DNS surface. General UDP, IPv6, ICMP, QUIC, inbound forwarding, link-local,
and other reserved destinations fail closed in the MVP. Accepted Linux-host
netns and seccomp designs remain future independent drivers.

## Develop

```bash
just test persisting-overlaynet
```

## Links

- [OverlayNet architecture](../../docs/src/pvisor/design/overlaynet.md)
- [Network control](../../docs/src/pvisor/guides/network.md)
- [`persisting-gateway`](../persisting-gateway/README.md)
- [`persisting-agentctl`](../persisting-agentctl/README.md)
