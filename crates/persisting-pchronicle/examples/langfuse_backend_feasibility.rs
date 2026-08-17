//! Executable feasibility probe for using pChronicle as Langfuse's analytics backend.
//!
//! This is deliberately an adapter PoC, not a production integration. It keeps
//! Langfuse's public APIs out of scope and makes unsupported mutation semantics
//! explicit instead of emulating them inside pChronicle core.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use persisting_pchronicle::{
    raw_event_lance_path, CatalogSnapshotOptions, ChronicleQueryEngine,
    ChronicleQueryExecutionOptions, DatasetCatalogSnapshot, DatasetMount, DocumentFormat,
    EventIdentity, EventRecord, LanceMaintenanceOptions, RawEventLanceAppender, RawEventLanceStore,
    StoryCoords,
};
use serde::Serialize;
use serde_json::{json, Value};

const DEFAULT_EVENTS: usize = 100_000;
const DEFAULT_SCORES: usize = 10_000;
const DEFAULT_DATASET_RUN_ITEMS: usize = 2_000;
const DEFAULT_BLOB_LOG_ROWS: usize = 1_000;
const DEFAULT_TRACES: usize = 200;
const DEFAULT_PRELOAD_BATCH_ROWS: usize = 5_000;
const DEFAULT_LOAD_SECONDS: usize = 3;
const LOAD_ROWS_PER_SECOND: usize = 10;

#[derive(Debug, Clone, Serialize)]
struct LogicalRow {
    row_kind: String,
    project_id: String,
    trace_id: String,
    span_id: String,
    parent_span_id: String,
    logical_id: String,
    event_ts: String,
    event_ts_ms: u64,
    start_time: String,
    name: String,
    event_type: String,
    session_id: String,
    user_id: String,
    environment: String,
    tags: Vec<String>,
    metadata_names: Vec<String>,
    metadata_values: Vec<String>,
    tool_names: Vec<String>,
    model: String,
    input: String,
    output: String,
    usage_input: u64,
    usage_output: u64,
    total_cost: String,
    version: u32,
    is_deleted: bool,
    bookmarked: bool,
    public: bool,
    dataset_id: String,
    dataset_run_id: String,
    storage_run_id: String,
    seq: u64,
}

impl LogicalRow {
    fn to_event_record(&self) -> Result<EventRecord> {
        Ok(EventRecord {
            identity: EventIdentity {
                event_id: Some(self.logical_id.clone()),
                timestamp_unix_ms: Some(self.event_ts_ms),
                ..Default::default()
            },
            seq: self.seq,
            source: "langfuse-backend-feasibility".into(),
            kind: self.row_kind.clone(),
            timestamp: Some(self.event_ts.replace(' ', "T") + "Z"),
            session_id: Some(self.storage_run_id.clone()),
            agent_id: Some(self.project_id.clone()),
            parent_uuid: None,
            trace_id: (!self.trace_id.is_empty()).then(|| self.trace_id.clone()),
            call_id: (!self.span_id.is_empty()).then(|| self.span_id.clone()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: (!self.parent_span_id.is_empty()).then(|| self.parent_span_id.clone()),
            payload: serde_json::to_value(self).context("encode logical Langfuse row")?,
        })
    }

    fn coords(&self, storage: &Path) -> StoryCoords {
        StoryCoords::new(
            storage.to_string_lossy(),
            self.project_id.clone(),
            self.storage_run_id.clone(),
            Some(self.storage_run_id.clone()),
        )
    }
}

#[derive(Debug, Serialize)]
struct AppendReceipt {
    acknowledged_rows: usize,
    persisted_rows: usize,
}

#[derive(Debug, Serialize)]
struct QueryMetric {
    repetitions: usize,
    result_rows: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Serialize)]
struct StreamMetric {
    rows: usize,
    bytes: usize,
    first_byte_ms: Option<f64>,
    elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
struct CapabilityProbe {
    supported: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct BackendHealth {
    physical_rows: u64,
    catalog_sources: usize,
}

#[async_trait(?Send)]
trait LangfuseAnalyticsBackend {
    async fn append(&mut self, rows: &[LogicalRow]) -> Result<AppendReceipt>;
    async fn point(&self, project_id: &str, logical_id: &str) -> Result<Vec<Value>>;
    async fn list(&self, project_id: &str, limit: usize) -> Result<Vec<Value>>;
    async fn aggregate(&self, project_id: &str) -> Result<Vec<Value>>;
    async fn stream(&self, project_id: &str) -> Result<StreamMetric>;
    async fn update_flags(
        &mut self,
        project_id: &str,
        logical_id: &str,
        bookmarked: bool,
        public: bool,
    ) -> Result<()>;
    async fn delete_trace(&mut self, project_id: &str, trace_id: &str) -> Result<()>;
    async fn delete_project(&mut self, project_id: &str) -> Result<()>;
    async fn delete_older_than(&mut self, project_id: &str, cutoff: &str) -> Result<()>;
    async fn flush(&mut self) -> Result<()>;
    async fn health(&self) -> Result<BackendHealth>;
}

struct PChronicleBackend {
    storage: PathBuf,
    writer: Option<RawEventLanceAppender>,
    engine: Option<ChronicleQueryEngine>,
    catalog_sources: usize,
}

impl PChronicleBackend {
    fn new(storage: PathBuf) -> Self {
        Self {
            storage,
            writer: Some(RawEventLanceAppender::default()),
            engine: None,
            catalog_sources: 0,
        }
    }

