# Persisting AgentCtl

`persisting-agentctl` owns the runtime control state machine, the versioned
AgentCtl protocol, and its small synchronous client SDK.

AgentCtl is an optional, cooperative channel between pVisor and a Run-local
Agent. It carries heartbeats, desired state, process declarations, quiescence
acknowledgements, and Agent-declared open operations. It is not a sandbox,
does not discover unregistered processes or effects, and is never enforcement
evidence by itself.

```text
Requested -> Allowed / Denied -> Applied / Failed
```

- `ControlRequest` is a typed resource request (currently network or model).
- `ControlController` evaluates policy and returns the authorization transition.
- `ControlMachine` validates transitions and retains the state/history.
- pVisor runtime drivers such as OverlayNet and Gateway apply policy decisions.
- `protocol` contains the dependency-light request/response schema shared with
  pVisor's server.
- `AgentCtlClient` discovers the authenticated Unix endpoint from the
  environment and drives heartbeats, process registration, checkpoint
  quiescence, and cooperative operation declarations.

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

The SDK and protocol are both named AgentCtl. New integrations use
`PERSISTING_AGENTCTL_*`. The former `PERSISTING_AGENT_ABI_*` names remain
accepted temporarily as migration aliases for wire-v2 clients.
