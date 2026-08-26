//! Black-box Langfuse OTLP Gateway pressure benchmark.
//!
//! This is intentionally ignored: it starts a real `pchronicle serve` child,
//! performs concurrent HTTP requests, and writes a sizeable temporary dataset.
//! Run it on controlled hardware with:
//!
//! ```text
//! PCHRONICLE_LANGFUSE_STRESS_REQUESTS=128 \
//! PCHRONICLE_LANGFUSE_STRESS_SPANS_PER_REQUEST=512 \
//! PCHRONICLE_LANGFUSE_STRESS_CONCURRENCY=16 \
//! cargo test -p persisting-pchronicle-cli --test langfuse_gateway_stress \
//!   langfuse_gateway_pressure -- --ignored --nocapture
//! ```
//!
//! The benchmark validates more than throughput: every request must receive a
//! full-success OTLP response, every span must become one durable event, and
//! trace/span/parent relationships must survive the Gateway chunking boundary.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

const DEFAULT_REQUESTS: usize = 32;
const DEFAULT_SPANS_PER_REQUEST: usize = 512;
const DEFAULT_CONCURRENCY: usize = 8;

fn env_usize(name: &str, default: usize) -> Result<usize> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value
        .parse::<usize>()
        .with_context(|| format!("{name} must be a positive integer"))?;
    if parsed == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(parsed)
}

fn attr(key: &str, value: impl Into<String>) -> Value {
    json!({"key": key, "value": {"stringValue": value.into()}})
}

fn trace_id(request: usize) -> String {
    format!("{:032x}", request + 1)
}

fn span_id(request: usize, span: usize) -> String {
    format!("{:016x}", (request as u64) << 32 | (span as u64 + 1))
}

