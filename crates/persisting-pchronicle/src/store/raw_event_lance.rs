//! Lance event log backend for the canonical trajectory store.
//!
//! Capture runs use one run-level manifest at `{run}/events.lance`; each writer
//! epoch owns one private Lance segment and `session_id` filters rows when
//! replaying one story view.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::EventRow;
use anyhow::{Context, Result};
use datafusion::error::DataFusionError;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::SendableRecordBatchStream;
use futures::{StreamExt, TryStreamExt};
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{Array, RecordBatch};
use lance::index::DatasetIndexExt;
use lance::Dataset;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;

use super::raw_event_lance_rows::{
    event_rows_from_batch, event_rows_to_batch, raw_event_arrow_schema, replay_records_from_batch,
    rows_for_events, schema_columns_note,
};
pub use super::raw_event_manifest::EventWriterFence;
use super::raw_event_manifest::{self, EventManifest, EventSegment};
use super::{
    dataset_write_lock, raw_event_lance_path, AppendOutcome, ReplayOutcome, TrajectorySession,
    TrajectoryStats,
};

const SESSION_INDEX_NAME: &str = "pchronicle_session_id_idx";
const MAX_SEGMENT_OPEN_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct LanceMaintenanceOptions {
    pub compact: bool,
    pub optimize_indices: bool,
    pub vacuum_older_than: Option<Duration>,
    pub target_rows_per_fragment: usize,
}

