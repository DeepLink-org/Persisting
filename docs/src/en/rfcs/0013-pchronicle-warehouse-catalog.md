# RFC-0013: pChronicle path Directory

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-09-18 |
| **Component** | pChronicle CLI, `pchronicle serve`, pChronicle Web |
| **Related** | [RFC-0003 Ownership](0003-pchronicle-ownership.md) · [Warehouse guide](../pchronicle/guides/serve.md) · [CLI reference](../pchronicle/reference/cli.md) · [Architecture](../pchronicle/design/architecture.md) |

---

## Summary

This RFC defines one way to open a **path** in a pChronicle platform deployment: a **Directory** (name → path + ACL + ticket exchange).

Dataset identity is always a path (local path or `s3://` / `az://` / `gs://` URI). Directory is not a third Dataset kind and does not replace Snapshot. It only decides which paths a caller may resolve; the ticket `uri` is what the engine opens.

CLI flags, config files, and HTTP paths keep the word `catalog` for compatibility (`--catalog-config`, `catalog.toml`, `catalog://`, `/api/v1/catalog/datasets`). Product and RFC language use Directory. The config is TOML (extension may be `.toml` / `.yml`, etc.; content is still parsed as TOML).

The normative implementation hangs off `pchronicle serve --catalog-config`. There is no separate `catalog serve` process.
The listener defaults to loopback and MAY bind non-loopback addresses; deployers MUST own the network boundary.

- **Serve**: the parent only authenticates, serves the directory/tickets, and spawns workers (front-only). It MUST NOT open `[datasets.*]` in-process. Authorized data-plane requests open mounts in a one-shot `--catalog-query-worker`.
- **Public browse**: datasets granted with `user = "*"` are visible anonymously; the parent MAY cache browse projections for them without backend keys.
- **CLI config**: `pchronicle serve catalog dataset add|remove|list` rewrites datasets; `issue|grant|revoke` rewrites users and grants. Users/grants hot-reload; Dataset URI and backend credential changes REQUIRE a restart.
- **Directory tickets**: `@team` resolves as `catalog://…`; `@team/prod` opens the ticket path after exchange. `/api/v1/catalog/datasets` filters by user keys (no headers → public libraries only).

```text
pchronicle serve catalog dataset add --catalog-config catalog.toml prod --uri s3://bucket/prod \
  --access-key BACKEND_AK --secret-key BACKEND_SK
pchronicle serve catalog issue --catalog-config catalog.toml alice
pchronicle serve catalog grant --catalog-config catalog.toml alice prod
pchronicle serve --catalog-config catalog.toml --listen 127.0.0.1:8081
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
pchronicle query @team/prod 'SELECT 1'
```

## Motivation

Local paths and static Warehouse mounts assume the operator can already see every Dataset. Sharing several object-store evaluation libraries with a group leaves three gaps:

1. **Discovery and authorization are mixed**. Users need a directory of library names they may open, not every bucket URI in each laptop `config.toml`.
2. **Backend keys must not live in user config**. Object-store ak/sk belong to the storage account; user keys only authenticate to Directory. Writing backend keys into local dataset pins spreads them and cannot trim visibility per person.
3. **Web and CLI data planes differ**. After a ticket exchange, the CLI can open `s3://` itself. Web queries run inside serve; if the parent loads every library's backend keys and runs SQL, one auth bypass sees unauthorized libraries.

This RFC defines Directory as **directory + ACL + ticket exchange**, leaves storage access to existing `open(path)`, and isolates the Web data plane in a one-shot worker.

## Goals and non-goals

### Goals

- Describe `meta`, `users`, `datasets`, and `[[grants]]` in one Directory config.
- Issue user keys and rewrite ACL via CLI: `pchronicle serve catalog issue|grant|revoke` without starting HTTP.
- Resolve `@name/library` to one path (the ticket `uri`); the engine then opens only that path.
- After exchange, the CLI talks to storage itself; backend keys appear only in tickets and worker stdin, never in the user's `config.toml`.
- Web exchanges user keys for an authorized mount set; datasets with `user = "*"` MAY be listed/browsed anonymously.
- Hot-reload users and grants about every 3 seconds; reject hot-reload of Dataset definitions and backend credentials (restart required).
- Allow Warehouse to bind any listen address; examples stay on loopback. Catalog headers are not a public auth boundary; exposure on untrusted networks is the deployer's responsibility.

### Non-goals

- STS, short-lived credential rotation, or mapping user keys to AWS sessions.
- Hot-reloading Dataset URI / endpoint / region / backend ak/sk (restart serve).
- HTTP mint APIs on a running Warehouse.
- A separate `catalog serve` binary.
- `fork(2)` of a running Tokio runtime (undefined behavior).
- Writing backend object-store keys into local dataset pin config.
- Enforcing fine-grained `permissions` in v1 (the field is writable; semantics remain library membership).
- Changing Snapshot protocol, SQL schema, or Gateway/Control protocols.