    fn engine(&self) -> Result<&ChronicleQueryEngine> {
        self.engine
            .as_ref()
            .context("backend catalog has not been flushed/refreshed")
    }

    async fn refresh_catalog(&mut self) -> Result<Duration> {
        let started = Instant::now();
        let snapshot = discover_catalog(&self.storage).await?;
        self.catalog_sources = snapshot
            .datasets()
            .iter()
            .map(|dataset| dataset.ready_source_count())
            .sum();
        self.engine = Some(snapshot.query_engine(Default::default()).await?);
        Ok(started.elapsed())
    }
}

#[async_trait(?Send)]
impl LangfuseAnalyticsBackend for PChronicleBackend {
    async fn append(&mut self, rows: &[LogicalRow]) -> Result<AppendReceipt> {
        let entries = rows
            .iter()
            .map(|row| Ok((row.coords(&self.storage), row.to_event_record()?)))
            .collect::<Result<Vec<_>>>()?;
        let outcome = self
            .writer
            .get_or_insert_with(RawEventLanceAppender::default)
            .append_event_batch(&entries)
            .await?;
        Ok(AppendReceipt {
            acknowledged_rows: outcome.accepted_records,
            persisted_rows: outcome.persisted_units,
        })
    }

    async fn point(&self, project_id: &str, logical_id: &str) -> Result<Vec<Value>> {
        query_values(
            self.engine()?,
            &format!(
                "SELECT event_id, timestamp, kind, trace_id, call_id, payload_json \
                 FROM events WHERE agent_id = {} AND event_id = {} \
                 ORDER BY timestamp DESC LIMIT 1",
                sql_string(project_id),
                sql_string(logical_id)
            ),
        )
        .await
    }

    async fn list(&self, project_id: &str, limit: usize) -> Result<Vec<Value>> {
        query_values(
            self.engine()?,
            &format!(
                "SELECT event_id, timestamp, kind, trace_id, call_id \
                 FROM events WHERE agent_id = {} AND kind = 'event' \
                 ORDER BY timestamp DESC LIMIT {limit}",
                sql_string(project_id)
            ),
        )
        .await
    }

    async fn aggregate(&self, project_id: &str) -> Result<Vec<Value>> {
        query_values(
            self.engine()?,
            &format!(
                "SELECT model, COUNT(*) AS row_count FROM events \
                 WHERE agent_id = {} AND kind = 'event' GROUP BY model ORDER BY model",
                sql_string(project_id)
            ),
        )
        .await
    }

