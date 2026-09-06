# Capabilities and evidence

A capability is bounded authority over one resource and action. pVisor reasons
about capabilities by dimension because no single `safe` or `sandboxed` label
can describe an Agent environment accurately.

| Dimension | Example request | Evidence to inspect |
| --- | --- | --- |
| Filesystem read | read selected project and toolchain paths | visible roots and installed read controls |
| Filesystem write | write only to a staged workspace | write boundary and promotion decisions |
| Network | reach declared destinations | interception path and bypass resistance |
| Process | start bounded descendants | namespace/profile and inherited handles |
| Credentials | use one short-lived identity | delivery, expiry, and observed use |
| Tools and models | invoke declared endpoints | policy decision and routed calls |

Requested authority and installed enforcement are different facts. Admission
must reject a required capability dimension when the selected provider cannot
satisfy it. Optional controls may degrade only when the Run record reports that
degradation explicitly.

Evidence answers four progressively stronger questions:

1. **Declared** — what policy was requested?
2. **Mediated** — which actions passed through a control point?
3. **Enforced** — which bypass paths were blocked for the stated threat model?
4. **Attested** — is that enforcement bound to this exact Run and provider?

The Run Bundle is the place to inspect the answer for a concrete execution.
Return to [pVisor concepts](index.md), use the
[network guide](../guides/network.md) to configure one capability dimension,
or read [pVisor isolation design](../design/isolation.md) for platform
mechanisms. For the end-to-end trust chain across execution, orchestration,
and history, read
[Security and evidence](../../system-design/security-evidence.md).
