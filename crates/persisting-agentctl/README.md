# Persisting AgentCtl

`persisting-agentctl` owns the runtime control state machine and policy-driven
state transitions shared by pVisor execution points. It also owns the
versioned Agent ABI wire contract and the small synchronous client SDK used by
Agents and pPilot.

```text
Requested -> Allowed / Denied -> Applied / Failed
```

- `ControlRequest` is a typed resource request (currently network or model).
- `ControlController` evaluates policy and returns the authorization transition.
- `ControlMachine` validates transitions and retains the state/history.
- pVisor runtime drivers such as OverlayNet and Gateway apply policy decisions.
- `abi` contains the dependency-light request/response schema shared with
  pVisor's server.
- `AgentCtlClient` discovers the authenticated Unix endpoint from the
  environment and drives heartbeats, process registration, checkpoint
  quiescence, and semantic effect boundaries.

An `Applied { effect: Deny }` state means the driver successfully blocked an
operation. It does not mean that a proxy-based driver is non-bypassable.

```rust
use persisting_agentctl::{AgentClientRole, AgentCtlClient, AgentCtlClientConfig};

let Some(config) = AgentCtlClientConfig::from_current_environment(
    "worker-1",
    AgentClientRole::Agent,
    "my-agent",
)? else {
    return Ok(()); // not running under pVisor
};
let mut client = AgentCtlClient::new(config);
let welcome = client.connect()?;
# Ok::<(), anyhow::Error>(())
```

The SDK name is AgentCtl; the newline-delimited JSON protocol remains the
versioned Agent ABI, and the existing `PERSISTING_AGENT_ABI_*` environment
contract remains stable.
