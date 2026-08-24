# Gateway forwarding, rewriting, and capture for `pchronicle serve`

`pchronicle serve` can run a local LLM Gateway with or without the read-only Dataset server.
For each request, the Gateway selects an upstream, can rewrite the model and
wire protocol, returns a response in the client's protocol, and appends
canonical capture events to one mounted Dataset. The Dataset Web UI and API
remain read-only.

Use this mode when an Agent or SDK already knows how to call an OpenAI-,
Anthropic-, or Gemini-compatible base URL and you want to capture that traffic
without starting a pVisor Run. Use [pVisor capture](../../pvisor/guides/capture.md)
instead when the Gateway must share the lifecycle and isolation boundary of an
Agent execution.

## Configuration inputs

Gateway mode deliberately keeps Dataset selection and forwarding configuration separate:

| Input | Owns |
| --- | --- |
| Positional `[NAME=]DATASET` values | Mounted Datasets |
| `gateway.toml` passed to `--gateway-config` | Gateway listeners, model routes, credentials, capture level, and network policy |
| CLI flags | Capture Dataset selection, local Gateway state, live Markdown, and foreground debugging |

Unknown Gateway TOML fields are rejected. The Gateway configuration must use
TOML; other file extensions are not accepted.

## Minimal configuration

Create `gateway.toml`:

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
agent_id = "local-agent"
capture_level = "dialogue"

[[models]]
name = "deepseek-chat"
provider = "openai"
upstream = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[[models]]
name = "*"
forward = "deepseek-chat"
```

Export the credential and start both services:

```bash
export DEEPSEEK_API_KEY=sk-...

pchronicle serve \
  --listen 127.0.0.1:8080 \
  --gateway-config gateway.toml \
  --gateway-dataset captures \
  --gateway-stream-markdown \
  captures=./data/captures
