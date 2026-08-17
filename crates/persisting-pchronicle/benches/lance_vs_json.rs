// Compare indexed Lance/DataFusion queries with JSON scan and in-memory JSON baselines.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use persisting_pchronicle::{
    from_storyline, into_storyline, AtifTrajectory, ChronicleFormat, FileTrajectoryDataSource,
    StorylineDataSource, StorylineDocument, StorylineLanceStore,
};

const ANALYTICAL_SQL: &str =
    "SELECT source, COUNT(*) AS step_count FROM steps GROUP BY source ORDER BY source";

fn main() -> Result<()> {
    let scale = env_usize("PCHRONICLE_BENCH_SCALE", 128);
    let iterations = env_usize("PCHRONICLE_BENCH_ITERS", 30);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(scale, iterations))?;

    println!(
        "dataset: {} documents, {} steps, scale={}x",
        result.documents, result.steps, scale
    );
    println!(
        "storage: JSON={} bytes, Lance store={} bytes ({:.3}x)",
        result.json_bytes,
        result.lance_bytes,
        result.lance_bytes as f64 / result.json_bytes as f64
    );
    println!(
        "build/open: JSON write {:?}, ATIF datasource {:?}, Lance+indexes {:?}, Lance datasource {:?}",
        result.json_write, result.atif_open, result.lance_write, result.lance_open
    );
    let cold_query_mean = mean_duration(result.lance_cold_query, result.iterations);
    let get_storyline_full_mean = mean_duration(result.get_storyline_full, result.iterations);
    println!("pChronicle lifecycle:");
    println!(
        "  cold open+plan+query: {:?} total ({:?}/query)",
        result.lance_cold_query, cold_query_mean
    );
    println!(
        "  get_storyline_full:  {:?} total ({:?}/lookup)",
        result.get_storyline_full, get_storyline_full_mean
    );
    println!("  single-story replace: {:?}", result.incremental_replace);
    println!(
        "RESULT benchmark=lifecycle iterations={} cold_query_ms={:.3} get_storyline_full_ms={:.3} replace_storyline_ms={:.3}",
        result.iterations,
        milliseconds(cold_query_mean),
        milliseconds(get_storyline_full_mean),
        milliseconds(result.incremental_replace),
    );
    print_comparison(
        "selective",
        "selective session+step query",
        iterations,
        &result.selective,
    );
    print_comparison(
        "group_by",
        "GROUP BY source analysis",
        iterations,
        &result.analytical,
    );
    print_conclusion(&result);
    Ok(())
}

struct Comparison {
    lance: Duration,
    atif_file: Duration,
    json_scan: Duration,
    json_memory: Duration,
}

struct BenchmarkResult {
    iterations: usize,
    documents: usize,
    steps: usize,
    json_bytes: u64,
    lance_bytes: u64,
    json_write: Duration,
    atif_open: Duration,
    lance_write: Duration,
    lance_open: Duration,
    lance_cold_query: Duration,
    get_storyline_full: Duration,
    incremental_replace: Duration,
    selective: Comparison,
    analytical: Comparison,
}

