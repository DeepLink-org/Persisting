# Gateway fuzz regressions

Gateway fuzzing is split by contract so a failure identifies the broken layer
instead of collapsing format, routing, persistence, and policy into one result.
Every scenario uses prebuilt `target/release/pchronicle`; none compiles Rust.

| Scenario | Contract | Command |
|---|---|---|
| `formats` | OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, and Gemini wire formats; streaming, valid JSON Unicode/whitespace controls, plain text, Base64, tool calls, reasoning, multimodal input, structured errors, and complete terminal Responses output | `just gateway-fuzz-formats` |
| `forwarding` | wildcard forwarding, target model rewrite, Responses-to-Chat conversion, and persisted `forward_to` evidence | `just gateway-fuzz-forwarding` |
| `storage` | graceful queue drain, session-level Lance durability, ordered lifecycle pairing, canonical tools/reasoning/multimodal/error bodies, and bounded multi-source queries | `just gateway-fuzz-storage` |
| `network-policy` | local-only allowlist and no-network decisions for absolute-URI HTTP, CONNECT, host/port denial, and model-owned relative LLM routes | `just gateway-fuzz-network` |

Run all four:

```bash
just gateway-fuzz
```

The aggregate keeps the historical one-minute default by assigning 15 seconds
to each scenario. A scenario run directly defaults to one minute. Override the
duration, rate, concurrency, or maximum message size without editing it:

```bash
PERSISTING_FUZZ_DURATION_SECONDS=3600 \
PERSISTING_FUZZ_REQUESTS_PER_SECOND=20 \
PERSISTING_FUZZ_CONCURRENCY=32 \
PERSISTING_FUZZ_MAX_MESSAGE_CHARS=16384 \
just gateway-fuzz-storage
```

Each scenario chooses a random 64-bit seed. Replay one generated case with the
printed seed and case index:

```bash
PERSISTING_FUZZ_SEED=123456789 \
PERSISTING_FUZZ_REPLAY_CASE=231 \
PERSISTING_KEEP_TEST_ARTIFACTS=1 \
just gateway-fuzz-forwarding
```

Artifacts are always retained on failure. Format/forward/storage artifacts
contain the case plan, SDK results, and process logs; forwarding and storage
also contain canonical events and per-session comparisons. Network-policy
artifacts contain policy configurations, asserted denial reasons, and
status/body digests. All network
policy targets are loopback services created by the scenario—this test never
depends on public network access.

成功时默认会删除临时产物。要人工检查 `formats` 数据，请保留它们：

```bash
PERSISTING_KEEP_TEST_ARTIFACTS=1 just gateway-fuzz-formats
```

命令会打印 `Gateway formats fuzz artifacts: <目录>`。主要文件是：

- `run.json`：seed 和运行参数；
- `logs/cases.jsonl`：每个生成用例的完整输入和协议参数；
- `logs/client-results.jsonl`：SDK 看到的结果摘要；
- `logs/format-contract-results.jsonl`：固定格式契约用例的实际结果；
- `dataset/`：Gateway 写入的 Lance capture Dataset；
- `gateway-state/`：WAL 和 Gateway 状态。

`formats` 不把持久化核对作为 pass/fail 契约，但保留后的 Dataset 仍可手工导出：

```bash
fuzz_artifacts=/tmp/persisting-gateway-formats-fuzz.example
target/release/pchronicle query "$fuzz_artifacts/dataset" \
  'SELECT seq, kind, session_id, model, call_id, payload_json FROM dataset.events ORDER BY session_id, seq' \
  --format jsonl \
  --output "$fuzz_artifacts/logs/events.jsonl"
```

如果希望测试本身自动核对持久化事件，请运行
`PERSISTING_KEEP_TEST_ARTIFACTS=1 just gateway-fuzz-storage`；它还会生成
`logs/events.jsonl` 和 `logs/comparison.jsonl`。

`formats` deliberately stops at the client/wire contract. It proves that the
Gateway can accept, forward/convert, and return each exercised shape, but it
does **not** prove that capture reached durable storage. A broken capture path
can therefore pass `formats`; `storage` and `forwarding` perform the durable
event reconciliation. Malformed JSON and transport-level invalid bytes are
also outside the random SDK alphabet and belong in explicit negative cases,
so an SDK rejection is not misreported as a Gateway defect.

Storage verification queries only sources owned by the run in bounded groups,
which keeps file-descriptor use bounded under low macOS process limits. It
checks each session as an ordered lifecycle: exactly one request, zero
cancellations, zero or more explicitly marked drafts, exactly one terminal
response, stable `call_id`, and strictly increasing sequence numbers. It does
not assume that a session always contains exactly two physical events.
The controlled storage cases additionally reconcile canonical tool-call
arguments, reasoning parts, image data sources, structured upstream errors,
and the terminal Responses tool-call event.

Every local service binds `127.0.0.1:0` itself and publishes the selected
address. The harness never reserves a port by bind-close-rebind.

The parent directory contains `.long-running`, so `just regression` skips this
suite.
