# persisting-gateway

`persisting-gateway` is pVisor's built-in Agent protocol driver. It implements
`persisting-overlaynet::OverlaySink` and owns the application-level path from
LLM HTTP exchanges to trajectory events:

- recognize and adapt supported Agent/LLM protocols;
- select and forward to upstream providers;
- correlate run, session, story, and call identities;
- capture canonical `pChronicle` events;
- coordinate WAL and live human-readable projections.

The crate does not own the proxy data plane or the canonical trajectory storage
format. `persisting-overlaynet` owns proxy transport, access enforcement, and
generic sink dispatch. `persisting-pchronicle` owns schemas, persistence,
reading, replay, conversion, and derived views.

Capture remains the name of the user-facing capability. It runs through
`pvisor run`; Gateway is an internal pVisor driver and a reusable crate, not a
peer product or standalone service.

## Chronicle LLM semantics

Gateway parses each accepted client request once, before model rewrite or wire
conversion, into pChronicle's versioned `LlmRequestEventPayload` (`llm/v1`). The
typed request preserves ordered messages, multimodal content, tool calls and
results, generation parameters, structured output, reasoning signatures, and
namespaced provider extensions. Protocol rendering and capture share that same
in-memory value, which becomes canonical `EventRecord.payload.llm_request`. The
WAL retains only the original wire JSON and reconstructs semantics during crash
replay, avoiding a second copy of large multimodal payloads on disk.
Gateway acknowledges a WAL row only after the canonical pChronicle sink has
confirmed its Lance publication. A failed sink write is also recorded as a
dead letter, but remains unacknowledged for restart replay.

WAL event and ACK writes use bounded-delay best-effort group commit. Request
threads only attempt a non-blocking bounded-queue submission; serialization,
file writes, and `sync_data` all run on a dedicated writer. The writer waits up
to 2 ms and writes at most 256 lines per fsync. Queue saturation or a process
crash before the next commit can lose the WAL copy, but never blocks forwarding;
canonical Lance capture continues on its independent apply path. Losing an ACK
only causes safe replay. Runtime flush/shutdown includes a WAL barrier before
pending-entry inspection or truncation.

Non-streaming responses follow `provider wire -> typed response -> client wire`.
Streaming follows `provider SSE -> typed stream events -> client SSE` and folds
the same events into the captured typed response. Provider wire formats are
never chained through Chat Completions as an intermediate protocol. Storyline
remains a derived trajectory view and is not part of the online
protocol-conversion path.

## Gemini native

A route with `provider = "gemini"` uses Google's native `generateContent` API. Chat
Completions, Anthropic Messages, and OpenAI Responses requests are converted to the native
wire format; non-streaming and SSE responses are converted back to the caller's protocol.

```toml
[[models]]
name = "gemini-2.5-pro"
provider = "gemini"
upstream = "https://generativelanguage.googleapis.com/v1beta"
api_key_env = "GEMINI_API_KEY" # GOOGLE_API_KEY is accepted as an alias
```

The upstream request targets `models/{model}:generateContent` or
`models/{model}:streamGenerateContent?alt=sse` and authenticates with `x-goog-api-key`.
Native Gemini client paths can also pass through directly. Conversion does not change the
capture contract: WAL and request events retain the original client JSON before rewrite,
alongside its typed Chronicle semantics.

## Deterministic Echo upstream

`pchronicle echo` provides local Chat Completions, Messages, Responses, and
Gemini endpoints. It returns the last user text directly or as standard Base64
and supports the corresponding SSE streaming shapes. This makes the request
input the complete test fixture while still exercising real Gateway HTTP
forwarding.

```bash
just echo

# Or invoke the installed CLI directly:
pchronicle echo --listen 127.0.0.1:19080 --encoding plain
```

The server only accepts a loopback listener. Override its default per request:

```text
x-persisting-echo-encoding: plain
x-persisting-echo-encoding: base64
```

It only accepts a loopback listener and is intended for deterministic local
Gateway tests.
