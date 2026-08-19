//! Lance event log backend for the canonical trajectory store.
//!
//! Capture runs use one run-level manifest at `{run}/events.lance`; each writer
//! epoch owns one private Lance segment and `session_id` filters rows when
//! replaying one story view.

pub(super) mod datafusion;
mod manifest;
mod rows;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::EventRow;
use ::datafusion::error::DataFusionError;
use ::datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use ::datafusion::physical_plan::SendableRecordBatchStream;
use anyhow::{Context, Result};
use futures::{StreamExt, TryStreamExt};
use lance::dataset::optimize::{compact_files, CompactionOptions};
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{Array, RecordBatch};
use lance::index::DatasetIndexExt;
use lance::Dataset;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;

pub use self::datafusion::{EventFactSnapshot, RawEventDataSource, DATAFUSION_EVENTS_TABLE};
use self::manifest as raw_event_manifest;
pub use self::manifest::EventWriterFence;
use self::manifest::{EventManifest, EventSegment, EventWriterConflict, ManifestWriteOutcome};
pub use self::rows::{
    event_records_from_batch, event_rows_from_batch, event_rows_to_batch, raw_event_arrow_schema,
};
use self::rows::{event_row_for_storage, schema_columns_note};
use super::{
    dataset_write_lock, raw_event_lance_path, AppendOutcome, ReplayOutcome, StoryCoords,
    TrajectoryStats,
};

const SESSION_INDEX_NAME: &str = "pchronicle_session_id_idx";
const MAX_SEGMENT_OPEN_CONCURRENCY: usize = 16;
const MAX_APPEND_GROUP_CONCURRENCY: usize = 16;

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
    pub max_segment_level: u8,
    pub sealed_segments: usize,
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
    pending_fragments: usize,
    manifest_revision: u64,
}

#[derive(Debug, Default)]
pub(crate) struct EventAppendBatchReport {
    outcomes: BTreeMap<String, Result<usize>>,
}

impl EventAppendBatchReport {
    #[cfg(test)]
    pub(crate) fn outcome_for(&self, root_uri: &str) -> Option<&Result<usize>> {
        self.outcomes.get(root_uri)
    }

    pub(crate) fn take_outcome(&mut self, root_uri: &str) -> Option<Result<usize>> {
        self.outcomes.remove(root_uri)
    }

    fn accepted_records(&self) -> usize {
        self.outcomes
            .values()
            .filter_map(|outcome| outcome.as_ref().ok())
            .sum()
    }

    fn take_failure(&mut self) -> Option<(String, anyhow::Error)> {
        let uri = self
            .outcomes
            .iter()
            .find(|(_, outcome)| outcome.is_err())
            .map(|(uri, _)| uri.clone())?;
        match self.outcomes.remove(&uri)? {
            Err(error) => Some((uri, error)),
            Ok(records) => {
                self.outcomes.insert(uri, Ok(records));
                None
            }
        }
    }
}

