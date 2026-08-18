//! Allocation, throughput, tail-latency, and process-RSS benchmark for a cold
//! projected ATIF JSON datasource + DataFusion query iteration.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use persisting_pchronicle::document::DocumentFormat;
use persisting_pchronicle::query::{ChronicleQueryEngine, ChronicleQueryExecutionOptions};

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

// The counters intentionally measure successful allocation traffic rather
// than live bytes; peak resident memory is reported independently via rusage.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !pointer.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        pointer
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Debug)]
struct Sample {
    elapsed: Duration,
    allocations: u64,
    allocated_bytes: u64,
    rows_scanned: u64,
    input_buffer_peak_bytes: u64,
}

fn main() -> Result<()> {
    let scale = env_usize("PCHRONICLE_BENCH_SCALE", 128);
    let iterations = env_usize("PCHRONICLE_BENCH_ITERS", 20);
    let json_shape =
        std::env::var("PCHRONICLE_BENCH_JSON_SHAPE").unwrap_or_else(|_| "ndjson".to_string());
    anyhow::ensure!(scale > 0, "PCHRONICLE_BENCH_SCALE must be positive");
    anyhow::ensure!(iterations > 0, "PCHRONICLE_BENCH_ITERS must be positive");
    anyhow::ensure!(
        matches!(json_shape.as_str(), "ndjson" | "array"),
        "PCHRONICLE_BENCH_JSON_SHAPE must be 'ndjson' or 'array'"
    );

    let temp = tempfile::tempdir()?;
    let input = temp.path().join(if json_shape == "array" {
        "streaming.json"
    } else {
        "streaming.ndjson"
    });
    let (documents, steps) = write_corpus(&input, scale, &json_shape)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_query(&input))?;

    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        reset_allocations();
        let started = Instant::now();
        let query = runtime.block_on(run_query(&input))?;
        let elapsed = started.elapsed();
        let (allocations, allocated_bytes) = allocation_snapshot();
        samples.push(Sample {
            elapsed,
            allocations,
            allocated_bytes,
            rows_scanned: query.0,
            input_buffer_peak_bytes: query.1,
        });
    }

    anyhow::ensure!(samples.iter().all(|sample| sample.rows_scanned == steps));
    let median = percentile_duration(&samples, 0.50);
    let p95 = percentile_duration(&samples, 0.95);
    let median_allocations = percentile_u64(&samples, 0.50, |sample| sample.allocations);
    let p95_allocations = percentile_u64(&samples, 0.95, |sample| sample.allocations);
    let median_allocated_bytes = percentile_u64(&samples, 0.50, |sample| sample.allocated_bytes);
    let p95_allocated_bytes = percentile_u64(&samples, 0.95, |sample| sample.allocated_bytes);
    let input_buffer_peak_bytes = samples
        .iter()
        .map(|sample| sample.input_buffer_peak_bytes)
        .max()
        .unwrap_or_default();
    let rows_per_second = steps as f64 / median.as_secs_f64();
    let rss_mib = process_peak_rss_bytes()? as f64 / (1024.0 * 1024.0);

    println!(
        "dataset: shape={json_shape}, {documents} trajectories, {steps} steps, {} bytes",
        std::fs::metadata(&input)?.len()
    );
    println!(
        "projected JSON: median={:.3} ms p95={:.3} ms rows/s={rows_per_second:.0}",
        milliseconds(median),
        milliseconds(p95)
    );
    println!(
        "allocation traffic: median={median_allocations} calls/{median_allocated_bytes} bytes, \
         p95={p95_allocations} calls/{p95_allocated_bytes} bytes"
    );
    println!(
        "memory: process peak RSS={rss_mib:.3} MiB, input buffer peak={input_buffer_peak_bytes} bytes"
    );
    println!(
        "RESULT benchmark=json_streaming shape={json_shape} documents={documents} rows={steps} iterations={iterations} \
         median_ms={:.3} p95_ms={:.3} rows_s={rows_per_second:.0} \
         median_allocations={median_allocations} p95_allocations={p95_allocations} \
         median_allocated_bytes={median_allocated_bytes} p95_allocated_bytes={p95_allocated_bytes} \
         process_peak_rss_mib={rss_mib:.3} input_buffer_peak_bytes={input_buffer_peak_bytes}",
        milliseconds(median),
        milliseconds(p95),
    );
    Ok(())
}