Directory in this RFC is not the same object as **Snapshot** after a path is opened (see [Snapshot design](../pchronicle/design/catalog.md)). Directory lists authorized paths; Snapshot pins Source membership and versions on an opened path.

## Roles and trust boundary

| Role | Holds | Use |
|---|---|---|
| Storage account | Backend `access_key` / `secret_key`, optional endpoint, region | Open `s3://` libraries |
| Directory user | User `access_key` / `secret_key` | List/fetch tickets for granted libraries |
| Local CLI | User keys (in dataset pin config) | After exchange, inject backend keys into process env and open the ticket path |
| Browser | User keys (`localStorage`) | Send as request headers to loopback serve |
| serve parent | Full Directory config | Authenticate, return tickets, spawn worker; do not write backend keys into AWS env |
| query worker | Tickets for that user's libraries | One-shot Warehouse data-plane request |

ACL is a **discovery and authorization** boundary, not mandatory object-store isolation. Callers who hold backend keys or can guess URIs may still bypass Directory. Directory does not replace bucket policy.

## Process model

Directory hangs on the existing Warehouse listener. Without `--catalog-config`, `pchronicle serve` is unchanged: static mounts, no user auth.

```text
browser / CLI
  → Warehouse listener
       ├─ GET /health
       ├─ GET /api/v1/catalog/datasets[/{name}]   parent: auth + directory/ticket
       ├─ static UI
       └─ other /api/*                            parent mounts / or spawn worker
              → pchronicle serve --catalog-query-worker
                    stdin:  mounts + HTTP request
                    stdout: status / content-type / body
                    exit
```

Constraints:

1. The listener MAY bind non-loopback. This RFC does not treat catalog headers as a public auth boundary; deployers MUST add a boundary on untrusted networks.
2. The parent MUST NOT open datasets from the config. It uses a front-only Warehouse with empty mounts; public browse caches hold paths only and MUST NOT write backend keys into parent `AWS_*`.
3. Workers MUST be started with `Command`, MUST NOT `fork(2)` a running Tokio runtime.
4. Workers MUST NOT listen, MUST NOT read the Directory config, MUST NOT read user keys. They only consume filtered mounts and the raw request from stdin.
5. Workers inherit parent env (certs, `PATH`, …), but the parent MUST NOT pre-write catalog backend keys into `AWS_*`. The worker sets backend env from the user's tickets before opening storage.
6. Each `[datasets.*]` MAY carry its own endpoint, region, and backend ak/sk. One worker process can hold only one process-global AWS env; if a user is granted incompatible `s3://` backends, the request MUST include `dataset=` to select one, or MUST fail.
7. The hidden flag `--catalog-query-worker` MUST NOT appear in user-facing `serve --help`.

On worker timeout the parent MUST return `unavailable` and MUST NOT log keys from stdin.

## Configuration

The Directory config only manages users, datasets, and grants. It is the single source of truth; runtime serve options still come from `pchronicle serve` flags. When the file is missing, catalog management commands create an empty catalog (with `[meta]`).

Authoritative schema (matches current implementation / deployment samples):

```toml
[meta]
version = 1
revision = 1
name = "default"

[users.alice]
access_key = "pcak_…"
secret_key = "…"

[datasets.default]
uri = "/data/warehouse"

[datasets.prod]
uri = "s3://prod/"
endpoint = "http://s3-a.example:8060"
region = "us-east-1"
access_key = "BACKEND_AK_A"
secret_key = "BACKEND_SK_A"

[datasets.prod2]
uri = "s3://prod"
endpoint = "http://s3-b.example:8060"
region = "us-east-1"
access_key = "BACKEND_AK_B"
secret_key = "BACKEND_SK_B"

[[grants]]
user = "*"
dataset = "prod"

[[grants]]
user = "*"
dataset = "prod2"

[[grants]]
user = "alice"
dataset = "default"
# permissions optional; v1 ignores fine-grained semantics (membership only)
# permissions = ["read", "query", "analyze"]
```

Rules:

- `meta.version` MUST be a supported config version; successful CLI writes SHOULD maintain `meta.revision` / `meta.name` (optional).
- User and Dataset names MUST be lowercase `[A-Za-z_][A-Za-z0-9_]*`.
- `[users.*]` contains only `access_key` / `secret_key`; `access_key` MUST be globally unique; v1 allows plaintext `secret_key`.
- `[datasets.*]` MUST include `uri`; local paths MUST NOT set backend keys; `s3://` MUST set both `access_key` and `secret_key`, and MAY set `endpoint` / `region`.
- Different datasets MAY use different endpoint / region / backend keys (see process model item 6).
- `[[grants]]` MUST include `user` and `dataset`; `permissions` is optional and not enforced in v1.
- `grants.user = "*"` marks the dataset public (anonymous list/browse) and expands to **all current** users at parse time; newly issued users inherit it after hot-reload.
- Named `grants.user` MUST reference an existing user; `grants.dataset` MUST reference an existing dataset.
- Duplicate grants for the same user and dataset MUST be rejected (including after `*` expansion).
- Config size MUST be bounded; parse/validation failure refuses serve start; hot-reload failure MUST keep the last valid ACL.
- TOML is authoritative; future SQLite/Postgres may only be indexes or derived projections.

## CLI management

Catalog management commands only edit the config file and do not start an HTTP listener. Missing files create the parent directory and an empty config.

```text
pchronicle serve catalog dataset add    --catalog-config FILE NAME --uri URI [OPTIONS]
pchronicle serve catalog dataset remove --catalog-config FILE NAME...
pchronicle serve catalog dataset list   --catalog-config FILE

pchronicle serve catalog issue  --catalog-config FILE NAME
pchronicle serve catalog grant  --catalog-config FILE NAME DATASET...
pchronicle serve catalog revoke --catalog-config FILE NAME DATASET...
```

`dataset add` only registers a Dataset; it does not create or delete backend data. When appending `s3://` via CLI, if other `s3://` entries already exist, the new endpoint / region / backend keys MUST match them exactly (hand-written multi-backend configs remain valid, but workers must select per item 6). `issue` generates user AK/SK (secret printed once on stdout) and MUST NOT write any grant. `grant` / `revoke` edit `[[grants]]`; NAME `*` writes/removes **named** grants for every **current** user (it does not write a `user = "*"` public row). All writes MUST replace the file atomically and keep the previous file on failure.

## HTTP

Directory routes share Warehouse `/api` and `/api/v1` prefixes. Auth headers:

| Header | Meaning |
|---|---|
| `x-pchronicle-access-key` | User access key |
| `x-pchronicle-secret-key` | User secret key |

Missing, blank, or mismatched keys MUST return `401`, and MUST NOT distinguish “unknown user” from “bad secret”.
With no catalog headers at all, list MAY return only datasets granted to `user = "*"`; partial headers still MUST `401`.

Unauthorized and unknown library names MUST both return `404`.

| Route | Parent | Response |
|---|---|---|
| `GET /api/v1/catalog/datasets` | yes | Libraries visible to the auth user, or public libraries when anonymous: `name`, `uri`, optional `endpoint`/`region`; **no** backend keys |
| `GET /api/v1/catalog/datasets/{name}` | yes | Full ticket with backend `access_key` / `secret_key` when authenticated and authorized; anonymous MUST `401` (public libraries allow key-free list/browse only) |
| `GET /api/health` | yes | No auth |
| Static UI | yes | No auth |
| Other `/api/*` (including `GET /api/catalog` Snapshot) | no, forward to worker | Authenticate, then run with the user's mounts; multi-backend requires `dataset=` |

Error JSON keeps Warehouse `code`, `message`, `request_id`. Logs MAY include user segment, library name, and `request_id`; MUST NOT print user or backend keys.

`GET /api/v1/catalog/datasets/{name}` is the CLI ticket exchange. Clients then open `uri` (the Dataset path) directly and do not proxy queries back through Directory.

## CLI dataset pin

`catalog://` is a pin **type**, not a storage URI `DatasetLocation` can open. After a successful exchange, Dataset identity is the ticket path, not `catalog://…` itself.

```bash
pchronicle dataset pin team catalog://127.0.0.1:8081 --ak USER_AK --sk USER_SK
```

Normalization:

- scheme MUST be `catalog`;
- host MUST be a loopback IP (e.g. `127.0.0.1`) with a port;
- MUST NOT include userinfo, path, query, or fragment;
- MUST NOT accept `--endpoint` / `--region` (those come from the ticket, not the pin).

Resolution dispatches on pin **type**, not path join for every `@name/suffix`:

| Reference | catalog pin | ordinary URI pin |
|---|---|---|
| `@team` / `@team/` | `ls` libraries the user may access | resolve to pin root URI |
| `@team/prod` | fetch ticket for library `prod`, open ticket path | join `prod` onto root URI |
| `@team/prod/more` | fetch `prod`, then join `more` onto ticket path | join `prod/more` onto root URI |

User `--ak/--sk` live in the local dataset pin credential table, isolated like S3 pins: not shown in `dataset list` / `dataset show` URIs. Backend keys MUST NOT be written there.

Tickets cache in the CLI process (`thread_local`), keyed by catalog URL, user access key, and library name. Long-lived `serve` does not use this CLI cache; Web re-authenticates each request. Cache is dropped on process exit.

