# pVisor

**pVisor is an AgentVisor: the control and containment layer between an
autonomous Agent and the infrastructure that executes it.**

Owns one Run, its Attempts, capability admission, staged filesystem Effects,
execution placement, and the host CLI (`pvisor`). It can place Runs on host,
container, and libkrun VM executors while preserving one Agent-facing contract.
It is not an Agent framework, an OCI runtime, or an operating system.

Does not own batch planning or global scheduling
([`persisting-ppilot`](../persisting-ppilot/README.md)), canonical trajectory
storage ([`persisting-pchronicle`](../persisting-pchronicle/README.md)), or the
implementation of every isolation backend. OverlayFS, OverlayNet, Gateway, and
AgentCtl are pVisor runtime drivers, not peer products.

![pVisor AgentVisor architecture](../../docs/src/assets/diagrams/pvisor/agentvisor-architecture.svg)

| Product area | Current responsibility |
| --- | --- |
| Agent lifecycle | One logical `Run`, one or more `Attempt`s, cancellation, deadlines, terminal publication, and parent lineage |
| Agent control | Optional authenticated AgentCtl v1 for Sessions, client state, directives, and cooperative quiescence |
| Capabilities | Models, tools, filesystem read/write, network, secrets, subprocess, and resources, with evidence recorded per dimension |
| Filesystem effects | Copy-on-write staging, classified review, logical checkpoint/fork, repeated selective apply, terminal apply/drop, and an apply ledger |
| Network and model access | Gateway capture plus OverlayNet policy; enforcement strength depends on executor and is never inferred from a product label |
| Execution placement | Host process, Docker/Podman transport, or libkrun VM using an OCI image, prepared rootfs, or Linux host rootfs |
| Evidence | Run Bundle, lifecycle events, capability enforcement, filesystem changes, network counters, AgentCtl observations, output, and artifact references |

The standalone product loop is `RunSpec → admission → Attempt → terminal
RunResult + private Run Bundle + staged Effects → later review/apply/drop`.
pChronicle is not a runtime prerequisite for that loop. Capture is a Gateway
capability, not pVisor's component identity.

The default build includes the local Lance/DataFusion pChronicle backend for an
optional durable Attempt-state handoff to pPilot. The default build excludes
cloud object-store SDKs, Jujutsu, `prost`, and a protobuf toolchain. Use
`lance-chronicle` for S3 support, `jujutsu-overlay` for the Jujutsu upper
backend, or `--no-default-features` for a storage-light binary.

## Develop

```bash
just pvisor          # release build + macOS Hypervisor signing
just pvisor debug    # debug build + macOS signing
just test persisting-pvisor
just test-pvisor-lance
just examples-pvisor
```

```bash
cargo build --locked -p persisting-pvisor --bin pvisor --release
```

On macOS, source builds that use HVF must be signed. `just pvisor` does this;
the equivalent entitlements file is `macos-hypervisor.entitlements`. Building
from source on macOS also requires Zig (`brew install zig`) to cross-compile
libkrun's embedded Linux guest init.

## Links

- [What is an AgentVisor?](../../docs/src/pvisor/concepts/agentvisor.md)
- [Get started](../../docs/src/pvisor/get-started.md)
- [Isolation architecture](../../docs/src/pvisor/design/isolation.md)
- [Gateway architecture](../../docs/src/pvisor/design/gateway.md)
- [OverlayNet architecture](../../docs/src/pvisor/design/overlaynet.md)
- [pVisor CLI](../../docs/src/pvisor/reference/cli.md)
- [System architecture](../../docs/src/system-design/architecture.md)
- [`persisting-overlayfs`](../persisting-overlayfs/README.md)
- [`persisting-overlaynet`](../persisting-overlaynet/README.md)
- [`persisting-gateway`](../persisting-gateway/README.md)
- [`persisting-agentctl`](../persisting-agentctl/README.md)