async fn run_query(input: &std::path::Path) -> Result<(u64, u64)> {
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        input,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    engine
        .query("SELECT source, COUNT(*) FROM steps GROUP BY source")
        .await?;
    let metrics = engine.local_file_metrics().expect("ATIF file metrics");
    Ok((metrics.rows_scanned, metrics.streaming_buffer_peak_bytes))
}

fn write_corpus(path: &std::path::Path, scale: usize, json_shape: &str) -> Result<(usize, u64)> {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif");
    let fixture_names = [
        "dialogue_10.json",
        "long_context_20.json",
        "multimodal_18.json",
        "parallel_tools_14.json",
        "reasoning_16.json",
        "sequential_tools_12.json",
        "sparse_13.json",
        "unicode_zh_15.json",
    ];
    let fixtures = fixture_names
        .iter()
        .map(|name| std::fs::read_to_string(fixture_root.join(name)))
        .map(|result| result.map_err(anyhow::Error::from))
        .map(|result| {
            result.and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text).map_err(Into::into)
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut output = BufWriter::new(File::create(path)?);
    let mut steps = 0_u64;
    let array = json_shape == "array";
    let mut first = true;
    if array {
        output.write_all(b"[\n")?;
    }
    for copy in 0..scale {
        for (index, fixture) in fixtures.iter().enumerate() {
            let mut trajectory = fixture.clone();
            let session = format!("stream-{copy:06}-{index:02}");
            trajectory["session_id"] = serde_json::Value::String(session.clone());
            trajectory["trajectory_id"] = serde_json::Value::String(session);
            steps += trajectory["steps"].as_array().map_or(0, Vec::len) as u64;
            if array && !first {
                output.write_all(b",\n")?;
            }
            serde_json::to_writer(&mut output, &trajectory)?;
            if !array {
                output.write_all(b"\n")?;
            }
            first = false;
        }
    }
    if array {
        output.write_all(b"\n]\n")?;
    }
    output.flush()?;
    Ok((fixtures.len() * scale, steps))
}
fn reset_allocations() {
    ALLOCATION_COUNT.store(0, Ordering::SeqCst);
    ALLOCATED_BYTES.store(0, Ordering::SeqCst);
}

fn allocation_snapshot() -> (u64, u64) {
    (
        ALLOCATION_COUNT.load(Ordering::SeqCst),
        ALLOCATED_BYTES.load(Ordering::SeqCst),
    )
}

fn percentile_duration(samples: &[Sample], percentile: f64) -> Duration {
    let mut values = samples
        .iter()
        .map(|sample| sample.elapsed)
        .collect::<Vec<_>>();
    values.sort_unstable();
    values[percentile_index(values.len(), percentile)]
}

fn percentile_u64(samples: &[Sample], percentile: f64, value: impl Fn(&Sample) -> u64) -> u64 {
    let mut values = samples.iter().map(value).collect::<Vec<_>>();
    values.sort_unstable();
    values[percentile_index(values.len(), percentile)]
}

fn percentile_index(length: usize, percentile: f64) -> usize {
    ((percentile * length as f64).ceil() as usize)
        .saturating_sub(1)
        .min(length - 1)
}

#[cfg(unix)]
fn process_peak_rss_bytes() -> Result<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    anyhow::ensure!(
        status == 0,
        "getrusage failed: {}",
        std::io::Error::last_os_error()
    );
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    return Ok(usage.ru_maxrss as u64);
    #[cfg(not(target_os = "macos"))]
    return Ok((usage.ru_maxrss as u64).saturating_mul(1024));
}

#[cfg(not(unix))]
fn process_peak_rss_bytes() -> Result<u64> {
    anyhow::bail!("json_streaming peak RSS measurement requires a Unix target")
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