async fn run(scale: usize, iterations: usize) -> Result<BenchmarkResult> {
    anyhow::ensure!(
        scale > 0,
        "PCHRONICLE_BENCH_SCALE must be greater than zero"
    );
    anyhow::ensure!(
        iterations > 0,
        "PCHRONICLE_BENCH_ITERS must be greater than zero"
    );
    let stories = if let Some(input) = std::env::var_os("PCHRONICLE_BENCH_ATIF_INPUT") {
        load_atif_stories(Path::new(&input))?
    } else {
        let base = load_base_stories()?;
        expand_stories(&base, scale)
    };
    let documents = stories.len();
    let steps = stories.iter().map(|story| story.turns.len()).sum();
    let atif_lines = stories
        .iter()
        .map(|story| {
            let pretty = from_storyline(ChronicleFormat::Atif, story)?;
            let value: serde_json::Value = serde_json::from_str(&pretty)?;
            Ok(serde_json::to_string(&value)?)
        })
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
    let json_document = format!("{}\n", atif_lines.join("\n"));
    let dir = tempfile::tempdir()?;
    let json_path = dir.path().join("trajectories.ndjson");
    let json_write_started = Instant::now();
    std::fs::write(&json_path, json_document.as_bytes())?;
    let json_write = json_write_started.elapsed();
    let atif_open_started = Instant::now();
    let atif_source = FileTrajectoryDataSource::open_atif(&json_path)?;
    let atif_open = atif_open_started.elapsed();
    let atif_context = atif_source.session_context()?;
    let parsed = atif_lines
        .iter()
        .map(|line| AtifTrajectory::from_json_str(line))
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;

    let store = StorylineLanceStore::open(dir.path().join("storyline-lance")).await?;
    let lance_write_started = Instant::now();
    store.replace_storylines(&stories).await?;
    let lance_write = lance_write_started.elapsed();
    let lance_open_started = Instant::now();
    let source = StorylineDataSource::from_store(&store).await?;
    let lance_open = lance_open_started.elapsed();
    let context = source.session_context()?;
    // Pick the last longest Storyline so the in-memory JSON baseline cannot win
    // by finding the target in the first few documents.
    let target_session = stories
        .iter()
        .enumerate()
        .max_by_key(|(index, story)| (story.turns.len(), *index))
        .map(|(_, story)| story.session_id.clone())
        .context("benchmark corpus is empty")?;
    let selective_sql = format!(
        "SELECT step_id, source, message_json FROM steps \
         WHERE session_id = '{target_session}' AND step_id BETWEEN 5 AND 15 \
         ORDER BY step_id"
    );
    let selective_query = context.sql(&selective_sql).await?;
    let atif_selective_query = atif_context.sql(&selective_sql).await?;
    let analytical_query = context.sql(ANALYTICAL_SQL).await?;
    let atif_analytical_query = atif_context.sql(ANALYTICAL_SQL).await?;

    // Warm filesystem, Lance metadata and execution plans, and verify every
    // implementation computes the same result before timing it.
    let json_selective = json_selective_query(&json_path, &target_session)?;
    let memory_selective = json_memory_selective(&parsed, &target_session);
    let lance_selective = selective_query.clone().collect().await?;
    let lance_selective = lance_selective
        .iter()
        .map(|batch| batch.num_rows())
        .sum::<usize>();
    anyhow::ensure!(json_selective == 11 && memory_selective == 11);
    anyhow::ensure!(lance_selective == json_selective);
    let atif_selective = atif_selective_query.clone().collect().await?;
    anyhow::ensure!(
        atif_selective
            .iter()
            .map(|batch| batch.num_rows())
            .sum::<usize>()
            == json_selective
    );

    let json_counts = json_analysis(&json_path)?;
    let memory_counts = json_memory_analysis(&parsed);
    let lance_counts = datafusion_counts(analytical_query.clone().collect().await?)?;
    let atif_counts = datafusion_counts(atif_analytical_query.clone().collect().await?)?;
    anyhow::ensure!(
        json_counts == memory_counts && lance_counts == json_counts && atif_counts == json_counts
    );

    let selective = Comparison {
        lance: time_async_query(&selective_query, iterations).await?,
        atif_file: time_async_query(&atif_selective_query, iterations).await?,
        json_scan: time_sync(iterations, || {
            json_selective_query(&json_path, &target_session)
        })?,
        json_memory: time_sync(iterations, || {
            Ok(json_memory_selective(&parsed, &target_session))
        })?,
    };
    let analytical = Comparison {
        lance: time_async_query(&analytical_query, iterations).await?,
        atif_file: time_async_query(&atif_analytical_query, iterations).await?,
        json_scan: time_sync(iterations, || json_analysis(&json_path))?,
        json_memory: time_sync(iterations, || Ok(json_memory_analysis(&parsed)))?,
    };
    let lance_cold_query = time_lance_cold_query(&store, &selective_sql, iterations).await?;
    let get_storyline_full_started = Instant::now();
    for _ in 0..iterations {
        black_box(store.get_storyline_full(&target_session).await?);
    }
    let get_storyline_full = get_storyline_full_started.elapsed();
    let lance_bytes = directory_size(store.root())?;
    let mut updated = stories
        .iter()
        .find(|story| story.session_id == target_session)
        .context("benchmark target Storyline is missing")?
        .clone();
    updated.notes = Some("incremental benchmark replacement".into());
    let incremental_replace_started = Instant::now();
    store.replace_storyline(&updated).await?;
    let incremental_replace = incremental_replace_started.elapsed();
    Ok(BenchmarkResult {
        iterations,
        documents,
        steps,
        json_bytes: std::fs::metadata(json_path)?.len(),
        lance_bytes,
        json_write,
        atif_open,
        lance_write,
        lance_open,
        lance_cold_query,
        get_storyline_full,
        incremental_replace,
        selective,
        analytical,
    })
}