```

This starts three loopback listeners:

- `127.0.0.1:8080` — Dataset Web UI and read API;
- `127.0.0.1:8787` — LLM Gateway;
- `127.0.0.1:8788` — Gateway status and session API.

Omit `--listen` to run only the Gateway listeners and capture sink; no
Dataset HTTP endpoint is created.

Point the Agent or SDK at `http://127.0.0.1:8787/v1`. For example:

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-persisting-session-id: example-session' \
  -d '{
    "model": "deepseek-chat",
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

Inspect the Gateway independently of the Dataset server:

```bash
curl http://127.0.0.1:8788/admin/status
curl http://127.0.0.1:8788/admin/sessions
```

## Request lifecycle

The Gateway performs forwarding, rewriting, and capture as one request
pipeline:

```text
client request
  -> detect client protocol and model
  -> select the first matching models[] route
  -> authorize the configured model route
  -> optionally rewrite the model and protocol
  -> construct the upstream URL and credentials
  -> forward to the upstream
  -> optionally translate the response to the client protocol
  -> record the request and completed or streaming response
```

Capture metadata distinguishes the model requested by the client from the
model sent upstream and records whether a model rewrite occurred. Capture does
not replace forwarding: an upstream error is returned to the client and also
closes the corresponding capture call.

## Top-level Gateway fields

| Field | Required | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | Yes | — | LLM proxy listener. Embedded mode accepts loopback addresses only. Port `0` selects an available port. |
| `admin_listen` | No | `127.0.0.1:9876` | Listener for `/admin/status` and `/admin/sessions`; also loopback-only. |
| `agent_id` | No | `default` | Agent identity attached to capture records unless a more specific identity is derived from the request. |
| `session_header` | No | `x-persisting-session-id` | Request header used to group calls into a session. |
| `capture_level` | No | `dialogue` | Amount of request and response content retained: `summary`, `dialogue`, or `full`. |
| `debug` | No | `false` | Writes Gateway diagnostics to the state directory. Logs can include bounded request and response bodies. |
| `models` | Yes | — | Ordered list of model routing rules. |
| `network` | No | `mode = "public"` | Policy for explicit forward-proxy traffic. |

The shared Gateway schema also accepts an `[overlay]` table, but
`pchronicle serve` does not create or apply a filesystem overlay. Overlay
lifecycle belongs to [pVisor](../../pvisor/guides/execution.md).

### Capture levels

- `summary` stores protocol metadata and byte counts without user or assistant
  message text.
- `dialogue` stores user and assistant dialogue and is the default.
- `full` additionally stores parsed request and response bodies. Use it only
  when the additional content and secret exposure are acceptable.

## Model routes

Routes are evaluated in file order; the first matching `name` wins. A name may
be an exact model, `prefix*`, `*suffix`, or the catch-all `*`.

| Field | Meaning |
| --- | --- |
| `name` | Required match pattern or exact target model name. Names must be unique. |
| `provider` | `openai`, `anthropic`, `gemini`, `vertex`, `bedrock`, `azure`, `copilot`, or `custom`; defaults to `openai`. |
| `upstream` | Upstream base URL, including its API prefix when applicable. Required unless `forward` is set. |
| `upstream_anthropic` | Optional Anthropic-compatible base used for `/v1/messages`; otherwise `upstream` is used. |
| `api_key_env` | Environment variable read by the `pchronicle` process. This is the recommended credential source. |
| `api_key` | Inline credential. Supported, but avoid committing secrets to configuration files. |
| `forward` | Exact name of another route. Rewrites the request model and uses that route's upstream. |

A route cannot set both `upstream` and `forward`. Forwarding must target a
route with an upstream and cannot be chained. If neither configuration nor the
named environment variable supplies a key, the Gateway can use a compatible
client authentication header; otherwise the request fails before forwarding.

Example with several providers:

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
capture_level = "dialogue"

[[models]]
name = "claude*"
provider = "anthropic"
upstream = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[[models]]
name = "gpt*"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[[models]]
name = "gemini*"
provider = "gemini"
upstream = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY"
```

## Forwarding and model rewriting

After route selection, Gateway normalizes the effective request path against
the `upstream` base. An upstream ending in `/v1` and a client request to
`/v1/chat/completions` therefore produce one `/v1/chat/completions`, not
`/v1/v1/chat/completions`. Passthrough query parameters are retained.

End-to-end headers are forwarded, while `Host`, `Content-Length`, hop-by-hop
headers, proxy authentication, and incoming LLM credentials are removed.
Gateway then applies the route credential as OpenAI Bearer, Anthropic
`x-api-key`, or Gemini `x-goog-api-key`. Redirects are returned to the client
instead of being followed inside Gateway.

`forward` rewrites a client-visible model to one exact target route:

```toml
[[models]]
name = "echo-upstream"
upstream = "http://127.0.0.1:19080/v1"

[[models]]
name = "*"
forward = "echo-upstream"
```

A request for `client-model` now reaches the target with
`"model":"echo-upstream"`. Capture metadata retains both identities and marks
the rewrite. Forward targets must define `upstream`, cannot forward again, and
cannot be selected through another pattern match. Put specific patterns before
broad patterns because the first match wins.

## Protocol rewriting

Gateway selects a protocol bridge from the client path and target route. Both
regular responses and SSE streams are translated:

| Client protocol | Target route | Upstream protocol |
| --- | --- | --- |
| Chat Completions | Non-Gemini | Chat Completions passthrough |
| Anthropic Messages | `upstream_anthropic` is set | Native Messages passthrough |
| Anthropic Messages | OpenAI-compatible without `upstream_anthropic` | Chat Completions |
| OpenAI Responses | Native OpenAI or Azure OpenAI | Responses passthrough |
| OpenAI Responses | Other OpenAI-compatible upstream | Chat Completions |
| Chat Completions, Messages, or Responses | `provider = "gemini"` | Gemini `generateContent` or `streamGenerateContent` |

For a translated call, Gateway rewrites the request path and body and renders
the response or supported error envelope back into the client's protocol. The
bridge preserves common messages, tool calls, usage, reasoning, and streaming
events, but does not promise lossless preservation of every provider-specific
extension. Prefer passthrough when the client relies on such fields.

## Test with the Echo upstream

The repository includes a deterministic Rust Echo server for testing real HTTP
forwarding without an API key or model service. It supports `/echo`, Chat
Completions, Messages, Responses, Gemini, and their streaming forms. The last
user text controls the assistant output.

Start it from a source checkout:

```bash
just echo

# Equivalent installed command:
pchronicle dev echo --listen 127.0.0.1:19080 --encoding plain
```

Point a route at it:

```toml
listen = "127.0.0.1:8787"
admin_listen = "127.0.0.1:8788"
capture_level = "full"

[[models]]
name = "echo-upstream"
provider = "openai"
upstream = "http://127.0.0.1:19080/v1"

[[models]]
name = "*"
forward = "echo-upstream"
```

By default, the assistant returns the last user text directly. Override one
request to receive standard Base64 instead:

```bash
curl http://127.0.0.1:8787/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-persisting-echo-encoding: base64' \
  -d '{
    "model": "client-alias",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "hello"}]
  }'
