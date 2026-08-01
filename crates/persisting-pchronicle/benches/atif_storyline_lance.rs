//! Lightweight, dependency-free conversion and storage-size benchmark.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use persisting_pchronicle::{
    from_storyline, into_storyline, ChronicleFormat, LanceStorylineStore, StorylineDataSource,
    StorylineDataSourceOptions, StorylineDocument,
};

fn main() -> Result<()> {
    let fixtures = load_fixtures()?;
    let iterations = std::env::var("PCHRONICLE_BENCH_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);

    let parse_started = Instant::now();
    for _ in 0..iterations {
        for (_, raw) in &fixtures {
            black_box(into_storyline(ChronicleFormat::Atif, black_box(raw))?);
        }
    }
    let parse_elapsed = parse_started.elapsed();

    let stories = fixtures
        .iter()
        .map(|(_, raw)| into_storyline(ChronicleFormat::Atif, raw))
        .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
    let conversion_started = Instant::now();
    for _ in 0..iterations {
        for story in &stories {
            let atif = from_storyline(ChronicleFormat::Atif, black_box(story))?;
            let reparsed = into_storyline(ChronicleFormat::Atif, black_box(&atif))?;
            black_box(reparsed);
        }
    }
    let conversion_elapsed = conversion_started.elapsed();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let storage = runtime.block_on(storage_benchmark(&stories, iterations))?;
    let operations = (iterations * fixtures.len()) as f64;
    println!(
        "ATIF corpus: {} documents, {} total steps",
        fixtures.len(),
        stories.iter().map(|story| story.turns.len()).sum::<usize>()
    );
    println!(
        "parse: {:?} total, {:.1} documents/s",
        parse_elapsed,
        operations / parse_elapsed.as_secs_f64()
    );
    println!(
        "ATIF -> Storyline -> ATIF -> Storyline: {:?} total, {:.1} documents/s",
        conversion_elapsed,
        operations / conversion_elapsed.as_secs_f64()
    );
    println!(
        "three-table writes: {:?} for {} documents",
        storage.write_elapsed,
        stories.len()
    );
    println!(
        "indexed DataFusion filter: {:?} total, {:.1} queries/s",
        storage.query_elapsed,
        iterations as f64 / storage.query_elapsed.as_secs_f64()
    );
    println!(
        "full-scan DataFusion filter: {:?} total, {:.1} queries/s",
        storage.full_scan_query_elapsed,
        iterations as f64 / storage.full_scan_query_elapsed.as_secs_f64()
    );
    println!(
        "indexed/full-scan speedup: {:.3}x",
        storage.full_scan_query_elapsed.as_secs_f64() / storage.query_elapsed.as_secs_f64()
    );
    println!("raw ATIF JSON: {} bytes", storage.raw_bytes);
    println!("current Lance generation: {} bytes", storage.lance_bytes);
    println!(
        "Lance/raw size ratio: {:.4}",
        storage.lance_bytes as f64 / storage.raw_bytes as f64
    );
    Ok(())
}

struct StorageResult {
    write_elapsed: std::time::Duration,
    query_elapsed: std::time::Duration,
    full_scan_query_elapsed: std::time::Duration,
    raw_bytes: u64,
    lance_bytes: u64,
}

async fn storage_benchmark(
    stories: &[StorylineDocument],
    query_iterations: usize,
) -> Result<StorageResult> {
    let dir = tempfile::tempdir()?;
    let store = LanceStorylineStore::open(dir.path()).await?;
    let started = Instant::now();
    for story in stories {
        store.replace_storyline(story).await?;
    }
    let write_elapsed = started.elapsed();
    let indexed_context = StorylineDataSource::from_store(&store)
        .await?
        .session_context()?;
    let full_scan_context = StorylineDataSource::from_store_with_options(
        &store,
        StorylineDataSourceOptions {
            use_scalar_indexes: false,
            ..Default::default()
        },
    )
    .await?
    .session_context()?;
    let sql = "SELECT step_id, source, message_json FROM steps \
               WHERE session_id = 'fixture-long_context_20' AND step_id BETWEEN 5 AND 15 \
               ORDER BY step_id";
    let indexed_query = indexed_context.sql(sql).await?;
    let full_scan_query = full_scan_context.sql(sql).await?;
    // Warm both plans before timing to reduce one-time cache bias.
    black_box(indexed_query.clone().collect().await?);
    black_box(full_scan_query.clone().collect().await?);
    let query_elapsed = time_query(&indexed_query, query_iterations).await?;
    let full_scan_query_elapsed = time_query(&full_scan_query, query_iterations).await?;
    let paths = store.current_table_paths().await?.unwrap();
    let lance_bytes = directory_size(&paths.runs)?
        + directory_size(&paths.steps)?
        + directory_size(&paths.tool_calls)?;
    let raw_bytes = fixture_paths()?
        .iter()
        .map(std::fs::metadata)
        .collect::<std::io::Result<Vec<_>>>()?
        .iter()
        .map(std::fs::Metadata::len)
        .sum();
    Ok(StorageResult {
        write_elapsed,
        query_elapsed,
        full_scan_query_elapsed,
        raw_bytes,
        lance_bytes,
    })
}

async fn time_query(
    query: &datafusion::dataframe::DataFrame,
    iterations: usize,
) -> Result<std::time::Duration> {
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(query.clone().collect().await?);
    }
    Ok(started.elapsed())
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif")
}

fn fixture_paths() -> Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(fixture_root())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();
    Ok(paths)
}

fn load_fixtures() -> Result<Vec<(PathBuf, String)>> {
    fixture_paths()?
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            Ok((path, raw))
        })
        .collect()
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