    async fn stream(&self, project_id: &str) -> Result<StreamMetric> {
        let sql = format!(
            "SELECT event_id, timestamp, kind, trace_id, call_id, payload_json \
             FROM events WHERE agent_id = {} AND kind = 'event' ORDER BY timestamp",
            sql_string(project_id)
        );
        let started = Instant::now();
        let mut writer = FirstByteWriter::new(started);
        self.engine()?.write_query_jsonl(&sql, &mut writer).await?;
        Ok(StreamMetric {
            rows: writer.rows,
            bytes: writer.bytes,
            first_byte_ms: writer.first_byte.map(duration_ms),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    async fn update_flags(
        &mut self,
        _project_id: &str,
        _logical_id: &str,
        _bookmarked: bool,
        _public: bool,
    ) -> Result<()> {
        anyhow::bail!(
            "UNSUPPORTED_MUTATION: pChronicle is append-only and has no latest-version patch projection"
        )
    }

    async fn delete_trace(&mut self, _project_id: &str, _trace_id: &str) -> Result<()> {
        anyhow::bail!(
            "UNSUPPORTED_DELETE: pChronicle has no trace tombstone projection or row delete contract"
        )
    }

    async fn delete_project(&mut self, _project_id: &str) -> Result<()> {
        anyhow::bail!(
            "UNSUPPORTED_DELETE: pChronicle has no project deletion contract for a shared catalog"
        )
    }

    async fn delete_older_than(&mut self, _project_id: &str, _cutoff: &str) -> Result<()> {
        anyhow::bail!(
            "UNSUPPORTED_RETENTION: pChronicle vacuum removes unreachable versions, not logical rows by tenant/time"
        )
    }

    async fn flush(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            let _reports = writer.finish();
        }
        self.refresh_catalog().await?;
        Ok(())
    }

    async fn health(&self) -> Result<BackendHealth> {
        let rows = count_query(self.engine()?, "SELECT COUNT(*) AS row_count FROM events").await?;
        Ok(BackendHealth {
            physical_rows: rows,
            catalog_sources: self.catalog_sources,
        })
    }
}

struct FirstByteWriter {
    started: Instant,
    first_byte: Option<Duration>,
    bytes: usize,
    rows: usize,
}

impl FirstByteWriter {
    fn new(started: Instant) -> Self {
        Self {
            started,
            first_byte: None,
            bytes: 0,
            rows: 0,
        }
    }
}

impl Write for FirstByteWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.first_byte.is_none() && !buffer.is_empty() {
            self.first_byte = Some(self.started.elapsed());
        }
        self.bytes += buffer.len();
        self.rows += buffer.iter().filter(|byte| **byte == b'\n').count();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    fs::create_dir_all(&config.workdir)
        .with_context(|| format!("create workdir {}", config.workdir.display()))?;
    let fixture_path = config.workdir.join("logical_rows.jsonl");
    let report_path = config.workdir.join("pchronicle-report.json");
    let storage = config.workdir.join("pchronicle-store");
    anyhow::ensure!(
        !fixture_path.exists() && !report_path.exists() && !storage.exists(),
        "workdir already contains feasibility-probe outputs; use an empty directory"
    );

    let fixture_started = Instant::now();
    let rows = generate_fixture(&config)?;
    let expected_sources = rows
        .iter()
        .map(|row| (row.project_id.clone(), row.storage_run_id.clone()))
        .collect::<BTreeSet<_>>()
        .len();
    write_fixture(&fixture_path, &rows)?;
    let fixture_elapsed = fixture_started.elapsed();

    let mut backend = PChronicleBackend::new(storage.clone());
    let append_started = Instant::now();
    let mut acknowledged = 0usize;
    for batch in rows.chunks(config.preload_batch_rows) {
        let receipt = backend.append(batch).await?;
        anyhow::ensure!(
            receipt.acknowledged_rows == batch.len() && receipt.persisted_rows == batch.len(),
            "append acknowledgement mismatch"
        );
        acknowledged += receipt.acknowledged_rows;
    }
    let append_elapsed = append_started.elapsed();
    anyhow::ensure!(
        acknowledged == rows.len(),
        "not every fixture row was acknowledged"
    );

    let rss_before_catalog = current_rss_bytes();
    let catalog_started = Instant::now();
    backend.flush().await?;
    let catalog_elapsed = catalog_started.elapsed();
    let rss_after_catalog = current_rss_bytes();
    let health = backend.health().await?;
    anyhow::ensure!(
        health.physical_rows == rows.len() as u64,
        "physical row count differs from acknowledged fixture rows"
    );
    anyhow::ensure!(
        health.catalog_sources == expected_sources,
        "catalog source count differs from generated run count"
    );

    let point_id = "project-a-event-00000000";
    let other_project_id = "project-b-event-00000500";
    let point_rows = backend.point("project-a", point_id).await?;
    let cross_project_rows = backend.point("project-a", other_project_id).await?;
    let list_rows = backend.list("project-a", 100).await?;
    let aggregate_rows = backend.aggregate("project-a").await?;
    let stream_metric = backend.stream("project-a").await?;

    let point_sql = format!(
        "SELECT event_id, timestamp, kind FROM events WHERE agent_id = {} AND event_id = {} \
         ORDER BY timestamp DESC LIMIT 1",
        sql_string("project-a"),
        sql_string(point_id)
    );
    let list_sql = format!(
        "SELECT event_id, timestamp, kind, trace_id, call_id FROM events WHERE agent_id = {} \
         AND kind = 'event' ORDER BY timestamp DESC LIMIT 100",
        sql_string("project-a")
    );
    let aggregate_sql = format!(
        "SELECT model, COUNT(*) AS row_count FROM events WHERE agent_id = {} AND kind = 'event' \
         GROUP BY model ORDER BY model",
        sql_string("project-a")
    );
    let point_metric = measure_query(backend.engine()?, &point_sql, 7).await?;
    let list_metric = measure_query(backend.engine()?, &list_sql, 7).await?;
    let aggregate_metric = measure_query(backend.engine()?, &aggregate_sql, 7).await?;
    let rss_after_unpruned_queries = current_rss_bytes();

    let fresh_global_point_setup_started = Instant::now();
    let fresh_global_point_engine = discover_catalog(&storage)
        .await?
        .query_engine(Default::default())
        .await?;
    let fresh_global_point_setup_ms = duration_ms(fresh_global_point_setup_started.elapsed());
    let fresh_global_point_metric =
        measure_query(&fresh_global_point_engine, &point_sql, 7).await?;
    drop(fresh_global_point_engine);

    let fresh_global_list_setup_started = Instant::now();
    let fresh_global_list_engine = discover_catalog(&storage)
        .await?
        .query_engine(Default::default())
        .await?;
    let fresh_global_list_setup_ms = duration_ms(fresh_global_list_setup_started.elapsed());
    let fresh_global_list_metric = measure_query(&fresh_global_list_engine, &list_sql, 7).await?;
    drop(fresh_global_list_engine);

    let point_row = rows
        .iter()
        .find(|row| row.logical_id == point_id)
        .context("fixture contains no point-query row")?;
    let direct_open_started = Instant::now();
    let direct_engine = ChronicleQueryEngine::open(
        DocumentFormat::CanonicalEvent,
        raw_event_lance_path(&point_row.coords(&storage))?,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let direct_open_ms = duration_ms(direct_open_started.elapsed());
    let direct_point_metric = measure_query(
        &direct_engine,
        &format!(
            "SELECT event_id, timestamp, kind, trace_id, call_id, payload_json \
             FROM events WHERE event_id = {} ORDER BY timestamp DESC LIMIT 1",
            sql_string(point_id)
        ),
        7,
    )
    .await?;
    drop(direct_engine);

    let exact_file_setup_started = Instant::now();
    let exact_file_engine = discover_catalog(&storage)
        .await?
        .query_engine(Default::default())
        .await?;
    let exact_file_setup_ms = duration_ms(exact_file_setup_started.elapsed());
    let exact_file = format!(
        "{}/{}/events.lance",
        point_row.project_id, point_row.storage_run_id
    );
    let exact_file_point_metric = measure_query(
        &exact_file_engine,
        &format!(
            "SELECT event_id, timestamp, kind, trace_id, call_id, payload_json \
             FROM events WHERE _file_ = {} AND agent_id = {} AND event_id = {} \
             ORDER BY timestamp DESC LIMIT 1",
            sql_string(&exact_file),
            sql_string(&point_row.project_id),
            sql_string(point_id)
        ),
        7,
    )
    .await?;
    drop(exact_file_engine);

    let project_file_setup_started = Instant::now();
    let project_file_engine = discover_catalog(&storage)
        .await?
        .query_engine(Default::default())
        .await?;
    let project_file_setup_ms = duration_ms(project_file_setup_started.elapsed());
    let project_file_list_metric = measure_query(
        &project_file_engine,
        "SELECT event_id, timestamp, kind, trace_id, call_id FROM events \
         WHERE _file_ LIKE 'project-a/%' AND agent_id = 'project-a' AND kind = 'event' \
         ORDER BY timestamp DESC LIMIT 100",
        7,
    )
    .await?;
    drop(project_file_engine);

    let event_physical_rows = count_query(
        backend.engine()?,
        "SELECT COUNT(*) AS row_count FROM events WHERE kind = 'event'",
    )
    .await?;
    let event_logical_ids = count_query(
        backend.engine()?,
        "SELECT COUNT(DISTINCT event_id) AS row_count FROM events WHERE kind = 'event'",
    )
    .await?;

    let mutation_errors = json!({
        "update_flags": backend.update_flags("project-a", point_id, true, true).await.unwrap_err().to_string(),
        "delete_trace": backend.delete_trace("project-a", "project-a-trace-0000").await.unwrap_err().to_string(),
        "delete_project": backend.delete_project("project-a").await.unwrap_err().to_string(),
        "delete_retention": backend.delete_older_than("project-a", "2026-01-01 00:00:00.000").await.unwrap_err().to_string(),
    });

    let capability_probes = run_capability_probes(backend.engine()?).await;

    let base_count = health.physical_rows;
    let pinned_snapshot = discover_catalog(&storage).await?;
    let pinned_engine = pinned_snapshot.query_engine(Default::default()).await?;
    let load_rows = generate_load_rows(&config);
    let load_query = async {
        let mut query_samples = Vec::new();
        let iterations = config.load_seconds.saturating_mul(2).max(1);
        for _ in 0..iterations {
            let started = Instant::now();
            let count = count_query(
                &pinned_engine,
                "SELECT COUNT(*) AS row_count FROM events WHERE agent_id = 'project-a'",
            )
            .await?;
            query_samples.push(duration_ms(started.elapsed()));
            tokio::time::sleep(Duration::from_millis(500)).await;
            anyhow::ensure!(count > 0, "concurrent pinned query returned no rows");
        }
        Result::<Vec<f64>>::Ok(query_samples)
    };
    let load_append = async {
        let phase_started = Instant::now();
        let mut ack_samples = Vec::new();
        let mut visibility_samples = Vec::new();
        for (index, row) in load_rows.iter().enumerate() {
            let target = phase_started + Duration::from_millis((index as u64) * 100);
            if Instant::now() < target {
                tokio::time::sleep_until(tokio::time::Instant::from_std(target)).await;
            }
            let started = Instant::now();
            let receipt = backend.append(std::slice::from_ref(row)).await?;
            anyhow::ensure!(
                receipt.acknowledged_rows == 1,
                "load append was not acknowledged"
            );
            ack_samples.push(duration_ms(started.elapsed()));
            let visibility_started = Instant::now();
            let path = raw_event_lance_path(&row.coords(&storage))?;
            let fresh = ChronicleQueryEngine::open(
                DocumentFormat::CanonicalEvent,
                path,
                ChronicleQueryExecutionOptions::default(),
            )
            .await?;
            let visible = count_query(&fresh, "SELECT COUNT(*) AS row_count FROM events").await?;
            anyhow::ensure!(
                visible == (index + 1) as u64,
                "acknowledged load row {index} is not visible"
            );
            visibility_samples.push(duration_ms(visibility_started.elapsed()));
        }
        Result::<(Vec<f64>, Vec<f64>, Duration)>::Ok((
            ack_samples,
            visibility_samples,
            phase_started.elapsed(),
        ))
    };
    let (query_samples, load_result) = tokio::join!(load_query, load_append);
    let query_samples = query_samples?;
    let (load_ack_samples, load_visibility_samples, load_elapsed) = load_result?;
    let pinned_count_after_append =
        count_query(&pinned_engine, "SELECT COUNT(*) AS row_count FROM events").await?;
    backend.flush().await?;
    let refreshed_count = backend.health().await?.physical_rows;
    anyhow::ensure!(
        pinned_count_after_append == base_count,
        "pinned catalog snapshot changed while append continued"
    );
    anyhow::ensure!(
        refreshed_count == base_count + load_rows.len() as u64,
        "catalog refresh did not expose every acknowledged load row"
    );

    let maintained_row = rows
        .iter()
        .find(|row| row.row_kind == "event")
        .context("fixture contains no event row")?;
    let maintenance_session = maintained_row.coords(&storage);
    let maintenance_started = Instant::now();
    let maintenance_report = RawEventLanceStore
        .maintain(
            &maintenance_session,
            &LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            },
        )
        .await?;
    let maintenance_elapsed = maintenance_started.elapsed();
    let mut restart_row = maintained_row.clone();
    restart_row.logical_id = "project-a-after-maintenance".into();
    restart_row.event_ts_ms += 10_000_000;
    restart_row.event_ts = timestamp_string(restart_row.event_ts_ms);
    restart_row.start_time = restart_row.event_ts.clone();
    restart_row.seq += 1_000_000;
    let restart_started = Instant::now();
    backend.append(std::slice::from_ref(&restart_row)).await?;
    backend.flush().await?;
    let restart_append_and_catalog_elapsed = restart_started.elapsed();
    let restart_visibility_started = Instant::now();
    let restart_visible = backend
        .point("project-a", "project-a-after-maintenance")
        .await?;
    let restart_visibility_elapsed = restart_visibility_started.elapsed();
    let restart_end_to_end_elapsed = restart_started.elapsed();

    let report = json!({
        "probe": "langfuse-clickhouse-to-pchronicle-feasibility",
        "status": "experimental",
        "config": {
            "events": config.events,
            "scores": config.scores,
            "dataset_run_items": config.dataset_run_items,
            "blob_log_rows": config.blob_log_rows,
            "traces": config.traces,
            "preload_batch_rows": config.preload_batch_rows,
            "load_rows_per_second": LOAD_ROWS_PER_SECOND,
            "load_seconds": config.load_seconds,
        },
        "artifacts": {
            "fixture": fixture_path,
            "storage": storage,
        },
        "fixture": {
            "rows": rows.len(),
            "expected_sources": expected_sources,
            "generation_ms": duration_ms(fixture_elapsed),
            "jsonl_bytes": fs::metadata(&fixture_path)?.len(),
        },
        "append": {
            "acknowledged_rows": acknowledged,
            "elapsed_ms": duration_ms(append_elapsed),
            "rows_per_second": rows.len() as f64 / append_elapsed.as_secs_f64(),
            "zero_acknowledged_loss": acknowledged as u64 == health.physical_rows,
        },
        "catalog": {
            "cold_start_ms": duration_ms(catalog_elapsed),
            "sources": health.catalog_sources,
            "rss_before_bytes": rss_before_catalog,
            "rss_after_bytes": rss_after_catalog,
            "rss_after_unpruned_queries_bytes": rss_after_unpruned_queries,
        },
        "catalog_pruning": {
            "fresh_global_point_catalog_setup_ms": fresh_global_point_setup_ms,
            "fresh_global_point": fresh_global_point_metric,
            "fresh_global_list_catalog_setup_ms": fresh_global_list_setup_ms,
            "fresh_global_list": fresh_global_list_metric,
            "direct_run_open_ms": direct_open_ms,
            "direct_run_point": direct_point_metric,
            "exact_file_catalog_setup_ms": exact_file_setup_ms,
            "exact_file_point": exact_file_point_metric,
            "project_file_catalog_setup_ms": project_file_setup_ms,
            "project_file_list": project_file_list_metric,
        },
        "semantics": {
            "point_rows": point_rows.len(),
            "list_rows": list_rows.len(),
            "aggregate_rows": aggregate_rows,
            "cross_project_point_rows": cross_project_rows.len(),
            "event_physical_rows": event_physical_rows,
            "event_distinct_logical_ids": event_logical_ids,
            "duplicate_versions_remain_physical": event_physical_rows > event_logical_ids,
            "mutations": mutation_errors,
            "json_export": stream_metric,
            "parquet_export": {
                "supported": false,
                "reason": "ChronicleQueryEngine exposes JSONL streaming only"
            },
        },
        "query_latency": {
            "point": point_metric,
            "list": list_metric,
            "aggregate": aggregate_metric,
        },
        "sql_compatibility": capability_probes,
        "load_phase": {
            "rows": load_rows.len(),
            "elapsed_ms": duration_ms(load_elapsed),
            "effective_rows_per_second": load_rows.len() as f64 / load_elapsed.as_secs_f64(),
            "ack_p50_ms": percentile(&load_ack_samples, 0.50),
            "ack_p95_ms": percentile(&load_ack_samples, 0.95),
            "visibility_p50_ms": percentile(&load_visibility_samples, 0.50),
            "visibility_p95_ms": percentile(&load_visibility_samples, 0.95),
            "visible_rows": refreshed_count - base_count,
            "zero_acknowledged_loss": refreshed_count == base_count + load_rows.len() as u64,
            "concurrent_query_p95_ms": percentile(&query_samples, 0.95),
            "pinned_count_after_append": pinned_count_after_append,
            "refreshed_count": refreshed_count,
        },
        "maintenance_restart": {
            "maintenance_ms": duration_ms(maintenance_elapsed),
            "fragments_removed": maintenance_report.fragments_removed,
            "restart_append_and_catalog_ms": duration_ms(restart_append_and_catalog_elapsed),
            "restart_visibility_ms": duration_ms(restart_visibility_elapsed),
            "restart_end_to_end_ms": duration_ms(restart_end_to_end_elapsed),
            "restart_row_visible": restart_visible.len() == 1,
        },
        "hard_failures": [
            "bookmarked/public updates require a mutable latest-version projection",
            "trace/project/retention deletes have no logical tombstone contract",
            "ClickHouse SQL and aggregate functions are not source-compatible with DataFusion",
            "Parquet export is absent from the public query adapter",
            "project-wide queries still resolve every per-Run source unless the adapter supplies catalog-owned _file_ pruning"
        ],
        "bounded_integration_verdict": "NO-GO",
    });
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("write report {}", report_path.display()))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[derive(Debug)]
struct Config {
    workdir: PathBuf,
    events: usize,
    scores: usize,
    dataset_run_items: usize,
    blob_log_rows: usize,
    traces: usize,
    preload_batch_rows: usize,
    load_seconds: usize,
}

impl Config {
    fn from_env() -> Result<Self> {
        let workdir = std::env::var("PCHRONICLE_LANGFUSE_WORKDIR")
            .map(PathBuf::from)
            .context("PCHRONICLE_LANGFUSE_WORKDIR must point to an empty output directory")?;
        let events = positive_env("PCHRONICLE_LANGFUSE_EVENTS", DEFAULT_EVENTS)?;
        let traces = positive_env("PCHRONICLE_LANGFUSE_TRACES", DEFAULT_TRACES)?;
        anyhow::ensure!(traces <= events, "trace count cannot exceed event count");
        Ok(Self {
            workdir,
            events,
            scores: positive_env("PCHRONICLE_LANGFUSE_SCORES", DEFAULT_SCORES)?,
            dataset_run_items: positive_env(
                "PCHRONICLE_LANGFUSE_DATASET_RUN_ITEMS",
                DEFAULT_DATASET_RUN_ITEMS,
            )?,
            blob_log_rows: positive_env(
                "PCHRONICLE_LANGFUSE_BLOB_LOG_ROWS",
                DEFAULT_BLOB_LOG_ROWS,
            )?,
            traces,
            preload_batch_rows: positive_env(
                "PCHRONICLE_LANGFUSE_PRELOAD_BATCH_ROWS",
                DEFAULT_PRELOAD_BATCH_ROWS,
            )?,
            load_seconds: positive_env("PCHRONICLE_LANGFUSE_LOAD_SECONDS", DEFAULT_LOAD_SECONDS)?,
        })
    }
}

fn positive_env(name: &str, default: usize) -> Result<usize> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("{name} must be a positive integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("read {name}")),
    };
    anyhow::ensure!(value > 0, "{name} must be greater than zero");
    Ok(value)
}

fn generate_fixture(config: &Config) -> Result<Vec<LogicalRow>> {
    let total = config
        .events
        .checked_add(config.scores)
        .and_then(|value| value.checked_add(config.dataset_run_items))
        .and_then(|value| value.checked_add(config.blob_log_rows))
        .context("fixture row count overflow")?;
    let mut rows = Vec::with_capacity(total);
    let base_ms = 1_767_225_600_000u64;

    for index in 0..config.events {
        let trace_index = index.saturating_mul(config.traces) / config.events;
        let project = project_for_trace(trace_index);
        let trace_id = trace_id(project, trace_index / 2);
        let trace_first = trace_index.saturating_mul(config.events) / config.traces;
        let duplicate_base = if index > 0 && index % 997 == 0 {
            index - 1
        } else {
            index
        };
        let logical_id = format!("{project}-event-{duplicate_base:08}");
        let span_id = format!("{project}-span-{duplicate_base:08}");
        let event_ts_ms = base_ms + index as u64;
        let large_input = index % 1_000 == 0;
        let input = if large_input {
            format!("needle-token {}", "x".repeat(65_536))
        } else if index % 257 == 0 {
            format!("needle-token prompt-{index}")
        } else {
            format!("prompt-{index}")
        };
        rows.push(LogicalRow {
            row_kind: "event".into(),
            project_id: project.into(),
            trace_id: trace_id.clone(),
            span_id,
            parent_span_id: if index == trace_first {
                String::new()
            } else {
                format!("{project}-span-{trace_first:08}")
            },
            logical_id,
            event_ts: timestamp_string(event_ts_ms),
            event_ts_ms,
            start_time: timestamp_string(base_ms + trace_first as u64),
            name: format!("operation-{}", index % 17),
            event_type: if index == trace_first {
                "TRACE".into()
            } else {
                ["SPAN", "GENERATION", "EVENT"][index % 3].into()
            },
            session_id: format!("session-{}", trace_index % 31),
            user_id: format!("user-{}", trace_index % 101),
            environment: ["default", "staging", "production"][trace_index % 3].into(),
            tags: vec![
                format!("tag-{}", index % 11),
                format!("team-{}", trace_index % 7),
            ],
            metadata_names: vec!["region".into(), "request.class".into()],
            metadata_values: vec![
                ["cn", "eu", "us"][trace_index % 3].into(),
                format!("class-{}", index % 13),
            ],
            tool_names: vec![format!("tool-{}", index % 9)],
            model: format!("model-{}", index % 5),
            input,
            output: format!("response-{index}-{}", "y".repeat(index % 257)),
            usage_input: (index % 4_096) as u64,
            usage_output: (index % 2_048) as u64,
            total_cost: format!("{:.12}", (index % 10_000) as f64 / 1_000_000.0),
            version: if duplicate_base == index { 1 } else { 2 },
            is_deleted: false,
            bookmarked: index % 101 == 0,
            public: index % 503 == 0,
            dataset_id: String::new(),
            dataset_run_id: String::new(),
            storage_run_id: trace_id,
            seq: 0,
        });
    }

    for index in 0..config.scores {
        let project = match index % 2 {
            0 => "project-a",
            _ => "project-b",
        };
        let attached = index % 3 != 0;
        let trace_index = index % config.traces;
        let attached_trace = trace_id(project, trace_index / 2);
        let event_ts_ms = base_ms + config.events as u64 + index as u64;
        rows.push(LogicalRow {
            row_kind: "score".into(),
            project_id: project.into(),
            trace_id: if attached {
                attached_trace.clone()
            } else {
                String::new()
            },
            span_id: String::new(),
            parent_span_id: String::new(),
            logical_id: format!("{project}-score-{index:08}"),
            event_ts: timestamp_string(event_ts_ms),
            event_ts_ms,
            start_time: timestamp_string(event_ts_ms),
            name: format!("score-{}", index % 13),
            event_type: "SCORE".into(),
            session_id: String::new(),
            user_id: String::new(),
            environment: "default".into(),
            tags: Vec::new(),
            metadata_names: vec!["data_type".into()],
            metadata_values: vec![["NUMERIC", "BOOLEAN", "CATEGORICAL"][index % 3].into()],
            tool_names: Vec::new(),
            model: String::new(),
            input: String::new(),
            output: format!("{}", index % 101),
            usage_input: 0,
            usage_output: 0,
            total_cost: "0.000000000000".into(),
            version: 1,
            is_deleted: false,
            bookmarked: false,
            public: false,
            dataset_id: String::new(),
            dataset_run_id: String::new(),
            storage_run_id: if attached {
                attached_trace
            } else {
                format!("synthetic-scores-{}", index % 4)
            },
            seq: 0,
        });
    }

    for index in 0..config.dataset_run_items {
        let project = if index % 2 == 0 {
            "project-a"
        } else {
            "project-b"
        };
        let event_ts_ms = base_ms + config.events as u64 + config.scores as u64 + index as u64;
        rows.push(LogicalRow {
            row_kind: "dataset_run_item".into(),
            project_id: project.into(),
            trace_id: String::new(),
            span_id: String::new(),
            parent_span_id: String::new(),
            logical_id: format!("{project}-dataset-run-item-{index:08}"),
            event_ts: timestamp_string(event_ts_ms),
            event_ts_ms,
            start_time: timestamp_string(event_ts_ms),
            name: "dataset-run-item".into(),
            event_type: "DATASET_RUN_ITEM".into(),
            session_id: String::new(),
            user_id: String::new(),
            environment: "default".into(),
            tags: Vec::new(),
            metadata_names: vec!["status".into()],
            metadata_values: vec![["complete", "error"][index % 2].into()],
            tool_names: Vec::new(),
            model: String::new(),
            input: String::new(),
            output: String::new(),
            usage_input: 0,
            usage_output: 0,
            total_cost: "0.000000000000".into(),
            version: 1,
            is_deleted: false,
            bookmarked: false,
            public: false,
            dataset_id: format!("dataset-{}", index % 17),
            dataset_run_id: format!("dataset-run-{}", index % 41),
            storage_run_id: format!("synthetic-dataset-items-{}", index % 4),
            seq: 0,
        });
    }

    for index in 0..config.blob_log_rows {
        let project = if index % 2 == 0 {
            "project-a"
        } else {
            "project-b"
        };
        let event_ts_ms = base_ms
            + config.events as u64
            + config.scores as u64
            + config.dataset_run_items as u64
            + index as u64;
        rows.push(LogicalRow {
            row_kind: "blob_storage_file_log".into(),
            project_id: project.into(),
            trace_id: String::new(),
            span_id: String::new(),
            parent_span_id: String::new(),
            logical_id: format!("{project}-blob-log-{index:08}"),
            event_ts: timestamp_string(event_ts_ms),
            event_ts_ms,
            start_time: timestamp_string(event_ts_ms),
            name: "blob-storage-file-log".into(),
            event_type: "BLOB_LOG".into(),
            session_id: String::new(),
            user_id: String::new(),
            environment: "default".into(),
            tags: Vec::new(),
            metadata_names: vec!["bucket".into(), "path".into()],
            metadata_values: vec![
                "review-bucket".into(),
                format!("{project}/events/{index}.json"),
            ],
            tool_names: Vec::new(),
            model: String::new(),
            input: String::new(),
            output: String::new(),
            usage_input: 0,
            usage_output: 0,
            total_cost: "0.000000000000".into(),
            version: 1,
            is_deleted: index % 97 == 0,
            bookmarked: false,
            public: false,
            dataset_id: String::new(),
            dataset_run_id: String::new(),
            storage_run_id: format!("synthetic-blob-log-{}", index % 2),
            seq: 0,
        });
    }

    let mut sequences = BTreeMap::<(String, String), u64>::new();
    for row in &mut rows {
        let sequence = sequences
            .entry((row.project_id.clone(), row.storage_run_id.clone()))
            .or_default();
        row.seq = *sequence;
        *sequence += 1;
    }
    Ok(rows)
}

fn generate_load_rows(config: &Config) -> Vec<LogicalRow> {
    let count = config.load_seconds * LOAD_ROWS_PER_SECOND;
    let base_ms = 1_767_225_900_000u64;
    (0..count)
        .map(|index| {
            let event_ts_ms = base_ms + (index as u64) * 100;
            LogicalRow {
                row_kind: "event".into(),
                project_id: "project-a".into(),
                trace_id: "project-a-load-trace".into(),
                span_id: format!("project-a-load-span-{index:04}"),
                parent_span_id: String::new(),
                logical_id: format!("project-a-load-event-{index:04}"),
                event_ts: timestamp_string(event_ts_ms),
                event_ts_ms,
                start_time: timestamp_string(event_ts_ms),
                name: "load-phase".into(),
                event_type: "SPAN".into(),
                session_id: "load-session".into(),
                user_id: "load-user".into(),
                environment: "default".into(),
                tags: vec!["load".into()],
                metadata_names: vec!["rate".into()],
                metadata_values: vec!["10eps".into()],
                tool_names: Vec::new(),
                model: "load-model".into(),
                input: format!("load-input-{index}"),
                output: format!("load-output-{index}"),
                usage_input: 1,
                usage_output: 1,
                total_cost: "0.000001000000".into(),
                version: 1,
                is_deleted: false,
                bookmarked: false,
                public: false,
                dataset_id: String::new(),
                dataset_run_id: String::new(),
                storage_run_id: "project-a-load-trace".into(),
                seq: index as u64,
            }
        })
        .collect()
}

fn write_fixture(path: &Path, rows: &[LogicalRow]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create fixture {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for row in rows {
        let mut value = serde_json::to_value(row)?;
        let payload_json = serde_json::to_string(row)?;
        value
            .as_object_mut()
            .context("logical row did not encode as an object")?
            .insert("payload_json".into(), Value::String(payload_json));
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

async fn discover_catalog(storage: &Path) -> Result<Arc<DatasetCatalogSnapshot>> {
    let mount = DatasetMount::default(storage.to_string_lossy())?;
    Ok(Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![mount],
            Some("dataset".into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    ))
}

async fn query_values(engine: &ChronicleQueryEngine, sql: &str) -> Result<Vec<Value>> {
    engine
        .query_jsonl(sql)
        .await?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("decode pChronicle query JSONL"))
        .collect()
}

async fn count_query(engine: &ChronicleQueryEngine, sql: &str) -> Result<u64> {
    let rows = query_values(engine, sql).await?;
    anyhow::ensure!(rows.len() == 1, "count query returned {} rows", rows.len());
    rows[0]["row_count"]
        .as_u64()
        .context("count query row_count is not an unsigned integer")
}

async fn measure_query(
    engine: &ChronicleQueryEngine,
    sql: &str,
    repetitions: usize,
) -> Result<QueryMetric> {
    let mut samples = Vec::with_capacity(repetitions);
    let mut result_rows = 0usize;
    for _ in 0..repetitions {
        let started = Instant::now();
        let batches = engine.query(sql).await?;
        samples.push(duration_ms(started.elapsed()));
        result_rows = batches.iter().map(|batch| batch.num_rows()).sum();
    }
    Ok(QueryMetric {
        repetitions,
        result_rows,
        p50_ms: percentile(&samples, 0.50),
        p95_ms: percentile(&samples, 0.95),
        max_ms: samples.iter().copied().fold(0.0, f64::max),
    })
}

async fn run_capability_probes(engine: &ChronicleQueryEngine) -> BTreeMap<String, CapabilityProbe> {
    let probes = [
        ("approx_top_k", "SELECT approx_top_k(5)(kind) FROM events"),
        (
            "full_text_has_all_tokens",
            "SELECT hasAllTokens(payload_json, 'needle-token') FROM events LIMIT 1",
        ),
        (
            "array_join",
            "SELECT arrayJoin(['a', 'b']) FROM events LIMIT 1",
        ),
        ("sum_map", "SELECT sumMap(map('x', 1)) FROM events"),
        ("arg_max", "SELECT argMax(kind, timestamp) FROM events"),
        ("quantile_curried", "SELECT quantile(0.95)(seq) FROM events"),
        (
            "with_fill",
            "SELECT timestamp, count(*) FROM events GROUP BY timestamp ORDER BY timestamp WITH FILL",
        ),
        (
            "prewhere",
            "SELECT * FROM events PREWHERE agent_id = 'project-a' LIMIT 1",
        ),
        (
            "limit_by",
            "SELECT * FROM events ORDER BY timestamp DESC LIMIT 1 BY event_id",
        ),
        ("final", "SELECT COUNT(*) FROM events FINAL"),
        (
            "json_extract_string",
            "SELECT JSONExtractString(payload_json, 'kind') FROM events LIMIT 1",
        ),
    ];
    let mut results = BTreeMap::new();
    for (name, sql) in probes {
        let result = match engine.query(sql).await {
            Ok(_) => CapabilityProbe {
                supported: true,
                error: None,
            },
            Err(error) => CapabilityProbe {
                supported: false,
                error: Some(redact_error(&error.to_string())),
            },
        };
        results.insert(name.into(), result);
    }
    results
}

fn timestamp_string(timestamp_ms: u64) -> String {
    let datetime = Utc
        .timestamp_millis_opt(timestamp_ms as i64)
        .single()
        .unwrap_or_else(|| Utc::now() + ChronoDuration::milliseconds(timestamp_ms as i64));
    datetime.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

fn project_for_trace(trace_index: usize) -> &'static str {
    if trace_index.is_multiple_of(2) {
        "project-a"
    } else {
        "project-b"
    }
}

fn trace_id(project: &str, local_index: usize) -> String {
    format!("{project}-trace-{local_index:04}")
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentile(samples: &[f64], quantile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn current_rss_bytes() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kib = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1_024)
}

fn redact_error(message: &str) -> String {
    let mut compact = message.replace('\n', " ");
    compact.truncate(compact.len().min(500));
    compact
}
