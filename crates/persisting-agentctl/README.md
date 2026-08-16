# Persisting AgentCtl

`persisting-agentctl` owns the runtime control state machine, the versioned
AgentCtl v1 protocol, and its small synchronous client SDK.

AgentCtl is an optional, cooperative channel between pVisor and a Run-local
runtime client. The control protocol has two requests: `Hello` authenticates
and opens a Session, while `Sync` exchanges the client's current state for
pVisor's current directive. It is not a sandbox, does not discover processes or
external effects, and is never enforcement evidence by itself.

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
  environment and drives Session creation, periodic state synchronization, and
  checkpoint quiescence.

An `Applied { effect: Deny }` state means the driver successfully blocked an
operation. It does not mean that a proxy-based driver is non-bypassable.

```rust
use persisting_agentctl::{AgentCtlClient, AgentCtlClientConfig, AgentState};

let Some(config) = AgentCtlClientConfig::from_current_environment("worker-1")? else {
    return Ok(()); // not running under pVisor
};
let mut client = AgentCtlClient::new(config);
let directive = client.connect()?;
let directive = client.sync(AgentState::Active)?;
# Ok::<(), anyhow::Error>(())
```

pVisor injects four Run-local variables:

- `PERSISTING_AGENTCTL_ENDPOINT`: Unix socket path;
- `PERSISTING_AGENTCTL_TOKEN`: bearer token for `Hello`;
- `PERSISTING_AGENTCTL_VERSION`: exactly `1`;
- `PERSISTING_AGENTCTL_TRANSPORT`: currently `unix`.

Clients report `active`, `idle`, or `quiesced { checkpoint_id }`. pVisor replies
with `continue`, `quiesce { checkpoint_id, deadline_unix_ms? }`, or
`shutdown { reason? }`. A checkpoint succeeds only after every Session frozen
into that checkpoint reports the matching quiesced state. See
[`src/protocol.rs`](src/protocol.rs) for the complete wire contract, JSON
examples, state semantics, typed errors, and safety boundary.

New integrations use only `PERSISTING_AGENTCTL_*`. A future interactive login
or terminal will use a separately authorized Debug protocol and Session;
terminal byte streams, PTY resize, and signals will not enlarge this compact
Control protocol.