fn mean_duration(total: Duration, iterations: usize) -> Duration {
    total.div_f64(iterations as f64)
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

async fn time_lance_cold_query(
    store: &StorylineLanceStore,
    sql: &str,
    iterations: usize,
) -> Result<Duration> {
    let started = Instant::now();
    for _ in 0..iterations {
        let source = StorylineDataSource::from_store(store).await?;
        let context = source.session_context()?;
        black_box(context.sql(sql).await?.collect().await?);
    }
    Ok(started.elapsed())
}

fn load_base_stories() -> Result<Vec<StorylineDocument>> {
    let mut paths =
        std::fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            into_storyline(ChronicleFormat::Atif, &raw).map_err(anyhow::Error::from)
        })
        .collect()
}

fn load_atif_stories(path: &Path) -> Result<Vec<StorylineDocument>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read benchmark ATIF input {}", path.display()))?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| into_storyline(ChronicleFormat::Atif, line).map_err(anyhow::Error::from))
        .collect()
}

fn expand_stories(base: &[StorylineDocument], scale: usize) -> Vec<StorylineDocument> {
    let mut stories = Vec::with_capacity(base.len() * scale);
    for replica in 0..scale {
        for story in base {
            let mut story = story.clone();
            let suffix = story
                .session_id
                .strip_prefix("fixture-")
                .unwrap_or(&story.session_id)
                .to_string();
            story.session_id = format!("bench-{replica:04}-{suffix}");
            story.run_id = Some(format!("run-bench-{replica:04}-{suffix}"));
            stories.push(story);
        }
    }
    stories
}

fn json_selective_query(path: &Path, target_session: &str) -> Result<usize> {
    let raw = std::fs::read_to_string(path)?;
    let mut matches = 0;
    for line in raw.lines() {
        let trajectory = AtifTrajectory::from_json_str(line)?;
        if trajectory.session_id.as_deref() == Some(target_session) {
            matches += trajectory
                .steps
                .iter()
                .filter(|step| (5..=15).contains(&step.step_id))
                .count();
        }
    }
    Ok(matches)
}

fn json_memory_selective(trajectories: &[AtifTrajectory], target_session: &str) -> usize {
    trajectories
        .iter()
        .find(|trajectory| trajectory.session_id.as_deref() == Some(target_session))
        .map(|trajectory| {
            trajectory
                .steps
                .iter()
                .filter(|step| (5..=15).contains(&step.step_id))
                .count()
        })
        .unwrap_or_default()
}

fn json_analysis(path: &Path) -> Result<BTreeMap<String, usize>> {
    let raw = std::fs::read_to_string(path)?;
    let mut counts = BTreeMap::new();
    for line in raw.lines() {
        let trajectory = AtifTrajectory::from_json_str(line)?;
        for step in trajectory.steps {
            *counts.entry(step.source).or_default() += 1;
        }
    }
    Ok(counts)
}

fn json_memory_analysis(trajectories: &[AtifTrajectory]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for trajectory in trajectories {
        for step in &trajectory.steps {
            *counts.entry(step.source.clone()).or_default() += 1;
        }
    }
    counts
}

async fn time_async_query(
    query: &datafusion::dataframe::DataFrame,
    iterations: usize,
) -> Result<Duration> {
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(query.clone().collect().await?);
    }
    Ok(started.elapsed())
}

fn time_sync<T>(iterations: usize, mut operation: impl FnMut() -> Result<T>) -> Result<Duration> {
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(operation()?);
    }
    Ok(started.elapsed())
}

