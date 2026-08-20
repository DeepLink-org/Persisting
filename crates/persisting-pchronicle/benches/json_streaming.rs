//! Allocation, throughput, tail-latency, and process-RSS benchmark for a cold
//! projected JSON datasource + DataFusion query iteration.
//!
//! Environment variables:
//! - `PCHRONICLE_BENCH_SCALE` (default: 128)
//! - `PCHRONICLE_BENCH_ITERS` (default: 20)
//! - `PCHRONICLE_BENCH_JSON_SHAPE`: `ndjson` | `array` (default: `ndjson`)
//! - `PCHRONICLE_BENCH_FORMAT`: `atif` | `actf` (default: `atif`)
//! - `PCHRONICLE_BENCH_PATH`: `projected` | `full` | `both` (default: `projected`)

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchFormat {
    Atif,
    Actf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchPath {
    Projected,
    Full,
}

#[derive(Debug)]
struct Sample {
    elapsed: Duration,
    allocations: u64,
    allocated_bytes: u64,
    rows_scanned: u64,
    files_parsed: u64,
    input_buffer_peak_bytes: u64,
    projected_files: u64,
}

#[derive(Debug)]
struct BenchReport {
    format: BenchFormat,
    path: BenchPath,
    json_shape: String,
    documents: usize,
    steps: u64,
    file_bytes: u64,
    iterations: usize,
    median: Duration,
    p95: Duration,
    median_allocations: u64,
    p95_allocations: u64,
    median_allocated_bytes: u64,
    p95_allocated_bytes: u64,
    input_buffer_peak_bytes: u64,
    rss_mib: f64,
}

fn main() -> Result<()> {
    let scale = env_usize("PCHRONICLE_BENCH_SCALE", 128);
    let iterations = env_usize("PCHRONICLE_BENCH_ITERS", 20);
    let json_shape =
        std::env::var("PCHRONICLE_BENCH_JSON_SHAPE").unwrap_or_else(|_| "ndjson".to_string());
    let format = parse_format(
        &std::env::var("PCHRONICLE_BENCH_FORMAT").unwrap_or_else(|_| "atif".to_string()),
    )?;
    let path_mode = parse_path_mode(
        &std::env::var("PCHRONICLE_BENCH_PATH").unwrap_or_else(|_| "projected".to_string()),
    )?;
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
    let (documents, steps) = write_corpus(&input, scale, &json_shape, format)?;
    let file_bytes = std::fs::metadata(&input)?.len();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let paths = match path_mode {
        BenchPathMode::Projected => vec![BenchPath::Projected],
        BenchPathMode::Full => vec![BenchPath::Full],
        BenchPathMode::Both => vec![BenchPath::Projected, BenchPath::Full],
    };
    let dataset = BenchDataset {
        json_shape: &json_shape,
        input: &input,
        documents,
        steps,
        file_bytes,
        iterations,
    };

    let mut reports = Vec::with_capacity(paths.len());
    for path in paths {
        runtime.block_on(run_query(format, path, &input))?;
        let report = benchmark_path(&runtime, format, path, &dataset)?;
        print_report(&report);
        reports.push(report);
    }

    if reports.len() == 2 {
        print_comparison(&reports[0], &reports[1]);
    }
    Ok(())
}

struct BenchDataset<'a> {
    json_shape: &'a str,
    input: &'a std::path::Path,
    documents: usize,
    steps: u64,
    file_bytes: u64,
    iterations: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchPathMode {
    Projected,
    Full,
    Both,
}

fn parse_format(value: &str) -> Result<BenchFormat> {
    match value {
        "atif" => Ok(BenchFormat::Atif),
        "actf" => Ok(BenchFormat::Actf),
        other => anyhow::bail!("PCHRONICLE_BENCH_FORMAT must be 'atif' or 'actf', got '{other}'"),
    }
}

fn parse_path_mode(value: &str) -> Result<BenchPathMode> {
    match value {
        "projected" => Ok(BenchPathMode::Projected),
        "full" => Ok(BenchPathMode::Full),
        "both" => Ok(BenchPathMode::Both),
        other => anyhow::bail!(
            "PCHRONICLE_BENCH_PATH must be 'projected', 'full', or 'both', got '{other}'"
        ),
    }
}

fn benchmark_path(
    runtime: &tokio::runtime::Runtime,
    format: BenchFormat,
    path: BenchPath,
    dataset: &BenchDataset<'_>,
) -> Result<BenchReport> {
    let mut samples = Vec::with_capacity(dataset.iterations);
    for _ in 0..dataset.iterations {
        reset_allocations();
        let started = Instant::now();
        let query = runtime.block_on(run_query(format, path, dataset.input))?;
        let elapsed = started.elapsed();
        let (allocations, allocated_bytes) = allocation_snapshot();
        samples.push(Sample {
            elapsed,
            allocations,
            allocated_bytes,
            rows_scanned: query.rows_scanned,
            files_parsed: query.files_parsed,
            input_buffer_peak_bytes: query.streaming_buffer_peak_bytes,
            projected_files: query.projected_files,
        });
    }

    match path {
        BenchPath::Projected => {
            anyhow::ensure!(
                samples.iter().all(|sample| sample.projected_files > 0),
                "projected path did not record projected_files"
            );
            anyhow::ensure!(
                samples
                    .iter()
                    .all(|sample| sample.rows_scanned == dataset.steps),
                "projected path rows_scanned mismatch"
            );
        }
        BenchPath::Full => {
            anyhow::ensure!(
                samples.iter().all(|sample| sample.projected_files == 0),
                "full path unexpectedly used projected streaming"
            );
            anyhow::ensure!(
                samples.iter().all(|sample| sample.files_parsed > 0),
                "full path did not parse any files"
            );
        }
    }

    let median = percentile_duration(&samples, 0.50);
    let p95 = percentile_duration(&samples, 0.95);
    Ok(BenchReport {
        format,
        path,
        json_shape: dataset.json_shape.to_string(),
        documents: dataset.documents,
        steps: dataset.steps,
        file_bytes: dataset.file_bytes,
        iterations: dataset.iterations,
        median,
        p95,
        median_allocations: percentile_u64(&samples, 0.50, |sample| sample.allocations),
        p95_allocations: percentile_u64(&samples, 0.95, |sample| sample.allocations),
        median_allocated_bytes: percentile_u64(&samples, 0.50, |sample| sample.allocated_bytes),
        p95_allocated_bytes: percentile_u64(&samples, 0.95, |sample| sample.allocated_bytes),
        input_buffer_peak_bytes: samples
            .iter()
            .map(|sample| sample.input_buffer_peak_bytes)
            .max()
            .unwrap_or_default(),
        rss_mib: process_peak_rss_bytes()? as f64 / (1024.0 * 1024.0),
    })
}

fn print_report(report: &BenchReport) {
    let rows_per_second = report.steps as f64 / report.median.as_secs_f64();
    let format_name = match report.format {
        BenchFormat::Atif => "atif",
        BenchFormat::Actf => "actf",
    };
    let path_name = match report.path {
        BenchPath::Projected => "projected",
        BenchPath::Full => "full",
    };
    println!(
        "dataset: format={format_name}, path={path_name}, shape={}, {} trajectories, {} steps, {} bytes",
        report.json_shape, report.documents, report.steps, report.file_bytes
    );
    println!(
        "{path_name} JSON: median={:.3} ms p95={:.3} ms rows/s={rows_per_second:.0}",
        milliseconds(report.median),
        milliseconds(report.p95)
    );
    println!(
        "allocation traffic: median={} calls/{} bytes, p95={} calls/{} bytes",
        report.median_allocations,
        report.median_allocated_bytes,
        report.p95_allocations,
        report.p95_allocated_bytes
    );
    println!(
        "memory: process peak RSS={:.3} MiB, input buffer peak={} bytes",
        report.rss_mib, report.input_buffer_peak_bytes
    );
    println!(
        "RESULT benchmark=json_streaming format={format_name} path={path_name} shape={} documents={} rows={} iterations={} \
         median_ms={:.3} p95_ms={:.3} rows_s={rows_per_second:.0} \
         median_allocations={} p95_allocations={} \
         median_allocated_bytes={} p95_allocated_bytes={} \
         process_peak_rss_mib={:.3} input_buffer_peak_bytes={}",
        report.json_shape,
        report.documents,
        report.steps,
        report.iterations,
        milliseconds(report.median),
        milliseconds(report.p95),
        report.median_allocations,
        report.p95_allocations,
        report.median_allocated_bytes,
        report.p95_allocated_bytes,
        report.rss_mib,
        report.input_buffer_peak_bytes
    );
}

fn print_comparison(projected: &BenchReport, full: &BenchReport) {
    let speedup = full.median.as_secs_f64() / projected.median.as_secs_f64();
    let alloc_ratio = full.median_allocated_bytes as f64 / projected.median_allocated_bytes as f64;
    println!(
        "comparison: projected median {:.3} ms vs full {:.3} ms ({speedup:.2}x faster), \
         allocated bytes ratio {alloc_ratio:.2}x (full/projected)",
        milliseconds(projected.median),
        milliseconds(full.median),
    );
}

struct QueryMetrics {
    rows_scanned: u64,
    files_parsed: u64,
    streaming_buffer_peak_bytes: u64,
    projected_files: u64,
}

async fn run_query(
    format: BenchFormat,
    path: BenchPath,
    input: &std::path::Path,
) -> Result<QueryMetrics> {
    let document_format = match format {
        BenchFormat::Atif => DocumentFormat::Atif,
        BenchFormat::Actf => DocumentFormat::Actf,
    };
    let engine = ChronicleQueryEngine::open(
        document_format,
        input,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let sql = match path {
        BenchPath::Projected => "SELECT source, COUNT(*) FROM steps GROUP BY source",
        BenchPath::Full => "SELECT * FROM steps",
    };
    engine.query(sql).await?;
    let metrics = engine.local_file_metrics().expect("local file metrics");
    Ok(QueryMetrics {
        rows_scanned: metrics.rows_scanned,
        files_parsed: metrics.files_parsed,
        streaming_buffer_peak_bytes: metrics.streaming_buffer_peak_bytes,
        projected_files: metrics.projected_files,
    })
}

fn write_corpus(
    path: &std::path::Path,
    scale: usize,
    json_shape: &str,
    format: BenchFormat,
) -> Result<(usize, u64)> {
    match format {
        BenchFormat::Atif => write_atif_corpus(path, scale, json_shape),
        BenchFormat::Actf => write_actf_corpus(path, scale, json_shape),
    }
}

fn write_atif_corpus(
    path: &std::path::Path,
    scale: usize,
    json_shape: &str,
) -> Result<(usize, u64)> {
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

fn write_actf_corpus(
    path: &std::path::Path,
    scale: usize,
    json_shape: &str,
) -> Result<(usize, u64)> {
    let fixture_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/import_roundtrip");
    let fixture_names = [
        "protein-assembly_trimmed.actf.json",
        "make-doom-for-mips_trimmed.actf.json",
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
            let mut document = fixture.clone();
            let task_id = format!("stream-{copy:06}-{index:02}");
            document["task_id"] = serde_json::Value::String(task_id);
            steps += document["attempts"]
                .as_object()
                .and_then(|attempts| attempts.values().next())
                .and_then(|attempt| attempt.get("trajectory"))
                .and_then(|trajectory| trajectory.get("steps"))
                .and_then(|steps| steps.as_array())
                .map_or(0, Vec::len) as u64;
            if array && !first {
                output.write_all(b",\n")?;
            }
            serde_json::to_writer(&mut output, &document)?;
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
