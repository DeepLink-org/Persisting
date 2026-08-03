# persisting-overlaynet

`persisting-overlaynet` is pVisor's lightweight proxy-based network overlay.

The current implementation intentionally covers only traffic that reaches an
explicit HTTP/HTTPS proxy:

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
matching limits apply.

`allowed_hosts` remains readable as a compatibility form with unrestricted
ports and transports. New policy should use `rules`. Gateway model upstreams
and the proxy listen address are operational configuration, not Agent egress
grants, and are never merged into this allowlist.

The crate owns the proxy data plane: request classification, `CONNECT`,
absolute-URI forwarding, access enforcement, request accounting, and dispatch
to a caller-supplied `OverlaySink`. Gateway is one sink; other sinks can
consume different protocols without changing OverlayNet.

It does not yet provide transparent socket interception, DNS/UDP mediation, a
TUN device, or a network namespace. The accepted Linux design for those —
an unprivileged network namespace with an in-process userspace stack as the
primary driver, and a seccomp user-notify fallback — is specified in
`docs/src/design/overlaynet.md`; both arrive as independent drivers without
changing the current proxy backend.

`no-network` and `allowlist` therefore mean "for traffic that reached this
proxy" today. Direct sockets and clients that remove proxy variables remain
ambient, and `InterceptionProfile::explicit_proxy().is_enforcing()` is always
false.

`persisting-gateway` implements `OverlaySink` and remains responsible for LLM
protocol adaptation, upstream selection, session correlation, capture events,
WAL, pChronicle writes, and live projections. A deployment configures one sink
per proxy server; a sink may compose and route to additional downstream sinks.