fn print_comparison(id: &str, name: &str, iterations: usize, comparison: &Comparison) {
    let lance_qps = iterations as f64 / comparison.lance.as_secs_f64();
    let atif_qps = iterations as f64 / comparison.atif_file.as_secs_f64();
    let atif_over_lance_time = comparison.atif_file.as_secs_f64() / comparison.lance.as_secs_f64();
    println!("{name}:");
    println!(
        "  Lance/DataFusion indexed: {:?} ({:.1} queries/s)",
        comparison.lance, lance_qps
    );
    println!(
        "  ATIF/DataFusion stream:   {:?} ({:.1} queries/s, Lance speed ratio {:.2}x)",
        comparison.atif_file, atif_qps, atif_over_lance_time
    );
    println!(
        "  JSON read+Serde scan:     {:?} ({:.1} queries/s, Lance {:.2}x faster)",
        comparison.json_scan,
        iterations as f64 / comparison.json_scan.as_secs_f64(),
        comparison.json_scan.as_secs_f64() / comparison.lance.as_secs_f64()
    );
    println!(
        "  parsed JSON native loop:  {:?} ({:.1} queries/s, Lance/native time ratio {:.2}x)",
        comparison.json_memory,
        iterations as f64 / comparison.json_memory.as_secs_f64(),
        comparison.lance.as_secs_f64() / comparison.json_memory.as_secs_f64()
    );
    println!(
        "RESULT benchmark={id} iterations={iterations} lance_qps={lance_qps:.1} \
         atif_qps={atif_qps:.1} atif_over_lance_time={atif_over_lance_time:.3}"
    );
}

fn print_conclusion(result: &BenchmarkResult) {
    let lance_over_json = result.lance_bytes as f64 / result.json_bytes as f64;
    let open_speedup = result.atif_open.as_secs_f64() / result.lance_open.as_secs_f64();
    let selective_disk_speedup =
        result.selective.json_scan.as_secs_f64() / result.selective.lance.as_secs_f64();
    let group_disk_speedup =
        result.analytical.json_scan.as_secs_f64() / result.analytical.lance.as_secs_f64();
    let selective_memory_ratio =
        result.selective.atif_file.as_secs_f64() / result.selective.lance.as_secs_f64();
    let group_memory_ratio =
        result.analytical.atif_file.as_secs_f64() / result.analytical.lance.as_secs_f64();
    println!("Conclusion:");
    println!(
        "  Storage: Lance uses {:.2}% of JSON space, saving {:.2}%.",
        lance_over_json * 100.0,
        (1.0 - lance_over_json) * 100.0
    );
    println!(
        "  Open: Lance datasource open is {open_speedup:.2}x faster than ATIF streaming validation/count scan."
    );
    println!(
        "  On-disk query: Lance is {selective_disk_speedup:.2}x faster for the selective query and {group_disk_speedup:.2}x faster for GROUP BY than JSON read+Serde."
    );
    println!(
        "  Streaming boundary: ATIF stream/Lance time ratio is {selective_memory_ratio:.2}x for the selective query and {group_memory_ratio:.2}x for GROUP BY."
    );
    println!(
        "  Lifecycle: cold query {:.3} ms, point lookup {:.3} ms, single-story replace {:.3} ms.",
        milliseconds(mean_duration(result.lance_cold_query, result.iterations)),
        milliseconds(mean_duration(result.get_storyline_full, result.iterations,)),
        milliseconds(result.incremental_replace),
    );
    println!(
        "RESULT benchmark=summary lance_over_json={lance_over_json:.4} open_speedup={open_speedup:.2} selective_disk_speedup={selective_disk_speedup:.2} group_disk_speedup={group_disk_speedup:.2} selective_memory_over_lance_time={selective_memory_ratio:.3} group_memory_over_lance_time={group_memory_ratio:.3}"
    );
}

fn datafusion_counts(
    batches: Vec<datafusion::arrow::record_batch::RecordBatch>,
) -> Result<BTreeMap<String, usize>> {
    use datafusion::arrow::array::{Int64Array, StringArray};

    let mut counts = BTreeMap::new();
    for batch in batches {
        let sources = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("DataFusion source column must be Utf8")?;
        let values = batch
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("DataFusion COUNT column must be Int64")?;
        for row in 0..batch.num_rows() {
            counts.insert(sources.value(row).to_string(), values.value(row) as usize);
        }
    }
    Ok(counts)
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            size += directory_size(&entry.path())?;
        } else {
            size += metadata.len();
        }
    }
    Ok(size)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
