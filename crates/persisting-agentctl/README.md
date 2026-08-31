# persisting-agentctl

**Agent control contracts, policies, the versioned AgentCtl v1 protocol, and its
synchronous client SDK.**

Owns the runtime control state machine, the wire protocol, and
`AgentCtlClient`. AgentCtl is an optional, cooperative channel between pVisor
and a Run-local runtime client. It is not a sandbox, does not discover
processes or external effects, and is never enforcement evidence by itself.

pVisor owns the Run-scoped server and credential injection. OverlayNet and
Gateway apply policy decisions; they do not own this protocol.

```text
Requested -> Allowed / Denied -> Applied / Failed
```

- `ControlRequest` is a typed resource request (currently network or model).
- `ControlController` evaluates policy and returns the authorization transition.
- `ControlMachine` validates transitions and retains the state/history.
- `protocol` is the dependency-light request/response schema shared with
  pVisor's server.
- `AgentCtlClient` discovers the authenticated Unix endpoint from the
  environment and drives Session creation, periodic state synchronization, and
  checkpoint quiescence.

An `Applied { effect: Deny }` state means the driver successfully blocked an
operation. It does not mean that a proxy-based driver is non-bypassable.

The protocol has two requests: `Hello` authenticates and opens a Session;
`Sync` exchanges the client's current state for pVisor's current directive.
Clients report `active`, `idle`, or `quiesced { checkpoint_id }`. pVisor replies
with `continue`, `quiesce { checkpoint_id, deadline_unix_ms? }`, or
`shutdown { reason? }`. A checkpoint succeeds only after every Session frozen
into that checkpoint reports the matching quiesced state.

pVisor injects four Run-local variables: `PERSISTING_AGENTCTL_ENDPOINT`,
`PERSISTING_AGENTCTL_TOKEN`, `PERSISTING_AGENTCTL_VERSION` (exactly `1`), and
`PERSISTING_AGENTCTL_TRANSPORT` (currently `unix`). New integrations use only
`PERSISTING_AGENTCTL_*`. A future interactive login or terminal will use a
separately authorized Debug protocol; terminal byte streams, PTY resize, and
signals will not enlarge this compact Control protocol.

See [`src/protocol.rs`](src/protocol.rs) for the complete wire contract, JSON
examples, state semantics, typed errors, and safety boundary.

## Develop

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

```bash
just test persisting-agentctl
# or: just test-crate agentctl
```

## Links

- [pVisor isolation architecture](../../docs/src/pvisor/design/isolation.md)
- [OverlayNet architecture](../../docs/src/pvisor/design/overlaynet.md)
- [System architecture](../../docs/src/system-design/architecture.md)
- [`persisting-pvisor`](../persisting-pvisor/README.md)
