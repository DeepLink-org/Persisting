# persisting-overlaynet

`persisting-overlaynet` contains pVisor's network interception and shared
egress-policy data planes.

Host and container execution use an explicit HTTP/HTTPS proxy:

- HTTP `CONNECT` tunnels;
- absolute-URI forwarding;
- egress allow/deny/allowlist decisions through `persisting-control`;
- shared proxy header safety rules;
- dispatch to one caller-supplied `OverlaySink` implementation.

The explicit proxy now also exposes a machine-readable interception profile
and lock-free request counters. Gateway publishes both from `/admin/status`,
and pVisor stores the profile in `run.json`. The profile is deliberately marked
`cooperative`, so policy decisions over intercepted requests cannot be
mistaken for non-bypassable enforcement.

Proxy hardening includes strict CONNECT authority parsing, establishing the
upstream connection before returning 200, streaming request/response bodies,
RFC `Connection` header filtering, and no implicit trust for arbitrary
loopback destinations. The Gateway forwarding client also disables automatic
redirects so every redirected destination returns through the policy gate. A
Run-local Gateway request uses its relative local route and does not require a
blanket loopback egress exception.

Allowlist policy supports exact hosts, `*.suffix` wildcards, IP literals, and
CIDRs, plus structured port and transport constraints. Authorization is split
into a pre-DNS check and a per-resolved-address check. Only authorized socket
addresses are retained and forwarded, so the connector cannot independently
re-resolve a hostname after policy approval. Hostname rules reject private and
loopback results unless `allow_private_ips` is explicitly enabled. Link-local,
multicast, reserved, and other special-purpose destinations still require an
explicit IP or CIDR rule.

Allow entries are valid only with `mode = "allowlist"`; configurations that
combine them with `public` or `no-network` are rejected instead of silently
ignoring the restriction. An injected control controller can further restrict
the compiled policy but cannot widen it, and is consulted both before DNS and
for every resolved address.

```toml
[network]
mode = "allowlist"

[[network.rules]]
host = "api.openai.com"
ports = [443]
transports = ["tcp_tunnel"]

[[network.rules]]
host = "10.20.0.0/16"
ports = [8080]
transports = ["http"]

[[network.deny_rules]]
host = "169.254.0.0/16"

[[network.limits]]
bytes_per_second = 1250000

[[network.limits]]
host = "api.openai.com"
port = 443
bytes_per_second = 250000
```

Explicit deny rules are evaluated first. Bandwidth limits are shared across
matching requests and connections and account for upload plus download; all
matching limits apply. CIDR bandwidth limits match both literal-IP targets and
the authorized addresses produced by hostname resolution.

`allowed_hosts` remains readable as a compatibility form with unrestricted
ports and transports. New policy should use `rules`. Gateway model upstreams
and the proxy listen address are operational configuration, not Agent egress
grants, and are never merged into this allowlist.

The crate owns the proxy data plane: request classification, `CONNECT`,
absolute-URI forwarding, access enforcement, request accounting, and dispatch
to a caller-supplied `OverlaySink`. Gateway is one sink; other sinks can
consume different protocols without changing OverlayNet.

libkrun VM execution uses a second, non-bypassable driver. Its virtio-net
UnixStream terminates in an in-process smoltcp stack that serves DHCP and
synthetic DNS, then terminates and re-originates policy-authorized IPv4 TCP.
Gateway capture is reachable through the guest's virtual router; Gateway and
ordinary VM egress share the Attempt controller, metrics, and bandwidth
buckets. A host DNS/TUN `198.18/15` fake IP is accepted only as an opaque
connector alias for an authorized hostname; guest literals in that range stay
blocked. The alias does not expose its final IP for CIDR policy. General UDP,
IPv6, ICMP, QUIC, inbound forwarding, link-local, and other reserved
destinations fail closed in the MVP. The accepted Linux-host netns and seccomp
designs remain future independent drivers and are specified in
`docs/src/design/overlaynet.md`.

For the explicit proxy, `no-network` and `allowlist` mean "for traffic that
reached this proxy"; direct sockets and clients that remove proxy variables
remain ambient, and `InterceptionProfile::explicit_proxy().is_enforcing()` is
always false. `InterceptionProfile::vm_smoltcp()` truthfully records the VM's
implemented non-bypassable TCP/DNS surface.

`persisting-gateway` implements `OverlaySink` and remains responsible for LLM
protocol adaptation, upstream selection, session correlation, capture events,
WAL, pChronicle writes, and live projections. A deployment configures one sink
per proxy server; a sink may compose and route to additional downstream sinks.
