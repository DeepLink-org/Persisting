//! Criterion microbenchmarks for pChronicle's CPU-bound interchange path.
//!
//! Storage, query, RSS, and end-to-end lifecycle scenarios stay in the custom
//! harnesses and are executed through hyperfine by `benchmark/pchronicle/bench.py`.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines, project_event_records,
    storyline_to_events,
};
use persisting_pchronicle::model::StorylineDocument;

fn parse_atif(raw: &str) -> StorylineDocument {
    decode_json_storylines(DocumentFormat::Atif, raw, "benchmark.json")
        .expect("parse benchmark ATIF")
        .pop()
        .expect("benchmark fixture contains one Storyline")
}

fn conversion_benchmarks(criterion: &mut Criterion) {
    let fixtures = load_fixtures();
    let stories = fixtures
        .iter()
        .map(|raw| parse_atif(raw))
        .collect::<Vec<_>>();
    let documents = fixtures.len() as u64;

    let mut group = criterion.benchmark_group("atif_conversion");
    group.throughput(Throughput::Elements(documents));
    group.bench_function("parse_corpus", |bencher| {
        bencher.iter(|| {
            for raw in &fixtures {
                black_box(parse_atif(black_box(raw)));
            }
        });
    });
    group.bench_function("roundtrip_corpus", |bencher| {
        bencher.iter(|| {
            for story in &stories {
                let atif = serde_json::to_string(
                    &encode_json_storylines(
                        DocumentFormat::Atif,
                        std::slice::from_ref(black_box(story)),
                    )
                    .expect("encode benchmark Storyline"),
                )
                .expect("serialize benchmark ATIF");
                black_box(parse_atif(black_box(&atif)));
            }
        });
    });
    group.finish();
}

fn projection_cpu_benchmarks(criterion: &mut Criterion) {
    let fixtures = load_fixtures();
    let stories = fixtures
        .iter()
        .map(|raw| parse_atif(raw))
        .collect::<Vec<_>>();
    let events = stories
        .iter()
        .map(|story| storyline_to_events(story).expect("project fixture to canonical events"))
        .collect::<Vec<_>>();
    let mut group = criterion.benchmark_group("projection_cpu");
    group.throughput(Throughput::Elements(stories.len() as u64));
    group.bench_function("events_to_storyline_corpus", |bencher| {
        bencher.iter(|| {
            for document in &events {
                black_box(
                    project_event_records(black_box(&document.events))
                        .expect("project canonical benchmark events"),
                );
            }
        });
    });
    group.finish();
}

fn load_fixtures() -> Vec<String> {
    fixture_paths()
        .into_iter()
        .map(|path| std::fs::read_to_string(&path).expect("read ATIF benchmark fixture"))
        .collect()
}

fn fixture_paths() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif");
    let mut paths = std::fs::read_dir(root)
        .expect("read ATIF benchmark fixture directory")
        .map(|entry| entry.expect("read ATIF benchmark fixture entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn criterion_config() -> Criterion {
    let sample_size = env_usize("PCHRONICLE_CRITERION_SAMPLES", 30).max(10);
    let measurement_ms = env_u64("PCHRONICLE_CRITERION_MEASUREMENT_MS", 3_000).max(100);
    let warmup_ms = env_u64("PCHRONICLE_CRITERION_WARMUP_MS", 1_000).max(100);
    Criterion::default()
        .sample_size(sample_size)
        .measurement_time(Duration::from_millis(measurement_ms))
        .warm_up_time(Duration::from_millis(warmup_ms))
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = conversion_benchmarks, projection_cpu_benchmarks
}
criterion_main!(benches);