## Web

Settings (left **Keys**) store catalog user keys in `localStorage`:

- `pchronicle.catalog.access_key`
- `pchronicle.catalog.secret_key`

The browser attaches these as the HTTP headers above on `/api/` requests **to the current pChronicle serve**. This is the opposite of Assistant Browser BYOK: Assistant keys go only to model endpoints; catalog keys must reach serve for auth.

Without user keys, Web MAY still browse `user = "*"` public libraries; authenticated data-plane requests MUST `401`. Ordinary serve without `--catalog-config` does not require these headers.

Queries run in the worker. The browser does not hold backend object-store keys.

## Data-plane isolation

In the data-plane middleware the parent:

1. Validates user keys;
2. Filters that user's library tickets;
3. Writes HTTP method, path, query, body, and mounts as a JSON job;
4. Spawns the same binary as `serve --catalog-query-worker`;
5. Reconstructs the HTTP response from the stdout envelope.

The worker builds `ChronicleServerConfig` mounts from tickets, runs the same read-only Warehouse routes, then exits.

Unauthorized library tickets MUST NOT enter the job. An empty grant set MUST surface as `404`, not an empty Warehouse.

## Rejected alternatives

### Warehouse HTTP mint

Rejected. Catalog headers are not a public auth boundary; unauthenticated mint on loopback would hand user keys to any local process that can hit the port. Issuance is CLI rewriting of the Directory config.

### Separate `catalog serve` process

Rejected. A second listener, port, and lifecycle would fork Warehouse docs. Directory traffic is small and belongs on existing `pchronicle serve`.

### Parent opens every dataset then filters SQL per user

Rejected. Once DataFusion and object-store clients hold all backend keys and mounts, a filter bug is a privilege escalation. Web queries MUST run in a process that only has authorized mounts.

### `fork(2)` a running Tokio “to drop privilege”

Rejected. Fork on a multi-threaded runtime is undefined behavior. Use a new `Command` process.

### STS / short-lived session tickets

Rejected. The target is a local collaboration directory, not cloud identity federation. Passing backend keys to authorized clients is simpler and matches existing S3 pin injection into `AWS_*`.

### Treat catalog as ordinary path-join pins

Rejected. `@prod/evals` on `s3://bucket` is path join; on a Directory locator it is “name + library name”, then open the ticket path. Mixing would produce the illegal URI `catalog://127.0.0.1:8081/prod`.

### Non-loopback bind + catalog headers as public auth

Rejected. Warehouse remains a local inspection surface. Binding `0.0.0.0` needs separate auth, TLS, and multi-tenant threat modeling beyond this RFC.

## Compatibility and evolution

- Without `--catalog-config`, existing Dataset references, ordinary pin `@name/suffix` path joins, and unauthenticated loopback Warehouse MUST stay unchanged.
- `catalog://` MUST NOT become an openable `DatasetLocation` storage scheme; only the dataset pin resolver understands it.
- Authoritative config keys are `meta` / `users` / `datasets` / `grants`; legacy `[libraries.*]` or grants embedded in `users.*.datasets` MUST NOT remain normative.
- New Dataset fields, auth headers, or worker protocols are breaking and require revising this RFC.
- Future STS or Dataset hot-reload may be follow-on RFCs and MUST NOT silently change “pass through backend keys / Dataset changes require restart”.

This RFC corrects architecture language that said loopback Warehouse had no authentication at all: with `--catalog-config`, the data plane and Directory routes use user-key headers, except for public libraries. It is still not a public multi-tenant service.

## Implementation status

Current implementation covers the core of this RFC:

- TOML Directory parse and startup validation (`meta` / `users` / `datasets` / `[[grants]]`, including `user = "*"`);
- `pchronicle serve catalog issue|grant|revoke|dataset …` config editors (issue grants nothing; sk printed once on stdout);
- ~3s hot-reload of users/grants; Dataset / backend credential changes rejected with the previous ACL kept;
- `GET /api/v1/catalog/datasets` and `/{name}` (including anonymous public list);
- `--catalog-config` front-only parent and `--catalog-query-worker`; multi-backend narrowed by `dataset=`;
- `catalog://` pins, `@team/prod` ticket exchange, in-process ticket cache;
- Web `localStorage` user keys and data-plane headers.

Follow-ups:

1. Integration tests that cover real worker subprocesses (unauthorized library keys must not appear in the environment);
2. Evaluate explicit audit fields for local-path libraries comparable to S3;
3. `issue --rotate` for existing user keys (duplicate-name issue currently rejects);
4. Whether CLI `dataset add` should formally write multiple S3 backends (hand-written multi-backend is valid today; CLI append still requires matching existing s3 backend identity).