impl Default for LanceMaintenanceOptions {
    fn default() -> Self {
        Self {
            compact: true,
            optimize_indices: true,
            vacuum_older_than: Some(Duration::from_secs(7 * 24 * 60 * 60)),
            target_rows_per_fragment: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanceMaintenanceReport {
    pub fragments_removed: usize,
    pub fragments_added: usize,
    pub old_versions_removed: u64,
    pub bytes_removed: u64,
    pub final_version: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventLogLayoutStats {
    pub manifest_revision: Option<u64>,
    pub active_epoch: Option<u64>,
    pub visible_segments: usize,
    pub visible_fragments: usize,
    pub visible_rows: u64,
}

#[derive(Debug)]
pub struct RawEventLanceAppender {
    requested_fence: Option<EventWriterFence>,
    auto_writer_id: String,
    datasets: BTreeMap<String, CachedRawDataset>,
}

#[derive(Debug)]
struct CachedRawDataset {
    fence: EventWriterFence,
    segment_id: String,
    segment_uri: String,
    dataset: Option<Dataset>,
    rows: u64,
    manifest_revision: u64,
}

impl Default for RawEventLanceAppender {
    fn default() -> Self {
        Self {
            requested_fence: None,
            auto_writer_id: format!("auto-{}", uuid::Uuid::new_v4()),
            datasets: BTreeMap::new(),
        }
    }
}

impl RawEventLanceAppender {
    /// Construct a writer bound to an externally allocated lease epoch.
    ///
    /// `writer_id` must uniquely identify the lease holder or process attempt.
    /// Another writer using the same epoch but a different id is rejected.
    pub fn fenced(fence: EventWriterFence) -> Self {
        Self {
            auto_writer_id: fence.writer_id.clone(),
            requested_fence: Some(fence),
            datasets: BTreeMap::new(),
        }
    }

    /// Activate this writer before accepting data. A newer activation fences
    /// every older appender at the manifest publication boundary.
    pub async fn activate(&mut self, session: &TrajectorySession) -> Result<EventWriterFence> {
        let uri = raw_event_lance_path(session)?
            .to_string_lossy()
            .into_owned();
        if let Some(state) = self.datasets.get(&uri) {
            return Ok(state.fence.clone());
        }
        let state = self.new_state(uri.as_str()).await?;
        let fence = state.fence.clone();
        self.datasets.insert(uri, state);
        Ok(fence)
    }

    async fn new_state(&self, uri: &str) -> Result<CachedRawDataset> {
        let manifest =
            raw_event_manifest::activate(uri, self.requested_fence.as_ref(), &self.auto_writer_id)
                .await?;
        let fence = manifest.active_writer.clone();
        let segment_id = format!("e{}-{}", fence.epoch, uuid::Uuid::new_v4());
        Ok(CachedRawDataset {
            fence,
            segment_uri: raw_event_manifest::segment_uri(uri, &segment_id),
            segment_id,
            dataset: None,
            rows: 0,
            manifest_revision: manifest.revision,
        })
    }

    /// Append one channel-sized micro-batch with no read-before-write work.
    ///
    /// This writer deliberately provides at-least-once, append-only semantics:
    /// every input record becomes one physical row, including duplicate
    /// `event_id` values. Visibility is published with an epoch-fenced manifest
    /// CAS, so a stale writer can leave only an unreachable Lance version.
    pub async fn append_event_batch(
        &mut self,
        entries: &[(TrajectorySession, crate::EventRecord)],
    ) -> Result<AppendOutcome> {
        if entries.is_empty() {
            return Ok(AppendOutcome {
                accepted_records: 0,
                persisted_units: 0,
                note: "Lance: empty event micro-batch".into(),
            });
        }
        let mut groups: BTreeMap<String, (TrajectorySession, Vec<(String, crate::EventRecord)>)> =
            BTreeMap::new();
        for (session, record) in entries {
            let uri = raw_event_lance_path(session)?
                .to_string_lossy()
                .into_owned();
            let canonical = super::canonicalize_event(session, record.clone());
            groups
                .entry(uri)
                .or_insert_with(|| (session.clone(), Vec::new()))
                .1
                .push((session.session_id.clone(), canonical));
        }

        let mut accepted_records = 0;
        for (uri, (dataset_session, records)) in groups {
            let mut state = match self.datasets.remove(&uri) {
                Some(state) => state,
                None => {
                    let _ = dataset_session;
                    self.new_state(&uri).await?
                }
            };
            let mut rows = Vec::with_capacity(records.len());
            for (session_id, record) in &records {
                rows.extend(rows_for_events(session_id, std::slice::from_ref(record))?);
            }
            let appended_rows = rows.len();
            let dataset = append_rows(&state.segment_uri, state.dataset.take(), rows).await?;
            state.rows = state
                .rows
                .checked_add(appended_rows as u64)
                .context("event segment row count overflow")?;
            let manifest = raw_event_manifest::publish_segment(
                &uri,
                &state.fence,
                EventSegment {
                    id: state.segment_id.clone(),
                    version: dataset.version_id(),
                    rows: state.rows,
                },
            )
            .await?;
            state.manifest_revision = manifest.revision;
            state.dataset = Some(dataset);
            accepted_records += appended_rows;
            self.datasets.insert(uri, state);
        }
        Ok(AppendOutcome {
            accepted_records,
            persisted_units: accepted_records,
            note: format!("Lance: committed {accepted_records} event(s) in a micro-batch"),
        })
    }

    /// Close the writer without indexing, compaction, vacuum, or any other
    /// synchronous maintenance. Heavy layout work is available through the
    /// explicit [`maintain`] API and never runs on the ingestion path.
    pub fn finish(self) -> Vec<LanceMaintenanceReport> {
        self.datasets
            .into_values()
            .map(|state| LanceMaintenanceReport {
                final_version: Some(state.manifest_revision),
                ..Default::default()
            })
            .collect()
    }
}

pub(super) fn validate_event_schema(dataset: &Dataset, uri: &str) -> Result<()> {
    let columns = dataset
        .schema()
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        columns == crate::TRAJECTORY_COLS,
        "trajectory dataset schema mismatch at {uri}: expected [{}], found [{}]",
        crate::TRAJECTORY_COLS.join(", "),
        columns.join(", ")
    );
    Ok(())
}

async fn open_visible_segment(root_uri: &str, segment: &EventSegment) -> Result<Dataset> {
    let uri = raw_event_manifest::segment_uri(root_uri, &segment.id);
    let latest = Dataset::open(&uri)
        .await
        .with_context(|| format!("open event segment {uri}"))?;
    let dataset = if latest.version_id() == segment.version {
        latest
    } else {
        latest
            .checkout_version(segment.version)
            .await
            .with_context(|| {
                format!(
                    "open visible event segment version {} at {uri}",
                    segment.version
                )
            })?
    };
    validate_event_schema(&dataset, &uri)?;
    Ok(dataset)
}

pub(super) async fn open_visible_snapshot(
    root_uri: &str,
) -> Result<Option<(EventManifest, Vec<Dataset>)>> {
    let Some(manifest) = raw_event_manifest::read(root_uri).await? else {
        return Ok(None);
    };
    let datasets = futures::stream::iter(manifest.segments.iter().cloned())
        .map(|segment| async move { open_visible_segment(root_uri, &segment).await })
        .buffered(MAX_SEGMENT_OPEN_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    Ok(Some((manifest, datasets)))
}

#[cfg(test)]
async fn visible_fragment_count(root_uri: &str) -> Result<usize> {
    let Some((_, datasets)) = open_visible_snapshot(root_uri).await? else {
        return Ok(0);
    };
    Ok(datasets
        .iter()
        .map(|dataset| dataset.get_fragments().len())
        .sum())
}

async fn ensure_session_index(dataset: &mut Dataset) -> Result<()> {
    if dataset
        .load_indices_by_name(SESSION_INDEX_NAME)
        .await
        .context("load trajectory session index")?
        .is_empty()
    {
        let _admission = super::index_build_gate::acquire().await;
        dataset
            .create_index(
                &[crate::TRAJECTORY_SESSION_ID_COL],
                IndexType::BTree,
                Some(SESSION_INDEX_NAME.into()),
                &ScalarIndexParams::for_builtin(BuiltinIndexType::BTree),
                false,
            )
            .await
            .context("create trajectory session index")?;
    }
    Ok(())
}

async fn compact_visible_segments(
    root_uri: &str,
    datasets: Vec<Dataset>,
    target_rows_per_fragment: usize,
    optimize_indices: bool,
) -> Result<(Dataset, EventSegment)> {
    anyhow::ensure!(
        target_rows_per_fragment > 0,
        "target_rows_per_fragment must be greater than zero"
    );
    let segment_id = format!("compact-{}", uuid::Uuid::new_v4());
    let segment_uri = raw_event_manifest::segment_uri(root_uri, &segment_id);
    let schema = raw_event_arrow_schema();
    let streams = futures::stream::iter(datasets).then(|dataset| async move {
        dataset
            .scan()
            .scan_in_order(true)
            .try_into_stream()
            .await
            .map(|stream| stream.map_err(|error| DataFusionError::External(Box::new(error))))
            .map_err(|error| DataFusionError::External(Box::new(error)))
    });
    let flattened = streams.try_flatten();
    let stream: SendableRecordBatchStream =
        Box::pin(RecordBatchStreamAdapter::new(schema.clone(), flattened));
    let params = WriteParams {
        max_rows_per_file: target_rows_per_fragment,
        max_rows_per_group: target_rows_per_fragment.min(1024 * 1024),
        ..Default::default()
    };
    let mut dataset = InsertBuilder::new(segment_uri.as_str())
        .with_params(&params)
        .execute_stream(stream)
        .await
        .with_context(|| format!("write compacted event segment {segment_uri}"))?;
    if optimize_indices {
        ensure_session_index(&mut dataset).await?;
    }
    let rows = dataset
        .count_rows(None)
        .await
        .context("count compacted event segment rows")? as u64;
    Ok((
        dataset,
        EventSegment {
            id: segment_id,
            version: 0,
            rows,
        },
    ))
}

pub async fn maintain(
    session: &TrajectorySession,
    options: &LanceMaintenanceOptions,
) -> Result<LanceMaintenanceReport> {
    let uri = raw_event_lance_path(session)?
        .to_string_lossy()
        .into_owned();
    let _guard = dataset_write_lock::acquire(&uri).await?;
    let Some((before_manifest, datasets)) = open_visible_snapshot(&uri).await? else {
        return Ok(LanceMaintenanceReport::default());
    };
    if datasets.is_empty() {
        return Ok(LanceMaintenanceReport {
            final_version: Some(before_manifest.revision),
            ..Default::default()
        });
    }
    let maintenance_writer_id = format!("maintenance-{}", uuid::Uuid::new_v4());
    let active = raw_event_manifest::activate(&uri, None, &maintenance_writer_id).await?;
    let fence = active.active_writer.clone();
    let fragments_before = datasets
        .iter()
        .map(|dataset| dataset.get_fragments().len())
        .sum::<usize>();
    let mut report = LanceMaintenanceReport {
        fragments_removed: 0,
        fragments_added: fragments_before,
        old_versions_removed: 0,
        bytes_removed: 0,
        final_version: Some(active.revision),
    };
    if options.compact {
        let (compacted, mut segment) = compact_visible_segments(
            &uri,
            datasets,
            options.target_rows_per_fragment,
            options.optimize_indices,
        )
        .await?;
        segment.version = compacted.version_id();
        let published = raw_event_manifest::replace_segments(&uri, &fence, vec![segment]).await?;
        report.fragments_removed = fragments_before;
        report.fragments_added = compacted.get_fragments().len();
        report.final_version = Some(published.revision);
        if let Some(retention) = options.vacuum_older_than {
            let retention = chrono::Duration::from_std(retention)
                .context("trajectory Lance vacuum retention is too large")?;
            let removed = compacted
                .cleanup_old_versions(retention, Some(false), Some(true))
                .await
                .context("vacuum compacted event segment versions")?;
            report.old_versions_removed = removed.old_versions;
            report.bytes_removed = removed.bytes_removed;
        }
    } else {
        let mut updated_segments = Vec::with_capacity(datasets.len());
        for (mut dataset, mut segment) in datasets.into_iter().zip(before_manifest.segments) {
            if options.optimize_indices {
                ensure_session_index(&mut dataset).await?;
                dataset
                    .optimize_indices(&OptimizeOptions::append())
                    .await
                    .context("optimize trajectory Lance indices")?;
            }
            segment.version = dataset.version_id();
            updated_segments.push(segment);
        }
        let published =
            raw_event_manifest::replace_segments(&uri, &fence, updated_segments).await?;
        report.final_version = Some(published.revision);
    }
    if let Some(retention) = options.vacuum_older_than {
        let current = raw_event_manifest::read(&uri)
            .await?
            .context("event manifest disappeared after maintenance")?;
        let removed =
            raw_event_manifest::cleanup_unreferenced_segments(&uri, &current.segments, retention)
                .await?;
        report.old_versions_removed = report
            .old_versions_removed
            .saturating_add(removed.segments_removed);
        report.bytes_removed = report.bytes_removed.saturating_add(removed.bytes_removed);
    }
    Ok(report)
}

#[cfg(test)]
async fn read_all_rows(uri: &str) -> Result<Vec<EventRow>> {
    let Some((_, datasets)) = open_visible_snapshot(uri).await? else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for dataset in datasets {
        let batches: Vec<RecordBatch> = dataset
            .scan()
            .try_into_stream()
            .await
            .with_context(|| format!("scan trajectory event segment under {uri}"))?
            .try_collect()
            .await
            .with_context(|| format!("collect trajectory event segment under {uri}"))?;
        for batch in &batches {
            rows.extend(event_rows_from_batch(batch)?);
        }
    }
    Ok(rows)
}

async fn append_rows(uri: &str, dataset: Option<Dataset>, rows: Vec<EventRow>) -> Result<Dataset> {
    let batch = event_rows_to_batch(raw_event_arrow_schema(), &rows)?;
    if !is_object_store_uri(uri) {
        if let Some(parent) = std::path::Path::new(uri).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
    }
    match dataset {
        Some(dataset) => InsertBuilder::new(Arc::new(dataset))
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute(vec![batch])
            .await
            .with_context(|| format!("append trajectory Lance dataset {uri}")),
        None => InsertBuilder::new(uri)
            .execute(vec![batch])
            .await
            .with_context(|| format!("create trajectory Lance dataset {uri}")),
    }
}

fn session_predicate(session_id: &str) -> String {
    format!("session_id = '{}'", session_id.replace('\'', "''"))
}

async fn read_session_rows(
    dataset: &Dataset,
    session_id: &str,
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<EventRow>> {
    let mut scan = dataset.scan();
    scan.filter(&session_predicate(session_id))
        .with_context(|| format!("filter trajectory session {session_id}"))?;
    scan.scan_in_order(true);
    scan.limit(
        limit.map(|value| value as i64),
        (offset > 0).then_some(offset as i64),
    )
    .context("apply trajectory replay offset/limit")?;
    let batches: Vec<RecordBatch> = scan
        .try_into_stream()
        .await
        .context("scan trajectory session rows")?
        .try_collect()
        .await
        .context("collect trajectory session rows")?;
    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(event_rows_from_batch(batch)?);
    }
    // Lance scan order is the canonical immutable append order. Producer `seq`
    // values may repeat or reset between logical Storylines and are never used
    // to reorder stored facts.
    Ok(rows)
}

pub fn display_path(session: &TrajectorySession) -> Result<String> {
    Ok(raw_event_lance_path(session)?
        .to_string_lossy()
        .into_owned())
}

pub async fn distinct_session_ids_in_run(run: &TrajectorySession) -> Result<Vec<String>> {
    let path = raw_event_lance_path(run)?;
    let uri = path.to_string_lossy().into_owned();
    let Some((_, datasets)) = open_visible_snapshot(&uri).await? else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for dataset in datasets {
        let mut scan = dataset.scan();
        scan.project(&[crate::TRAJECTORY_SESSION_ID_COL])
            .context("project trajectory session_id")?;
        let batches: Vec<RecordBatch> = scan
            .try_into_stream()
            .await
            .context("scan trajectory session ids")?
            .try_collect()
            .await
            .context("collect trajectory session ids")?;
        for batch in &batches {
            let column = batch
                .column_by_name(crate::TRAJECTORY_SESSION_ID_COL)
                .context("trajectory scan missing session_id")?;
            let values = column
                .as_any()
                .downcast_ref::<lance::deps::arrow_array::StringArray>()
                .context("trajectory session_id must be Utf8")?;
            ids.extend(values.iter().flatten().map(ToOwned::to_owned));
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub async fn exists(session: &TrajectorySession) -> Result<bool> {
    let path = raw_event_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let Some((_, datasets)) = open_visible_snapshot(&uri).await? else {
        return Ok(false);
    };
    for dataset in datasets {
        if dataset
            .count_rows(Some(session_predicate(&session.session_id)))
            .await
            .context("count trajectory session rows")?
            > 0
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn append(session: &TrajectorySession, lines: &[String]) -> Result<AppendOutcome> {
    let records = crate::decode_event_lines(lines)?;
    append_events(session, &records).await
}

pub async fn append_events(
    session: &TrajectorySession,
    records: &[crate::EventRecord],
) -> Result<AppendOutcome> {
    let entries = records
        .iter()
        .cloned()
        .map(|record| (session.clone(), record))
        .collect::<Vec<_>>();
    RawEventLanceAppender::default()
        .append_event_batch(&entries)
        .await
}

pub(super) fn is_object_store_uri(uri: &str) -> bool {
    uri.contains("://")
}

pub async fn replay(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<ReplayOutcome> {
    replay_available(session, offset, limit)
        .await?
        .ok_or_else(|| {
            let uri = raw_event_lance_path(session)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "<invalid trajectory path>".into());
            anyhow::anyhow!("trajectory Lance dataset does not exist at {uri}")
        })
}

/// Replay the currently committed rows for one Storyline, returning `None`
/// while its run-level Lance dataset has not been created yet.
///
/// Each call reads one atomic manifest revision and checks out the exact Lance
/// versions it names. This makes follow reads immune to stale-writer versions.
pub async fn replay_available(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<Option<ReplayOutcome>> {
    let path = raw_event_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let Some((manifest, datasets)) = open_visible_snapshot(&uri).await? else {
        return Ok(None);
    };
    let mut remaining_offset = offset;
    let mut remaining_limit = limit.unwrap_or(usize::MAX);
    let mut rows = Vec::new();
    for dataset in datasets {
        if remaining_limit == 0 {
            break;
        }
        let segment_rows = dataset
            .count_rows(Some(session_predicate(&session.session_id)))
            .await
            .context("count trajectory session rows in visible segment")?;
        if remaining_offset >= segment_rows {
            remaining_offset -= segment_rows;
            continue;
        }
        let take = remaining_limit.min(segment_rows - remaining_offset);
        rows.extend(
            read_session_rows(&dataset, &session.session_id, remaining_offset, Some(take)).await?,
        );
        remaining_offset = 0;
        remaining_limit -= take;
    }
    let schema = raw_event_arrow_schema();
    let batch = event_rows_to_batch(schema, &rows)?;
    let records = replay_records_from_batch(&batch)?;
    Ok(Some(ReplayOutcome {
        records,
        note: format!(
            "Replay fenced Lance manifest revision {} at {uri}: session_id={}, ordered by immutable segment and append order, offset={offset}, limit={limit:?}.",
            manifest.revision,
            session.session_id,
        ),
    }))
}

pub async fn stats(session: &TrajectorySession) -> Result<TrajectoryStats> {
    let path = raw_event_lance_path(session)?;
    let display = path.to_string_lossy().into_owned();
    let Some((manifest, datasets)) = open_visible_snapshot(&display).await? else {
        return Ok(TrajectoryStats {
            dataset: display,
            row_count: 0,
            manifest_version: None,
            status: "missing".to_string(),
            note: "No Lance event log at this path yet; use trajectory add first.".to_string(),
        });
    };
    let mut row_count = 0usize;
    let mut fragment_count = 0usize;
    let mut indexed_segments = 0usize;
    for dataset in &datasets {
        row_count += dataset
            .count_rows(Some(session_predicate(&session.session_id)))
            .await
            .context("count trajectory session rows")?;
        fragment_count += dataset.get_fragments().len();
        if !dataset
            .load_indices_by_name(SESSION_INDEX_NAME)
            .await
            .context("inspect trajectory session index")?
            .is_empty()
        {
            indexed_segments += 1;
        }
    }
    Ok(TrajectoryStats {
        dataset: display.clone(),
        row_count,
        manifest_version: Some(manifest.revision),
        status: "ok".to_string(),
        note: format!(
            "Lance epoch-fenced append-only [{}]; session_id={}; dataset={}; epoch={}; \
             visible_rows={}; segments={}; fragments={fragment_count}; indexed_segments={indexed_segments}",
            schema_columns_note(),
            session.session_id,
            display,
            manifest.active_writer.epoch,
            manifest.total_rows(),
            manifest.segments.len(),
        ),
    })
}

pub async fn layout_stats(session: &TrajectorySession) -> Result<EventLogLayoutStats> {
    let uri = raw_event_lance_path(session)?
        .to_string_lossy()
        .into_owned();
    let Some((manifest, datasets)) = open_visible_snapshot(&uri).await? else {
        return Ok(EventLogLayoutStats::default());
    };
    Ok(EventLogLayoutStats {
        manifest_revision: Some(manifest.revision),
        active_epoch: Some(manifest.active_writer.epoch),
        visible_segments: manifest.segments.len(),
        visible_fragments: datasets
            .iter()
            .map(|dataset| dataset.get_fragments().len())
            .sum(),
        visible_rows: manifest.total_rows(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventRecord, StoryCoords};
    use std::sync::atomic::{AtomicU64, Ordering};

    const CHUNK_ROWS: usize = 8192;
    static NEXT_REMOTE_STORE: AtomicU64 = AtomicU64::new(1);

    fn remote_storage(label: &str) -> String {
        format!(
            "shared-memory://pchronicle-events-{}-{label}-{}/trajectories",
            std::process::id(),
            NEXT_REMOTE_STORE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn note_line(content: &str) -> String {
        ron::to_string(
            &serde_json::to_value(EventRecord {
                identity: crate::EventIdentity::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({ "content": content }),
            })
            .unwrap(),
        )
        .unwrap()
    }

    fn identified_note(event_id: &str, seq: u64, content: &str) -> EventRecord {
        EventRecord {
            identity: crate::EventIdentity {
                event_id: Some(event_id.into()),
                ..Default::default()
            },
            seq,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({ "content": content }),
        }
    }

    fn run_session(storage: &str, agent: &str, session_id: &str, root: &str) -> TrajectorySession {
        StoryCoords::new(storage, agent, session_id, Some(root.to_string()))
    }

    fn flat_session(storage: &str, agent: &str, session_id: &str) -> TrajectorySession {
        StoryCoords::new(storage, agent, session_id, None)
    }

    fn payload_content(replay_json: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(replay_json).unwrap();
        v["payload"]["content"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn append_creates_lance_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        append(&session, &[note_line("one")]).await.unwrap();

        let path = raw_event_lance_path(&session).unwrap();
        let (manifest, datasets) = open_visible_snapshot(&path.to_string_lossy())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(manifest.total_rows(), 1);
        assert_eq!(datasets.len(), 1);
    }

    #[tokio::test]
    async fn append_then_append_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        append(&session, &[note_line("first")]).await.unwrap();
        append(&session, &[note_line("second")]).await.unwrap();

        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 2);
        assert_eq!(payload_content(&replay.records[0]), "first");
        assert_eq!(payload_content(&replay.records[1]), "second");
    }

    #[tokio::test]
    async fn typed_append_preserves_duplicates_and_storyline_seq() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
        let record = identified_note("stable-event", 42, "once");

        let first = append_events(&session, std::slice::from_ref(&record))
            .await
            .unwrap();
        let duplicate = append_events(&session, &[record.clone(), record])
            .await
            .unwrap();

        assert_eq!(first.accepted_records, 1);
        assert_eq!(duplicate.accepted_records, 2);
        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 3);
        let restored: EventRecord = serde_json::from_str(&replay.records[0]).unwrap();
        assert_eq!(restored.identity.event_id.as_deref(), Some("stable-event"));
        assert_eq!(restored.seq, 42);
    }

    #[tokio::test]
    async fn missing_event_id_is_stored_as_null_without_deduplication() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
        let mut record = identified_note("temporary", 7, "retry-safe");
        record.identity.event_id = None;

        let first = append_events(&session, std::slice::from_ref(&record))
            .await
            .unwrap();
        let retry = append_events(&session, std::slice::from_ref(&record))
            .await
            .unwrap();
        assert_eq!(first.accepted_records, 1);
        assert_eq!(retry.accepted_records, 1);

        let uri = raw_event_lance_path(&session)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (_, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
        assert!(datasets[0]
            .schema()
            .field(crate::TRAJECTORY_EVENT_ID_COL)
            .is_some());
        let rows = read_all_rows(&raw_event_lance_path(&session).unwrap().to_string_lossy())
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].event_id, None);
        assert_eq!(rows[1].event_id, None);
        let restored = replay(&session, 0, None)
            .await
            .unwrap()
            .records
            .into_iter()
            .map(|record| serde_json::from_str::<EventRecord>(&record).unwrap())
            .collect::<Vec<_>>();
        assert!(restored
            .iter()
            .all(|record| record.identity.event_id.is_none()));
    }

    #[tokio::test]
    async fn incompatible_event_schema_is_rejected_without_migration() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
        append_events(&session, &[identified_note("event-1", 0, "first")])
            .await
            .unwrap();

        let uri = raw_event_lance_path(&session)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (manifest, _) = open_visible_snapshot(&uri).await.unwrap().unwrap();
        let mut segment = manifest.segments[0].clone();
        let mut dataset = Dataset::open(&raw_event_manifest::segment_uri(&uri, &segment.id))
            .await
            .unwrap();
        dataset
            .drop_columns(&[crate::TRAJECTORY_EVENT_ID_COL])
            .await
            .unwrap();
        segment.version = dataset.version_id();
        raw_event_manifest::publish_segment(&uri, &manifest.active_writer, segment)
            .await
            .unwrap();

        let replay_error = replay(&session, 0, None).await.unwrap_err();
        assert!(replay_error.to_string().contains("schema mismatch"));
    }

    #[tokio::test]
    async fn one_cached_appender_preserves_physical_append_order() {
        let storage = remote_storage("cached-single-writer");
        let session = flat_session(&storage, "agent", "session");
        let mut writer = RawEventLanceAppender::default();

        writer
            .append_event_batch(&[(session.clone(), identified_note("first", 10, "first"))])
            .await
            .unwrap();
        writer
            .append_event_batch(&[(session.clone(), identified_note("second", 20, "second"))])
            .await
            .unwrap();
        writer
            .append_event_batch(&[(session.clone(), identified_note("third", 30, "third"))])
            .await
            .unwrap();
        let reports = writer.finish();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].fragments_removed, 0);

        let uri = raw_event_lance_path(&session)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(datasets[0].get_fragments().len(), 3);
        assert!(datasets[0]
            .load_indices_by_name(SESSION_INDEX_NAME)
            .await
            .unwrap()
            .is_empty());

        let restored = replay(&session, 0, None).await.unwrap();
        assert_eq!(
            restored
                .records
                .iter()
                .map(|record| serde_json::from_str::<EventRecord>(record).unwrap().seq)
                .collect::<Vec<_>>(),
            [10, 20, 30]
        );
    }

    #[tokio::test]
    async fn replay_available_follows_committed_pages() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        assert!(replay_available(&session, 0, Some(2))
            .await
            .unwrap()
            .is_none());

        append(
            &session,
            &[note_line("first"), note_line("second"), note_line("third")],
        )
        .await
        .unwrap();
        let first_page = replay_available(&session, 0, Some(2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first_page
                .records
                .iter()
                .map(|record| payload_content(record))
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        append(&session, &[note_line("fourth")]).await.unwrap();
        let second_page = replay_available(&session, first_page.records.len(), Some(2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            second_page
                .records
                .iter()
                .map(|record| payload_content(record))
                .collect::<Vec<_>>(),
            ["third", "fourth"]
        );
        assert!(replay_available(&session, 4, Some(2))
            .await
            .unwrap()
            .unwrap()
            .records
            .is_empty());
    }

    #[tokio::test]
    async fn object_store_uri_supports_append_only_replay() {
        let storage = remote_storage("round-trip");
        let session = flat_session(&storage, "agent", "remote-session");
        assert!(display_path(&session).unwrap().starts_with(&storage));

        append(&session, &[note_line("first"), note_line("second")])
            .await
            .unwrap();
        assert_eq!(replay(&session, 0, None).await.unwrap().records.len(), 2);

        append(&session, &[note_line("third")]).await.unwrap();
        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 3);
        assert_eq!(payload_content(&replay.records[2]), "third");
    }

    #[tokio::test]
    async fn object_store_append_failure_preserves_committed_rows() {
        let storage = remote_storage("failed-append");
        let session = flat_session(&storage, "agent", "session");
        append(&session, &[note_line("committed")]).await.unwrap();

        let error = append(&session, &["this is not valid RON".into()])
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty());

        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 1);
        assert_eq!(payload_content(&replay.records[0]), "committed");
    }

    #[tokio::test]
    async fn session_partition_replay_isolates_stories_in_shared_run_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-20260101";

        let main = run_session(&storage_s, "agent", root, root);
        let sub_a = run_session(&storage_s, "agent", "agent-sub-a", root);
        let sub_b = run_session(&storage_s, "agent", "agent-sub-b", root);

        append(&main, &[note_line("main-1"), note_line("main-2")])
            .await
            .unwrap();
        append(&sub_a, &[note_line("sub-a-1")]).await.unwrap();
        append(&sub_b, &[note_line("sub-b-1"), note_line("sub-b-2")])
            .await
            .unwrap();

        let lance_path = raw_event_lance_path(&main).unwrap();
        assert_eq!(
            raw_event_lance_path(&sub_a).unwrap(),
            lance_path,
            "run-level sessions share one events.lance"
        );

        let main_replay = replay(&main, 0, None).await.unwrap();
        assert_eq!(main_replay.records.len(), 2);
        assert!(main_replay
            .records
            .iter()
            .all(|r| payload_content(r).starts_with("main-")));

        let sub_a_replay = replay(&sub_a, 0, None).await.unwrap();
        assert_eq!(sub_a_replay.records.len(), 1);
        assert_eq!(payload_content(&sub_a_replay.records[0]), "sub-a-1");

        let sub_b_replay = replay(&sub_b, 1, Some(1)).await.unwrap();
        assert_eq!(sub_b_replay.records.len(), 1);
        assert_eq!(payload_content(&sub_b_replay.records[0]), "sub-b-2");
    }

    #[tokio::test]
    async fn session_partition_stats_and_exists_respect_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-partition";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-worker", root);
        let empty = run_session(&storage_s, "agent", "agent-never-written", root);

        append(&main, &[note_line("main")]).await.unwrap();
        append(&sub, &[note_line("sub-1"), note_line("sub-2")])
            .await
            .unwrap();

        assert!(exists(&main).await.unwrap());
        assert!(exists(&sub).await.unwrap());
        assert!(!exists(&empty).await.unwrap());

        let main_stats = stats(&main).await.unwrap();
        assert_eq!(main_stats.row_count, 1);

        let sub_stats = stats(&sub).await.unwrap();
        assert_eq!(sub_stats.row_count, 2);
    }

    #[tokio::test]
    async fn append_keeps_producer_seq_independent_across_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-global-seq";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-sub", root);

        append(&main, &[note_line("m1"), note_line("m2")])
            .await
            .unwrap();
        append(&sub, &[note_line("s1")]).await.unwrap();

        let rows = read_all_rows(&raw_event_lance_path(&main).unwrap().to_string_lossy())
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 0, 0]
        );
    }

    #[tokio::test]
    async fn routed_micro_batch_commits_multiple_stories_in_one_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let main = run_session(&storage_s, "agent", "run", "run");
        let sub = run_session(&storage_s, "agent", "sub", "run");
        let records = [note_line("main"), note_line("sub")]
            .iter()
            .map(|line| {
                crate::decode_event_lines(std::slice::from_ref(line))
                    .unwrap()
                    .remove(0)
            })
            .collect::<Vec<_>>();
        RawEventLanceAppender::default()
            .append_event_batch(&[
                (main.clone(), records[0].clone()),
                (sub.clone(), records[1].clone()),
            ])
            .await
            .unwrap();

        assert_eq!(replay(&main, 0, None).await.unwrap().records.len(), 1);
        assert_eq!(replay(&sub, 0, None).await.unwrap().records.len(), 1);
        let uri = raw_event_lance_path(&main)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(visible_fragment_count(&uri).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn routed_batch_creates_one_fragment_per_run_not_per_record() {
        const RUNS: usize = 3;
        const STORIES_PER_RUN: usize = 4;
        const ROWS_PER_STORY: usize = 64;

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let mut entries = Vec::with_capacity(RUNS * STORIES_PER_RUN * ROWS_PER_STORY);
        let mut run_roots = Vec::with_capacity(RUNS);

        for run_index in 0..RUNS {
            let root = format!("run-{run_index}");
            let root_session = run_session(&storage_s, "agent", &root, &root);
            run_roots.push(root_session);
            for story_index in 0..STORIES_PER_RUN {
                let session = run_session(
                    &storage_s,
                    "agent",
                    &format!("run-{run_index}-story-{story_index}"),
                    &root,
                );
                for row_index in 0..ROWS_PER_STORY {
                    entries.push((
                        session.clone(),
                        identified_note(
                            &format!("event-{run_index}-{story_index}-{row_index}"),
                            row_index as u64,
                            &format!("row-{row_index}"),
                        ),
                    ));
                }
            }
        }

        let outcome = RawEventLanceAppender::default()
            .append_event_batch(&entries)
            .await
            .unwrap();
        assert_eq!(outcome.accepted_records, entries.len());

        for root in run_roots {
            let uri = raw_event_lance_path(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
            assert_eq!(manifest.segments.len(), 1);
            assert_eq!(
                datasets[0].get_fragments().len(),
                1,
                "one routed call should create one fragment for each run dataset"
            );
            assert_eq!(
                datasets[0].count_rows(None).await.unwrap(),
                STORIES_PER_RUN * ROWS_PER_STORY
            );
        }
    }

    #[tokio::test]
    async fn explicit_maintenance_compacts_fragments_and_builds_session_index() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "run");
        for index in 0..4 {
            append(&session, &[note_line(&format!("event-{index}"))])
                .await
                .unwrap();
        }
        let report = maintain(
            &session,
            &LanceMaintenanceOptions {
                vacuum_older_than: Some(Duration::ZERO),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(report.fragments_removed >= 4);
        let uri = raw_event_lance_path(&session)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(datasets[0].get_fragments().len(), 1);
        assert!(!datasets[0]
            .load_indices_by_name(SESSION_INDEX_NAME)
            .await
            .unwrap()
            .is_empty());
        assert!(report.old_versions_removed >= 4);
        let segment_directories =
            std::fs::read_dir(raw_event_lance_path(&session).unwrap().join("segments"))
                .unwrap()
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.path().is_dir())
                .count();
        assert_eq!(segment_directories, 1);
        assert_eq!(replay(&session, 0, None).await.unwrap().records.len(), 4);
    }

    #[tokio::test]
    async fn large_append_produces_valid_lance_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "bulk");

        let lines: Vec<String> = (0..CHUNK_ROWS + 50)
            .map(|i| note_line(&format!("row-{i}")))
            .collect();
        let outcome = append(&session, &lines).await.unwrap();
        assert_eq!(outcome.persisted_units, lines.len());

        let st = stats(&session).await.unwrap();
        assert_eq!(st.row_count, lines.len());

        let replay = replay(&session, CHUNK_ROWS, Some(10)).await.unwrap();
        assert_eq!(replay.records.len(), 10);
        assert_eq!(payload_content(&replay.records[0]), "row-8192");
    }
}
