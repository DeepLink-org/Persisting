# persisting-agent-abi

Small, synchronous client SDK for pVisor's low-frequency, Run-scoped Agent
ABI. A client discovers the authenticated Unix endpoint from the environment,
connects with an explicit role/name, heartbeats desired state, registers its
process, and brackets semantic external effects.

The ABI is cooperative: filesystem observation belongs to OverlayFS and model
traffic belongs to Gateway. Report only semantic effects for which the client
can provide a stable identity/digest and a real quiescence safe point.

```rust
use persisting_agent_abi::{AgentAbiClient, AgentAbiClientConfig};
use persisting_pvisor::AgentClientRole;

let Some(config) = AgentAbiClientConfig::from_current_environment(
    "worker-1",
    AgentClientRole::Agent,
    "my-agent",
)? else {
    return Ok(()); // not running under pVisor
};
let mut client = AgentAbiClient::new(config);
let welcome = client.connect()?;
# Ok::<(), anyhow::Error>(())
```

Wire frames are newline-delimited JSON, bounded by
`AGENT_ABI_MAX_FRAME_BYTES`. Consumers should use this SDK rather than relying
on an unversioned ad-hoc schema.