fn otlp_batch(request: usize, spans_per_request: usize) -> Result<Vec<u8>> {
    let trace_id = trace_id(request);
    let session_id = format!("langfuse-bench-session-{request}");
    let user_id = format!("langfuse-bench-user-{}", request % 16);
    let base_ns = 1_800_000_000_000_000_000u64 + request as u64 * 1_000_000_000;
    let spans = (0..spans_per_request)
        .map(|span| {
            let current_span_id = span_id(request, span);
            let is_root = span == 0;
            let observation_type = if is_root {
                "agent"
            } else if span % 7 == 0 {
                "tool"
            } else {
                "generation"
            };
            let mut attributes = vec![
                attr("user.id", user_id.clone()),
                attr("session.id", session_id.clone()),
                attr("langfuse.trace.name", "Gateway pressure benchmark"),
                attr("langfuse.observation.type", observation_type),
                attr(
                    "langfuse.observation.input",
                    format!("input-{request}-{span}"),
                ),
                attr(
                    "langfuse.observation.output",
                    format!("output-{request}-{span}"),
                ),
            ];
            if observation_type == "generation" {
                attributes.push(attr("langfuse.observation.model.name", "bench-model"));
            }
            let mut span_json = json!({
                "traceId": trace_id,
                "spanId": current_span_id,
                "name": if is_root { "Codex Turn" } else { "LLM" },
                "kind": 1,
                "startTimeUnixNano": base_ns + span as u64 * 1_000_000,
                "endTimeUnixNano": base_ns + span as u64 * 1_000_000 + 500_000,
                "attributes": attributes,
                "events": [],
                "links": [],
                "status": {"code": 0}
            });
            if !is_root {
                span_json["parentSpanId"] = Value::String(span_id(request, 0));
            }
            span_json
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&json!({
        "resourceSpans": [{
            "resource": {"attributes": [
                attr("service.name", "langfuse-gateway-benchmark"),
                attr("telemetry.sdk.language", "rust")
            ]},
            "scopeSpans": [{
                "scope": {"name": "langfuse-sdk", "version": "5.4.1"},
                "spans": spans
            }]
        }]
    }))
    .context("encode synthetic Langfuse OTLP batch")
}

async fn query_jsonl(binary: &str, dataset: &std::path::Path, sql: &str) -> Result<Vec<Value>> {
    let output = Command::new(binary)
        .args([
            "query",
            "--mount",
            &format!("default={}", dataset.display()),
            "--sql",
            sql,
            "--format",
            "jsonl",
        ])
        .output()
        .await
        .context("run pChronicle query after Gateway benchmark")?;
    if !output.status.success() {
        bail!(
            "pChronicle query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty() && line.first() == Some(&b'{'))
        .map(|line| serde_json::from_slice(line).context("decode pChronicle JSONL row"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in real HTTP pressure benchmark; tune PCHRONICLE_LANGFUSE_STRESS_*"]
async fn langfuse_gateway_pressure() -> Result<()> {
    let requests = env_usize("PCHRONICLE_LANGFUSE_STRESS_REQUESTS", DEFAULT_REQUESTS)?;
    let spans_per_request = env_usize(
        "PCHRONICLE_LANGFUSE_STRESS_SPANS_PER_REQUEST",
        DEFAULT_SPANS_PER_REQUEST,
    )?;
    let concurrency = env_usize(
        "PCHRONICLE_LANGFUSE_STRESS_CONCURRENCY",
        DEFAULT_CONCURRENCY,
    )?
    .min(requests);

    let temporary = tempfile::tempdir()?;
    let dataset = temporary.path().join("captures");
    let binary = env!("CARGO_BIN_EXE_pchronicle");
    let mut child = Command::new(binary)
        .args([
            "serve",
            "--gateway",
            "auto",
            "--gateway-dataset",
            dataset.to_str().context("dataset path is not UTF-8")?,
            "--gateway-split",
            "{user}/{date}/{hour}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start pChronicle Gateway benchmark child")?;
    let stdout = child
        .stdout
        .take()
        .context("capture Gateway readiness output")?;
    let mut ready_line = String::new();
    tokio::time::timeout(
        Duration::from_secs(30),
        BufReader::new(stdout).read_line(&mut ready_line),
    )
    .await
    .context("wait for Gateway readiness timed out")??;
    let ready: Value = serde_json::from_str(&ready_line).context("decode Gateway readiness")?;
    let endpoint = ready["gateway_endpoint"]
        .as_str()
        .context("Gateway readiness did not advertise gateway_endpoint")?;
    let url = format!("http://{endpoint}/api/public/otel/v1/traces");

    let client = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(concurrency)
        .timeout(Duration::from_secs(120))
        .build()?;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(requests);
    for request in 0..requests {
        let permit = semaphore.clone().acquire_owned().await?;
        let client = client.clone();
        let url = url.clone();
        let body = Arc::new(otlp_batch(request, spans_per_request)?);
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let response = client
                .post(url)
                .header("content-type", "application/json")
                .header("x-persisting-user-id", format!("langfuse-bench-user-{}", request % 16))
                .body(body.as_ref().clone())
                .send()
                .await
                .with_context(|| format!("send OTLP benchmark request {request}"))?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let response_body = response.bytes().await?;
            if status != reqwest::StatusCode::OK {
                bail!("request {request} returned HTTP {status}: {response_body:?}");
            }
            if !content_type.starts_with("application/json") || response_body.as_ref() != b"{}" {
                bail!(
                    "request {request} did not receive full-success OTLP JSON response: content-type={content_type}, body={response_body:?}"
                );
            }
            Ok::<u128, anyhow::Error>(started.elapsed().as_millis())
        }));
    }
    let mut latencies_ms = Vec::with_capacity(requests);
    for task in tasks {
        latencies_ms.push(task.await.context("join Gateway benchmark request")??);
    }
    let elapsed = started.elapsed();

    child.kill().await.context("stop Gateway benchmark child")?;
    child.wait().await.context("reap Gateway benchmark child")?;

    let expected_events = requests
        .checked_mul(spans_per_request)
        .context("benchmark event count overflow")?;
    let rows = query_jsonl(
        binary,
        &dataset,
        "SELECT COUNT(*) AS events, COUNT(DISTINCT trace_id) AS traces, COUNT(DISTINCT call_id) AS spans, COUNT(event_id) AS ids, COUNT(DISTINCT event_id) AS unique_ids FROM default.events",
    )
    .await?;
    let counts = rows
        .first()
        .context("Gateway query returned no count row")?;
    let events = counts["events"]
        .as_u64()
        .context("events count is not u64")?;
    let traces = counts["traces"]
        .as_u64()
        .context("traces count is not u64")?;
    let spans = counts["spans"].as_u64().context("spans count is not u64")?;
    let ids = counts["ids"].as_u64().context("ids count is not u64")?;
    let unique_ids = counts["unique_ids"]
        .as_u64()
        .context("unique_ids count is not u64")?;
    if events != expected_events as u64
        || traces != requests as u64
        || spans != expected_events as u64
        || ids != expected_events as u64
        || unique_ids != expected_events as u64
    {
        bail!(
            "durability/integrity mismatch: expected events={expected_events}, traces={requests}, spans={expected_events}, ids={expected_events}; got {counts}"
        );
    }
    let kinds = query_jsonl(
        binary,
        &dataset,
        "SELECT kind, COUNT(*) AS count FROM default.events GROUP BY kind ORDER BY kind",
    )
    .await?;
    let relationship_rows = query_jsonl(
        binary,
        &dataset,
        "SELECT COUNT(*) AS missing_parent FROM default.events WHERE kind <> 'otel.agent' AND parent_call_id IS NULL",
    )
    .await?;
    let missing_parent = relationship_rows
        .first()
        .and_then(|row| row["missing_parent"].as_u64())
        .context("missing_parent count is not u64")?;
    if missing_parent != 0 {
        bail!("{missing_parent} non-root Langfuse spans lost their parent_call_id");
    }
    let expected_tools = requests as u64 * spans_per_request.saturating_sub(1) as u64 / 7;
    let expected_generations = expected_events as u64 - requests as u64 - expected_tools;
    let modeled_rows = query_jsonl(
        binary,
        &dataset,
        "SELECT COUNT(*) AS modeled FROM default.events WHERE kind = 'otel.generation' AND model = 'bench-model'",
    )
    .await?;
    let modeled = modeled_rows
        .first()
        .and_then(|row| row["modeled"].as_u64())
        .context("modeled count is not u64")?;
    if modeled != expected_generations {
        bail!(
            "model promotion mismatch: expected {expected_generations} generation events, got {modeled}"
        );
    }
    let elapsed_secs = elapsed.as_secs_f64();
    latencies_ms.sort_unstable();
    let percentile = |numerator: usize, denominator: usize| -> u128 {
        let index = ((latencies_ms.len() * numerator).saturating_add(denominator - 1)
            / denominator)
            .saturating_sub(1)
            .min(latencies_ms.len().saturating_sub(1));
        latencies_ms[index]
    };
    println!(
        "langfuse_gateway_pressure: requests={requests} spans_per_request={spans_per_request} concurrency={concurrency} events={events} traces={traces} elapsed_ms={} requests_per_sec={:.1} spans_per_sec={:.1} latency_ms={{p50:{},p95:{},p99:{}}} kinds={}",
        elapsed.as_millis(),
        requests as f64 / elapsed_secs,
        expected_events as f64 / elapsed_secs,
        percentile(50, 100),
        percentile(95, 100),
        percentile(99, 100),
        serde_json::to_string(&kinds)?
    );
    Ok(())
}
