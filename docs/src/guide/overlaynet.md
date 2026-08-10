# Control network access with OverlayNet

OverlayNet lets pVisor apply allow, deny, and bandwidth rules to HTTP and
HTTPS traffic sent through its in-process proxy. It is useful for controlling
cooperative Agent tools without requiring a container or a VM.

!!! warning "Current security boundary"
    OverlayNet is currently an explicit, cooperative proxy. pVisor injects
    proxy environment variables into the child process, but a program can
    bypass the policy by removing those variables, using `NO_PROXY`, or opening
    a direct socket. Treat it as controlled egress for proxy-aware clients, not
    as a non-bypassable network sandbox.

## Allow only declared destinations

Pass one or more `--overlaynet-allow` options before the Agent command:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

The presence of an allow rule enables OverlayNet and changes the default
action to deny. In this example, intercepted traffic may reach the two listed
HTTPS destinations; other intercepted destinations are rejected.

## Choose a policy

The visible CLI options infer the proxy mode and default action:

| Goal | Option | Behavior for other intercepted destinations |
|---|---|---|
| Allow only selected targets | `--overlaynet-allow TARGET` | Denied |
| Block selected targets | `--overlaynet-deny TARGET` | Allowed |
| Block all proxy egress | `--overlaynet-deny-all` | Denied |
| Limit bandwidth | `--overlaynet-limit [TARGET=]RATE` | Unchanged |

Allow, deny, and limit options are repeatable. Explicit deny rules take
precedence over allow rules. `--overlaynet-deny-all` is a standalone policy and
cannot be combined with the other policy flags.

Targets accept an exact hostname, wildcard suffix, IP address, or CIDR, with
an optional port:

```bash
pvisor run \
  --overlaynet-allow '*.example.com:443' \
  --overlaynet-allow 203.0.113.10:443 \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command
```

### Deny all intercepted traffic

```bash
pvisor run --overlaynet-deny-all -- agent-command
```

This denies HTTP and HTTPS requests that reach the injected proxy. It does not
disable direct sockets or local Gateway routes.

`--overlaynet-deny-all` does not support allow exceptions. If the intended
policy is “deny by default and allow only a few destinations,” do not start
with deny-all; declare the allowed targets directly:

```bash
pvisor run \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-allow pypi.org:443 \
  -- agent-command
```

The presence of `--overlaynet-allow` automatically selects the allowlist
policy: matching destinations are allowed and all other intercepted
destinations are denied by default.

### Limit bandwidth

Apply a global limit and a stricter target-specific limit:

```bash
pvisor run \
  --overlaynet-limit 10mbps \
  --overlaynet-limit api.openai.com:443=2mbps \
  -- agent-command
```

Matching limits stack, and the strictest effective rate applies. Rates ending
in `kbps`, `mbps`, or `gbps` are bits per second; `kb/s`, `mb/s`, and `gb/s`
are bytes per second. A limit constrains traffic but does not grant access.

## Use structured rules

Use a TOML configuration when a rule needs multiple ports, transport matching,
or intentional access to a private address resolved from a hostname:

```toml
[run]
command = ["agent-command"]

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.example.com"
ports = [443]
transports = ["tcp_tunnel"]
allow_private_ips = false

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
host = "api.example.com"
port = 443
bytes_per_second = 250000
```

Run it with:

```bash
pvisor run --config run.toml
```

Supported transport values are `http`, `https`, and `tcp_tunnel`. Empty
`ports` or `transports` mean unrestricted for that dimension.

Hostname rules reject private and loopback DNS results by default. For an
intentional private service, prefer an explicit IP/CIDR rule; alternatively,
set `allow_private_ips = true` on a narrowly scoped hostname rule. Link-local
and other special-purpose ranges still require an explicit IP or CIDR rule.

## Understand which clients are controlled

pVisor injects `HTTP_PROXY`, `HTTPS_PROXY`, their lowercase forms, and
`ALL_PROXY` into the Agent process. HTTP clients that honor these settings are
routed through OverlayNet. The proxy handles ordinary HTTP forwarding and
HTTPS `CONNECT` tunnels.

The following paths are outside the current policy boundary:

- a client that ignores or removes the proxy environment;
- a destination added to `NO_PROXY`;
- a program that opens a direct socket;
- DNS and UDP traffic that does not pass through the HTTP proxy.

Consequently, a cooperative-proxy Run reports
`safety.network_non_bypassable = false`. When direct network access must be
blocked, use `pvisor run --safe --overlaynet-deny-all`: Linux adds a private
network namespace; macOS blocks IP and ambient host Unix sockets with Seatbelt,
retaining only Run-local IPC. Container Runs can instead
use `--container-network none`. Selective allow/deny rules remain cooperative
on both native host paths. The current KVM executor cannot use the host
OverlayNet endpoint, while the container executor requires
`--container-network host` when using the in-process proxy.

## Review the result

The current directory is the default reusable workspace. Each invocation keeps
an independent Run under `PERSISTING_RUN_HOME`:

```bash
pvisor run \
  --overlaynet-deny 169.254.0.0/16 \
  -- agent-command

pvisor review --json last | jq '{policy: .network.policy,
     interception: .network.interception,
     counters: .network.intercepted,
     non_bypassable: .safety.network_non_bypassable}'
```

The counters describe only requests that reached OverlayNet. They cannot count
traffic that bypassed the proxy.

## Troubleshooting

| Symptom | Check |
|---|---|
| An allowed hostname resolves to loopback or a private address | Use an explicit IP/CIDR rule, or a narrowly scoped structured rule with `allow_private_ips = true` |
| A request succeeds under `--overlaynet-deny-all` | Confirm the client honors the injected proxy and does not use `NO_PROXY` or a direct socket |
| pVisor cannot bind the proxy | Select a free non-zero address with `--overlaynet-listen 127.0.0.1:19082` |
| A container cannot reach the proxy | Use `--container-network host` |
| KVM configuration is rejected | Host OverlayNet is not yet exposed to the guest |

For an offline runnable walkthrough, use
[`examples/pvisor/03-network-isolation`](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation).
For LLM request capture and model routing, continue with the
[Capture guide](capture.md).
