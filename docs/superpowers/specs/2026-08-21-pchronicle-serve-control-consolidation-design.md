# pChronicle Serve and Control Consolidation Design

## Goal

Replace the standalone `pchronicle control` process with an optional Control
service hosted by `pchronicle serve`. pPilot and pVisor will start `serve` in
Control-only mode where they currently start `control`.

This is a process and CLI consolidation. The authenticated Control protocol,
Run lease semantics, Attempt registry, trajectory append behavior, and durable
storage layouts remain unchanged.

## CLI contract

`serve` accepts exactly one Dataset source:

- `--storage URI` creates a single Dataset mount named `default` and supplies
  the durable root required by Control;
- `--config FILE` loads the existing multi-Dataset Warehouse configuration.

`--storage` and `--config` are mutually exclusive. One of them is required.

The independently selectable services are:

- `--listen ADDR` enables the Warehouse HTTP API and Web UI;
- `--control ADDR` enables the authenticated Control TCP listener and requires
  `--storage`;
- `--gateway FILE` enables the Gateway using its existing TOML configuration.

At least one of `--listen`, `--control`, or `--gateway` is required. The
Warehouse listener no longer has an implicit default: omitting `--listen`
means that no Warehouse HTTP socket is created. `--open` requires `--listen`.
Warehouse and Control listeners must use loopback addresses.

The supported modes include:

```text
pchronicle serve --storage URI --control 127.0.0.1:0
pchronicle serve --storage URI --listen 127.0.0.1:8080
pchronicle serve --storage URI --listen 127.0.0.1:8080 --control 127.0.0.1:0
pchronicle serve --config warehouse.toml --listen 127.0.0.1:8080
pchronicle serve --storage URI --gateway gateway.toml --control 127.0.0.1:0
```

Gateway may run without Warehouse HTTP. In `--storage` mode its Dataset is the
automatic `default` mount. In `--config` mode its existing explicit/default
Dataset selection rules continue to apply.

The `pchronicle control` subcommand and its compatibility aliases are removed.

## Service architecture

The current Control command implementation becomes an embeddable Control
service with three phases:

1. open the Run control store and Attempt registry;
2. bind the configured TCP listener and generate a per-process authentication
   token;
3. serve authenticated, newline-delimited Control requests until shutdown.

`run_serve` becomes the supervisor for Warehouse, Control, and Gateway. It
resolves the Dataset source, prepares every enabled component, and binds every
requested listener before starting any public serving loop.

All enabled services share one cancellation token. A termination signal
cancels all services and performs their existing graceful shutdown work. If
any service returns unexpectedly or fails after readiness, the supervisor
cancels the remaining services and exits with the original error. A partially
initialized process never publishes readiness.

Control remains a separate loopback TCP listener rather than becoming a
Warehouse HTTP write route. This preserves the authentication and trust
boundary of the existing write-capable protocol while allowing one process to
own its lifecycle.

## Readiness protocol

After all enabled listeners have bound successfully, `serve` writes exactly
one newline-terminated JSON object to stdout:

```json
{
  "version": 1,
  "warehouse_endpoint": "127.0.0.1:8080",
  "control": {
    "endpoint": "127.0.0.1:49152",
    "auth_token": "generated-secret"
  },
  "gateway_endpoint": "127.0.0.1:8081",
  "gateway_admin_endpoint": "127.0.0.1:8082"
}
```

Members for disabled services are omitted. The ready-envelope version is
independent of the existing Control protocol version. Once readiness has been
written, `serve` writes no further data to stdout. Human-readable endpoints,
diagnostics, and runtime errors remain on stderr. The Control authentication
token is never included in stderr diagnostics.

Control-only process clients require the `control` member and reject a ready
envelope that omits it or uses an unsupported version.

## pPilot and pVisor migration

`ChronicleControlProcessClient` is replaced by
`ChronicleServeProcessClient`. It starts:

```text
pchronicle serve --storage <URI> --control 127.0.0.1:0
```

It parses the unified ready envelope, extracts the Control endpoint and token,
and then uses the existing `ChronicleControl` request/response implementation.
The `ChronicleControl` trait and in-memory implementation remain available;
only the executable process adapter changes.

All current launch sites in pPilot, pVisor, coordination, and trajectory CLI
flows migrate to the new process client. Existing binary-path and storage-root
configuration remains valid. Child-process failure and shutdown continue to
propagate through the process client.

This is an intentional breaking CLI and Rust API change: no deprecated
`pchronicle control` wrapper or `ChronicleControlProcessClient` type alias is
retained.

## Storage and visibility semantics

The consolidated process continues to use the existing storage structures:

- `run-control/` for CAS-managed Run leases and terminal commits;
- `attempt-registry/` for Attempt liveness and terminal results;
- the existing raw trajectory event destinations carried by append requests.

Local locking, object-store conditional updates, lease epochs, fencing,
idempotent terminal publication, and trajectory append acknowledgement do not
change. Existing local and object-store roots remain readable and writable
without migration.

Hosting Warehouse and Control in one process does not introduce implicit
catalog refreshes. Control writes become visible according to the Warehouse's
existing snapshot and explicit refresh behavior.

## Error handling and security

- Invalid service combinations fail during CLI validation.
- `--control` without `--storage`, `--open` without `--listen`, and a process
  with no enabled service are rejected.
- Non-loopback Warehouse or Control addresses are rejected before binding.
- Failure to open storage or bind any enabled listener prevents readiness.
- Unsupported ready-envelope and Control-protocol versions fail closed.
- Control retains its random per-process bearer token and maximum frame size.
- Warehouse remains read-only; no Control operations are added to its HTTP
  router.
- Unexpected service termination shuts down sibling services rather than
  leaving a partially functional process alive.

## Testing

Tests cover:

- Clap help and validation for all valid and invalid service combinations;
- the absence of a Warehouse socket when `--listen` is omitted;
- Warehouse-only, Control-only, Gateway-only, and combined service startup;
- `--storage` creating the automatic `default` Dataset mount;
- readiness being emitted once and only after every listener has bound;
- omission of disabled endpoints and omission of the Control token from
  stderr;
- unified shutdown on Ctrl-C and unexpected component failure;
- lease acquire/renew/takeover, fencing, Attempt heartbeat/terminal state,
  Run commit, and trajectory append through a spawned `serve` process;
- pPilot and pVisor process-launch integration;
- continued access to existing local and object-store Control state;
- existing Warehouse, Gateway, and read-only HTTP contract regressions.

## Out of scope

- exposing Control as Warehouse HTTP write endpoints;
- remote or non-loopback Control access;
- automatic Warehouse catalog refresh after Control writes;
- combining `--config` and `--storage`;
- retaining a hidden or deprecated `pchronicle control` command;
- changing Run, Attempt, trajectory, Gateway, or Dataset storage formats.
