# OverlayNet transparent interception

> Status: accepted design, not yet implemented. Scope is Linux only. macOS and
> other hosts keep today's explicit-proxy, observe-grade behavior.

## Problem

OverlayNet's current data plane is an explicit HTTP/HTTPS proxy. pVisor injects
proxy environment variables and, for known Agent CLIs, proxy configuration
arguments. Coverage is therefore opt-in: any child process that ignores proxy
environment variables — a static Go binary, a raw socket, a subprocess that
scrubs its environment — talks to the network directly. This is why
`RuntimeCapabilities.network` reports `false` and the host `ProcessExecutor`
refuses `PolicyMode::Enforce` for network capabilities.

The goal of this design is **complete interception with a lightweight
footprint**: every byte the Agent process tree sends must pass through a
pVisor-owned choke point, regardless of language runtime, linkage, or syscall
discipline — without a VM, a root daemon, or persistent elevated privileges.

The key move is to relocate the interception point from *convention*
(environment variables the child may ignore) to *a layer the child cannot
choose to bypass*.

## Design A (primary): unprivileged network namespace + in-process userspace network stack

This is the default driver on capable Linux hosts. It mirrors the design of
pVisor's filesystem path:

```text
filesystem: pVisor embeds a FUSE server and IS the child's filesystem
network:    pVisor embeds a userspace TCP/IP stack and IS the child's network
```

### Mechanism

1. The Attempt child is spawned with `CLONE_NEWUSER | CLONE_NEWNET`. Creating
   a network namespace inside a fresh user namespace requires **no
   privileges**; the namespace owner holds `CAP_NET_ADMIN` within it.
2. Inside the namespace, setup code creates a `tun` device, assigns a
   link-local subnet, and installs a default route pointing at it. Loopback is
   brought up so Run-local services keep working.
3. The `tun` file descriptor is passed back to the pVisor parent over a
   `socketpair` before `exec`. From that point pVisor owns the only egress
   path of the entire process tree.
4. pVisor runs a `smoltcp`-based userspace stack on the `tun` fd. Inbound
   TCP flows terminate in the stack and are re-originated on the host side
   after passing the `persisting-control` policy gate. The existing OverlayNet
   proxy / Gateway sink remains the LLM capture path, unchanged.
5. DNS: the stack answers a virtual resolver address advertised via the
   namespace's `resolv.conf`. Queries are resolved host-side, giving a
   domain-level policy point *before* any connection exists.

### Properties

- **Topologically complete.** libc interposition, static binaries, raw
  syscalls, and forked grandchildren are all inside the namespace; there is no
  second path out. No cooperation from the child is needed or assumed.
- **Zero privilege at runtime.** No root, no setuid helper, no daemon. The
  only host prerequisite is unprivileged user namespaces
  (`kernel.unprivileged_userns_clone` / distro equivalent).
- **In-process.** Consistent with the embedded FUSE decision: pVisor does not
  spawn `passt`/`slirp4netns`-style helpers.

### Policy evaluation points

| Layer | Signal | Notes |
|---|---|---|
| DNS | queried name | virtual resolver; cheapest allowlist point |
| L4 | destination IP:port | last resort for literal-IP traffic |
| TLS | SNI from ClientHello | parsed passively, no MITM, no injected CA |
| QUIC | SNI from Initial packet, or blocked | default: refuse UDP/443 to force TCP fallback |

### Failure and probing

Driver availability is probed at Attempt prepare time. If user namespaces are
unavailable, behavior depends on the requested policy mode:

- `PolicyMode::Observe`: fall back to the explicit proxy driver and record the
  downgrade in the implant plan notes.
- `PolicyMode::Enforce`: fail the Run preparation. A downgrade under Enforce
  must never be silent.

## Design B (fallback): seccomp user-notify + ADDFD

For hosts where unprivileged user namespaces are disabled (hardened distros,
some container runtimes), a second driver achieves per-syscall completeness
without any namespace.

### Mechanism

1. The Attempt child installs a seccomp filter routing `connect`, `sendto`,
   and `sendmsg` (for unconnected sockets) to `SECCOMP_RET_USER_NOTIF`.
2. pVisor supervises the notification fd. For each intercepted call it reads
   and validates the socket address (with notification-cookie revalidation to
   close the TOCTOU window), then evaluates the same `persisting-control`
   policy.
3. Decisions:
   - **allow**: let the original syscall continue;
   - **deny**: return the policy errno (`ECONNREFUSED`/`EACCES`);
   - **redirect**: `SECCOMP_IOCTL_NOTIF_ADDFD` substitutes a pVisor-owned file
     descriptor already connected to the policy/capture proxy. The child
     observes a successful `connect`; no child memory is rewritten.

### Properties and caveats

- Covers static binaries and raw syscalls; `AF_UNIX` and loopback are passed
  through untouched.
- No namespace, no tun, no userspace stack — but per-datagram UDP coverage is
  fiddlier than Design A, and the supervisor sits on the syscall hot path.
- Chosen per Attempt; Design A remains preferred when both are available.

## Enforcement vs capture are separate layers

Transparent interception provides **enforcement** (deny / allowlist) and flow
accounting. It deliberately does not decrypt:

- Enforcement needs no MITM CA: DNS names, destination addresses, and
  passively parsed SNI are sufficient for host-level policy.
- **Capture** of LLM payloads stays on the existing explicit-proxy path:
  Gateway injects proxy configuration into known Agent CLIs and sees
  plaintext. Non-cooperating traffic cannot leave the allowlist but is not
  decrypted.

Known erosion: Encrypted ClientHello will eventually hide SNI. When that
matters, deployments choose between an opt-in MITM CA for capture-grade
visibility or falling back to DNS/IP-level enforcement. This is an industry
constraint, not specific to either driver.

## Capability reporting

`RuntimeCapabilities.network` becomes the result of the per-Attempt driver
probe rather than a constant:

- `enforce` when the netns or seccomp driver is active (Linux, capable host);
- `observe` when only the explicit proxy is available.

The honesty invariant is preserved: the host `ProcessExecutor` still never
claims network enforcement by itself; the claim is made by the active
OverlayNet driver, and `PolicyMode::Enforce` is satisfiable only while such a
driver is attached.

## Configuration

`[overlaynet].mode` grows two values next to the existing `off` / `proxy`:

```toml
[overlaynet]
mode = "auto"        # off | proxy | netns | seccomp | auto
policy = "allowlist"
allow = ["api.openai.com"]
# udp443 = "block"   # block (default) | allow
```

`auto` probes `netns → seccomp → proxy`, applying the downgrade rules from
Design A. `run.json` records the driver actually attached so `pvisor status`
can report the real enforcement level of a finished Run.

## Non-goals

- macOS transparent interception (Network Extension, pf-based UID routing).
  macOS remains observe-grade; the capability report says so.
- An eBPF (`cgroup/connect4`) driver. Elegant, but requires CAP_BPF/root and a
  setup-host deployment model; out of scope for now.
- TLS decryption by default. MITM stays an explicit opt-in, if ever.

## Phasing

1. netns driver: spawn plumbing, tun handoff, smoltcp TCP relay, DNS
   mediation, DNS/IP allowlist.
2. SNI-based policy; UDP/QUIC handling (default-block UDP/443).
3. seccomp user-notify driver as fallback.
4. Capability probing, `mode = "auto"` selection, `run.json` / `pvisor
   status` integration.