```

The Messages request is converted to Chat Completions, its model is rewritten
to `echo-upstream`, and the response is converted back to Messages with
`aGVsbG8=` as its text. Add `"stream": true` to exercise the same path with
SSE. The request header accepts `plain` or `base64`; `--encoding` sets the
server-wide default. The hidden testing command `pchronicle dev echo` only
binds to loopback and is intended for deterministic local Gateway tests.

## Network policy

`[network]` controls explicit proxy traffic such as `CONNECT` and absolute-URI
requests. Configured `models[].upstream` destinations are Gateway-owned routes,
not Agent egress grants; restrict the LLM surface with the model route list.

The available modes are:

```toml
[network]
mode = "no-network"
```

```toml
[network]
mode = "allowlist"
allowed_hosts = ["pypi.org", "files.pythonhosted.org", "*.github.com"]
```

`public` is the default. `no-network` denies explicit proxy egress.
`allowlist` requires a matching `allowed_hosts` entry or structured
`[[network.rules]]` rule. Explicit `[[network.deny_rules]]` entries take
precedence over allows. Prefer pVisor when this policy must be a
non-bypassable boundary for an Agent process; `pchronicle serve` only controls
traffic that the client sends through the Gateway.

## Dataset and state selection

The capture destination is selected in this order:

1. `--gateway-dataset NAME`;
2. the only positional Dataset, when exactly one exists.

If none of these yields one unambiguous Dataset, startup fails. The selected
name must refer to one of the positional `[NAME=]DATASET` mounts.

Canonical events are appended directly to that Dataset. Gateway runtime state
is separate and includes the session index, debug logs, and optional live
AgenticMD projection:

- for a local Dataset, state defaults to the Dataset path;
- for an object-store Dataset such as `s3://...`, pass a writable local
  `--gateway-state DIRECTORY`;
- pass `--gateway-stream-markdown` to maintain the live AgenticMD projection in
  the state directory.

Using an explicit state directory is also useful for keeping transient Gateway
files out of a local Dataset:

```bash
pchronicle serve \
  --gateway-config gateway.toml \
  --gateway-state ./.pchronicle-gateway \
  --gateway-stream-markdown \
  captures=./data/captures
```

## CLI precedence and safety

- `--listen` configures the Dataset server only. Gateway listeners always come from
  `gateway.toml`.
- `--gateway-debug` enables Gateway debugging even
  when `debug = false`; there is no CLI flag that forces configured debugging
  off.
- `--gateway-dataset`, `--gateway-state`, and
  `--gateway-stream-markdown` are composition settings and are not Gateway TOML
  fields.
- Dataset, Gateway, and admin listeners must all be loopback addresses. The
  services do not provide an authentication or authorization boundary.
- Debug and `full` capture can retain sensitive request or response content.
  Protect the state directory and capture Dataset accordingly.

## Observe new captures

Gateway events are durable after they have been flushed to the selected
Dataset. The `serve` projection supervisor discovers the canonical change,
updates its deterministic sibling Storyline Store, then completely rebuilds
and atomically swaps the server's Dataset catalog. Projection or refresh failures use
bounded retry and retain the old queryable Catalog; neither blocks durable
capture writes. `POST /api/catalog` remains available for an explicit manual
refresh, but it is not required for captured events to become visible. On
`SIGINT` or `SIGTERM`, `pchronicle serve` stops both services and finishes the
Gateway capture writer before exiting.

For exact command flags, see the [`pchronicle` CLI reference](../reference/cli.md).
For the storage model behind refresh, see
[Dataset Catalog design](../design/catalog.md).