/// Immutable writer segment handed to background tiny-fragment maintenance.
#[derive(Debug)]
pub(crate) struct SealedEventSegment {
    root_uri: String,
    fence: EventWriterFence,
    segment: EventSegment,
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
    pub async fn activate(&mut self, session: &StoryCoords) -> Result<EventWriterFence> {
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
        let manifest = manifest_write_applied(
            raw_event_manifest::activate(uri, self.requested_fence.as_ref(), &self.auto_writer_id)
                .await?,
        )?;
        let fence = manifest.active_writer.clone();
        let segment_id = format!("e{}-{}", fence.epoch, uuid::Uuid::new_v4());
        Ok(CachedRawDataset {
            fence,
            segment_uri: raw_event_manifest::segment_uri(uri, &segment_id),
            segment_id,
            dataset: None,
            rows: 0,
            pending_fragments: 0,
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
        entries: &[(StoryCoords, crate::EventRecord)],
    ) -> Result<AppendOutcome> {
        let mut report = self.append_event_batch_partitioned(entries).await?;
        if let Some((uri, error)) = report.take_failure() {
            return Err(error.context(format!("append canonical event partition {uri}")));
        }
        let accepted_records = report.accepted_records();
        Ok(AppendOutcome {
            accepted_records,
            persisted_units: accepted_records,
            note: format!("Lance: committed {accepted_records} event(s) in a micro-batch"),
        })
    }

    /// Append independent event-log roots concurrently and retain a result for
    /// every root so capture acknowledgements do not share one failure domain.
    pub(crate) async fn append_event_batch_partitioned(
        &mut self,
        entries: &[(StoryCoords, crate::EventRecord)],
    ) -> Result<EventAppendBatchReport> {
        if entries.is_empty() {
            return Ok(EventAppendBatchReport::default());
        }
        let mut groups: BTreeMap<String, Vec<(StoryCoords, crate::EventRecord)>> = BTreeMap::new();
        for (session, record) in entries {
            let uri = raw_event_lance_path(session)?
                .to_string_lossy()
                .into_owned();
            groups
                .entry(uri)
                .or_default()
                .push((session.clone(), record.clone()));
        }

        let mut report = EventAppendBatchReport::default();
        let mut pending = Vec::with_capacity(groups.len());
        for (uri, entries) in groups {
            let rows = entries
                .into_iter()
                .map(|(session, record)| {
                    let record = super::canonicalize_event(&session, record)?;
                    event_row_for_storage(&session.session_id, &session.agent_id, &record)
                })
                .collect::<Result<Vec<_>>>();
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) => {
                    report.outcomes.insert(uri, Err(error));
                    continue;
                }
            };
            let state = match self.datasets.remove(&uri) {
                Some(state) => state,
                None => match self.new_state(&uri).await {
                    Ok(state) => state,
                    Err(error) => {
                        report.outcomes.insert(uri, Err(error));
                        continue;
                    }
                },
            };
            pending.push((uri, state, rows));
        }

        // Every URI owns an independent Lance segment and manifest. Appending
        // them sequentially made one slow session hold the completion for the
        // entire cross-session micro-batch and indirectly starved Gateway's
        // capture actors. Preserve per-URI ordering across batches while
        // allowing independent sessions in this batch to progress concurrently.
        let completed = futures::stream::iter(pending)
            .map(|(uri, state, rows)| {
                let outcome_uri = uri.clone();
                async move { (outcome_uri, append_event_group(uri, state, rows).await) }
            })
            .buffer_unordered(MAX_APPEND_GROUP_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for (uri, outcome) in completed {
            match outcome {
                Ok((_returned_uri, state, appended_rows)) => {
                    self.datasets.insert(uri.clone(), state);
                    report.outcomes.insert(uri, Ok(appended_rows));
                }
                Err(error) => {
                    report.outcomes.insert(uri, Err(error));
                }
            }
        }
        Ok(report)
    }

    /// Seal active segments that have accumulated enough micro-append
    /// fragments. Future appends immediately move to a new private segment, so
    /// the sealed segment can be compacted concurrently without commit races.
    pub(crate) fn seal_fragmented_segments(
        &mut self,
        fragment_threshold: usize,
    ) -> Result<Vec<SealedEventSegment>> {
        anyhow::ensure!(
            fragment_threshold > 1,
            "fragment sealing threshold must be greater than one"
        );

        let mut sealed = Vec::new();
        for (uri, state) in &mut self.datasets {
            if state.pending_fragments < fragment_threshold {
                continue;
            }
            let dataset = state
                .dataset
                .take()
                .context("fragmented event segment is missing its Lance dataset")?;
            sealed.push(SealedEventSegment {
                root_uri: uri.clone(),
                fence: state.fence.clone(),
                segment: EventSegment {
                    id: state.segment_id.clone(),
                    version: dataset.version_id(),
                    rows: state.rows,
                    level: 0,
                    sealed: false,
                },
            });

            state.segment_id = format!("e{}-{}", state.fence.epoch, uuid::Uuid::new_v4());
            state.segment_uri = raw_event_manifest::segment_uri(uri, &state.segment_id);
            state.rows = 0;
            state.pending_fragments = 0;
        }
        Ok(sealed)
    }

    /// Close the writer without indexing, full compaction, vacuum, or any other
    /// synchronous maintenance. The append worker already performs bounded
    /// tiny-fragment compaction; heavy layout work remains available through
    /// the explicit [`maintain`] API.
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

async fn append_event_group(
    uri: String,
    mut state: CachedRawDataset,
    rows: Vec<EventRow>,
) -> Result<(String, CachedRawDataset, usize)> {
    let appended_rows = rows.len();
    let dataset = append_rows(&state.segment_uri, state.dataset.take(), rows).await?;
    state.rows = state
        .rows
        .checked_add(appended_rows as u64)
        .context("event segment row count overflow")?;
    state.pending_fragments = state.pending_fragments.saturating_add(1);
    let manifest = manifest_write_applied(
        raw_event_manifest::publish_segment(
            &uri,
            &state.fence,
            EventSegment {
                id: state.segment_id.clone(),
                version: dataset.version_id(),
                rows: state.rows,
                level: 0,
                sealed: false,
            },
        )
        .await?,
    )?;
    state.manifest_revision = manifest.revision;
    state.dataset = Some(dataset);
    Ok((uri, state, appended_rows))
}

/// Compact one immutable segment and atomically advance its outer visibility
/// pointer. A crash before publication leaves the original segment version
/// visible; a newer writer fence rejects the stale publication.
pub(crate) async fn compact_sealed_event_segment(
    sealed: SealedEventSegment,
    target_rows_per_fragment: usize,
    hierarchy_fanout: usize,
) -> Result<()> {
    anyhow::ensure!(
        target_rows_per_fragment > 0,
        "fragment compaction target rows must be greater than zero"
    );
    anyhow::ensure!(
        hierarchy_fanout > 1,
        "fragment hierarchy fanout must be greater than one"
    );
    let _guard = dataset_write_lock::acquire(&sealed.root_uri).await?;
    let segment_uri = raw_event_manifest::segment_uri(&sealed.root_uri, &sealed.segment.id);
    let mut dataset = Dataset::open(&segment_uri)
        .await
        .with_context(|| format!("open sealed event segment {segment_uri}"))?;
    let metrics = compact_files(
        &mut dataset,
        CompactionOptions {
            target_rows_per_fragment,
            max_rows_per_group: target_rows_per_fragment.min(1024 * 1024),
            num_threads: Some(1),
            ..Default::default()
        },
        None,
    )
    .await
    .with_context(|| format!("compact sealed trajectory segment {segment_uri}"))?;
    let mut published_segment = sealed.segment;
    if metrics.fragments_removed > 0 {
        published_segment.version = dataset.version_id();
    }
    published_segment.sealed = true;
    manifest_write_applied(
        raw_event_manifest::publish_segment(&sealed.root_uri, &sealed.fence, published_segment)
            .await?,
    )?;
    compact_event_hierarchy_locked(&sealed.root_uri, &sealed.fence, hierarchy_fanout).await?;
    Ok(())
}

fn next_hierarchy_group(manifest: &EventManifest, fanout: usize) -> Option<Vec<EventSegment>> {
    manifest
        .segments
        .windows(fanout)
        .find(|segments| {
            let level = segments[0].level;
            segments
                .iter()
                .all(|segment| segment.sealed && segment.level == level)
        })
        .map(<[EventSegment]>::to_vec)
}

async fn compact_event_hierarchy_locked(
    root_uri: &str,
    fence: &EventWriterFence,
    fanout: usize,
) -> Result<()> {
    loop {
        let manifest = raw_event_manifest::read(root_uri)
            .await?
            .context("event manifest disappeared during hierarchical compaction")?;
        let Some(group) = next_hierarchy_group(&manifest, fanout) else {
            return Ok(());
        };
        let next_level = group[0]
            .level
            .checked_add(1)
            .context("event segment compaction level overflow")?;
        let datasets = futures::stream::iter(group.iter().cloned())
            .map(|segment| async move { open_visible_segment(root_uri, &segment).await })
            .buffered(MAX_SEGMENT_OPEN_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;
        let rows = group.iter().try_fold(0_u64, |total, segment| {
            total
                .checked_add(segment.rows)
                .context("hierarchical event segment row count overflow")
        })?;
        let target_rows = usize::try_from(rows)
            .context("hierarchical event segment is too large for this platform")?
            .max(1);
        let (dataset, mut replacement) =
            compact_visible_segments(root_uri, datasets, target_rows, false).await?;
        replacement.version = dataset.version_id();
        replacement.level = next_level;
        replacement.sealed = true;
        manifest_write_applied(
            raw_event_manifest::replace_segment_group(root_uri, fence, &group, replacement).await?,
        )?;
    }
}

pub(super) fn validate_event_schema(dataset: &Dataset, uri: &str) -> Result<()> {
    let actual = lance::deps::arrow_schema::Schema::from(dataset.schema());
    let expected = raw_event_arrow_schema();
    anyhow::ensure!(
        actual.fields() == expected.fields(),
        "trajectory dataset schema mismatch at {uri}: expected {expected:?}, found {actual:?}"
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

async fn open_visible_snapshot(root_uri: &str) -> Result<Option<(EventManifest, Vec<Dataset>)>> {
    let Some(manifest) = pin_visible_snapshot(root_uri).await? else {
        return Ok(None);
    };
    let datasets = open_pinned_snapshot(root_uri, &manifest).await?;
    Ok(Some((manifest, datasets)))
}

/// Read only the small visibility manifest. The returned segment versions are
/// the immutable catalog descriptor used for later lazy resolution.
async fn pin_visible_snapshot(root_uri: &str) -> Result<Option<EventManifest>> {
    raw_event_manifest::read(root_uri).await
}

/// Open exactly the segment versions captured by
/// [`pin_visible_snapshot`], without consulting the latest manifest again.
async fn open_pinned_snapshot(root_uri: &str, manifest: &EventManifest) -> Result<Vec<Dataset>> {
    let datasets = futures::stream::iter(manifest.segments.iter().cloned())
        .map(|segment| async move { open_visible_segment(root_uri, &segment).await })
        .buffered(MAX_SEGMENT_OPEN_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    Ok(datasets)
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
            level: 0,
            sealed: true,
        },
    ))
}

pub async fn maintain(
    session: &StoryCoords,
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
    let active = manifest_write_applied(
        raw_event_manifest::activate(&uri, None, &maintenance_writer_id).await?,
    )?;
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
        let published = manifest_write_applied(
            raw_event_manifest::replace_segments(&uri, &fence, vec![segment]).await?,
        )?;
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
        let published = manifest_write_applied(
            raw_event_manifest::replace_segments(&uri, &fence, updated_segments).await?,
        )?;
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

pub fn display_path(session: &StoryCoords) -> Result<String> {
    Ok(raw_event_lance_path(session)?
        .to_string_lossy()
        .into_owned())
}

pub async fn distinct_session_ids_in_run(run: &StoryCoords) -> Result<Vec<String>> {
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

pub async fn exists(session: &StoryCoords) -> Result<bool> {
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

pub async fn append_events(
    session: &StoryCoords,
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

fn manifest_write_applied<T>(outcome: ManifestWriteOutcome<T>) -> Result<T> {
    match outcome {
        ManifestWriteOutcome::Applied(value) => Ok(value),
        ManifestWriteOutcome::Conflict(EventWriterConflict::StaleFence) => {
            anyhow::bail!("stale event writer fence")
        }
        ManifestWriteOutcome::Conflict(EventWriterConflict::EpochAlreadyOwned) => {
            anyhow::bail!("event writer epoch is already owned")
        }
        ManifestWriteOutcome::Conflict(EventWriterConflict::PublicationChanged) => {
            anyhow::bail!("event manifest publication changed")
        }
    }
}

pub async fn replay(
    session: &StoryCoords,
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
    session: &StoryCoords,
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
    let records = event_records_from_batch(&batch)?;
    Ok(Some(ReplayOutcome {
        records,
        note: format!(
            "Replay fenced Lance manifest revision {} at {uri}: session_id={}, ordered by immutable segment and append order, offset={offset}, limit={limit:?}.",
            manifest.revision, session.session_id,
        ),
    }))
}

pub async fn stats(session: &StoryCoords) -> Result<TrajectoryStats> {
    let path = raw_event_lance_path(session)?;
    let display = path.to_string_lossy().into_owned();
    let Some((manifest, datasets)) = open_visible_snapshot(&display).await? else {
        return Ok(TrajectoryStats {
            dataset: display,
            row_count: 0,
            manifest_revision: None,
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
        manifest_revision: Some(manifest.revision),
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

pub async fn layout_stats(session: &StoryCoords) -> Result<EventLogLayoutStats> {
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
        max_segment_level: manifest
            .segments
            .iter()
            .map(|segment| segment.level)
            .max()
            .unwrap_or(0),
        sealed_segments: manifest
            .segments
            .iter()
            .filter(|segment| segment.sealed)
            .count(),
    })
}

#[cfg(test)]
mod tests;
