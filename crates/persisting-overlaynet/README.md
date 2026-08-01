# persisting-overlaynet

`persisting-overlaynet` is pVisor's lightweight proxy-based network overlay.

The current implementation intentionally covers only traffic that reaches an
explicit HTTP/HTTPS proxy:

- HTTP `CONNECT` tunnels;
- absolute-URI forwarding;
- egress allow/deny/allowlist decisions through `persisting-control`;
- shared proxy header safety rules;
- dispatch to one caller-supplied `OverlaySink` implementation.

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

`persisting-gateway` implements `OverlaySink` and remains responsible for LLM
protocol adaptation, upstream selection, session correlation, capture events,
WAL, pChronicle writes, and live projections. A deployment configures one sink
per proxy server; a sink may compose and route to additional downstream sinks.
