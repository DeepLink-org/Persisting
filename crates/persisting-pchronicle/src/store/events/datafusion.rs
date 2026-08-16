//! DataFusion datasource for the canonical Lance event log.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::union::UnionExec;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use futures::TryStreamExt;
use lance::deps::arrow_schema::{Schema as ArrowSchema, SchemaRef};
use lance::Dataset;

use super::manifest::EventManifest;
use crate::{event_row_to_event_record, event_rows_from_batch, EventRecord};

pub const DATAFUSION_EVENTS_TABLE: &str = "events";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEventDataSourceOptions {
    pub use_scalar_indexes: bool,
    pub scan_in_order: bool,
}

impl Default for RawEventDataSourceOptions {
    fn default() -> Self {
        Self {
            use_scalar_indexes: true,
            scan_in_order: false,
        }
    }
}

#[derive(Debug)]
pub struct RawEventTableProvider {
    datasets: Vec<Arc<Dataset>>,
    schema: SchemaRef,
    options: RawEventDataSourceOptions,
}

impl RawEventTableProvider {
    fn new(datasets: Vec<Dataset>, options: RawEventDataSourceOptions) -> Result<Self> {
        let first = datasets
            .first()
            .context("event manifest has no visible Lance segments")?;
        let schema = Arc::new(ArrowSchema::from(first.schema()));
        let datasets = datasets.into_iter().map(Arc::new).collect();
        Ok(Self {
            datasets,
            schema,
            options,
        })
    }

    pub fn datasets(&self) -> &[Arc<Dataset>] {
        &self.datasets
    }
}

#[async_trait]
impl TableProvider for RawEventTableProvider {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let mut plans = Vec::with_capacity(self.datasets.len());
        for dataset in &self.datasets {
            let mut scan = dataset.scan();
            match projection {
                Some(projection) if projection.is_empty() => {
                    scan.empty_project().map_err(DataFusionError::from)?;
                }
                Some(projection) => {
                    let columns = projection
                        .iter()
                        .map(|index| self.schema.field(*index).name())
                        .collect::<Vec<_>>();
                    scan.project(&columns).map_err(DataFusionError::from)?;
                }
                None => {}
            }
            if let Some(filter) = combine_filters(filters) {
                scan.filter_expr(filter);
            }
            scan.scan_in_order(self.options.scan_in_order);
            scan.use_scalar_index(self.options.use_scalar_indexes);
            plans.push(scan.create_plan().await.map_err(DataFusionError::from)?);
        }
        let union = UnionExec::try_new(plans)?;
        Ok(match limit {
            Some(limit) => Arc::new(GlobalLimitExec::new(union, 0, Some(limit))),
            None => union,
        })
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|_| TableProviderFilterPushDown::Exact)
            .collect())
    }
}

#[derive(Debug)]
pub struct RawEventDataSource {
    uri: String,
    snapshot: EventFactSnapshot,
    provider: Arc<RawEventTableProvider>,
    segment_rows: Vec<u64>,
}

/// Stable logical and physical coordinates for one pinned canonical event view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventFactSnapshot {
    pub source_uri: String,
    pub fact_version: u64,
    pub fact_rows: u64,
    pub layout_revision: u64,
}

/// A manifest-only canonical event snapshot. It pins visible segment versions
/// without opening any Lance dataset until a selected catalog scan needs it.
#[derive(Debug, Clone)]
pub(crate) struct RawEventSnapshot {
    uri: String,
    manifest: EventManifest,
}

impl RawEventSnapshot {
    pub(crate) fn uri(&self) -> &str {
        &self.uri
    }

    pub(crate) fn fact_snapshot(&self) -> EventFactSnapshot {
        EventFactSnapshot {
            source_uri: self.uri.clone(),
            fact_version: self.manifest.fact_version,
            fact_rows: self.manifest.fact_rows,
            layout_revision: self.manifest.revision,
        }
    }
}

impl RawEventDataSource {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let uri = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("event Lance path is not valid UTF-8"))?;
        Self::open_uri(uri).await
    }

    pub async fn open_uri(uri: impl AsRef<str>) -> Result<Self> {
        Self::open_uri_with_options(uri, RawEventDataSourceOptions::default()).await
    }

    pub async fn open_uri_with_options(
        uri: impl AsRef<str>,
        options: RawEventDataSourceOptions,
    ) -> Result<Self> {
        let snapshot = Self::pin_uri(uri).await?;
        Self::from_pinned_snapshot_with_options(snapshot, options).await
    }

    pub(crate) async fn pin_uri(uri: impl AsRef<str>) -> Result<RawEventSnapshot> {
        let uri = if uri.as_ref().contains("://") {
            uri.as_ref().to_string()
        } else {
            std::fs::canonicalize(uri.as_ref())
                .with_context(|| format!("canonicalize canonical event source {}", uri.as_ref()))?
                .to_string_lossy()
                .into_owned()
        };
        let manifest = super::pin_visible_snapshot(&uri)
            .await?
            .with_context(|| format!("canonical event manifest does not exist at {uri}"))?;
        anyhow::ensure!(
            !manifest.segments.is_empty(),
            "canonical event manifest has no visible segments at {uri}"
        );
        Ok(RawEventSnapshot { uri, manifest })
    }

    pub(crate) async fn from_pinned_snapshot_with_options(
        snapshot: RawEventSnapshot,
        options: RawEventDataSourceOptions,
    ) -> Result<Self> {
        let segment_rows = snapshot
            .manifest
            .segments
            .iter()
            .map(|segment| segment.rows)
            .collect::<Vec<_>>();
        let datasets = super::open_pinned_snapshot(snapshot.uri(), &snapshot.manifest).await?;
        anyhow::ensure!(
            datasets.len() == segment_rows.len(),
            "canonical event manifest segment count does not match opened datasets"
        );
        let fact_snapshot = snapshot.fact_snapshot();
        Ok(Self {
            uri: snapshot.uri,
            snapshot: fact_snapshot,
            provider: Arc::new(RawEventTableProvider::new(datasets, options)?),
            segment_rows,
        })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn version(&self) -> u64 {
        self.snapshot.layout_revision
    }

    pub fn fact_snapshot(&self) -> &EventFactSnapshot {
        &self.snapshot
    }

    pub fn provider(&self) -> Arc<RawEventTableProvider> {
        self.provider.clone()
    }

    pub fn register(&self, context: &SessionContext) -> Result<()> {
        self.register_as(context, DATAFUSION_EVENTS_TABLE)
    }

    pub fn register_as(&self, context: &SessionContext, table_name: &str) -> Result<()> {
        anyhow::ensure!(
            !table_name.trim().is_empty(),
            "DataFusion event table name must not be empty"
        );
        context
            .register_table(table_name, self.provider.clone())
            .with_context(|| format!("register DataFusion table '{table_name}'"))?;
        Ok(())
    }

    pub fn session_context(&self) -> Result<SessionContext> {
        let context = SessionContext::new();
        self.register(&context)?;
        Ok(context)
    }

    /// Read a pinned source in manifest segment and physical append order.
    pub async fn read_records_in_append_order(&self) -> Result<Vec<EventRecord>> {
        self.read_records_range_in_append_order(0, self.snapshot.fact_rows)
            .await
    }

    /// Read the half-open logical append range `[start, end)` from one pinned
    /// manifest without scanning preceding or following event rows.
    pub async fn read_records_range_in_append_order(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Vec<EventRecord>> {
        anyhow::ensure!(start <= end, "event append range start exceeds end");
        anyhow::ensure!(
            end <= self.snapshot.fact_rows,
            "event append range end {end} exceeds pinned fact rows {}",
            self.snapshot.fact_rows
        );
        if start == end {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        let mut segment_start = 0u64;
        for (dataset, segment_rows) in self
            .provider
            .datasets()
            .iter()
            .zip(self.segment_rows.iter().copied())
        {
            let segment_end = segment_start
                .checked_add(segment_rows)
                .context("event segment row range overflow")?;
            let overlap_start = start.max(segment_start);
            let overlap_end = end.min(segment_end);
            if overlap_start >= overlap_end {
                segment_start = segment_end;
                continue;
            }
            let offset = overlap_start - segment_start;
            let limit = overlap_end - overlap_start;
            let mut scan = dataset.scan();
            scan.scan_in_order(true);
            scan.limit(
                Some(i64::try_from(limit).context("event range limit exceeds i64")?),
                (offset > 0)
                    .then(|| i64::try_from(offset).context("event range offset exceeds i64"))
                    .transpose()?,
            )
            .context("apply pinned event append range")?;
            let batches = scan
                .try_into_stream()
                .await
                .context("scan pinned canonical events in append order")?
                .try_collect::<Vec<_>>()
                .await
                .context("collect pinned canonical events in append order")?;
            for batch in &batches {
                for row in event_rows_from_batch(batch)? {
                    records.push(event_row_to_event_record(&row)?);
                }
            }
            segment_start = segment_end;
        }
        anyhow::ensure!(
            records.len() == usize::try_from(end - start)?,
            "pinned event append range returned {} rows; expected {}",
            records.len(),
            end - start
        );
        Ok(records)
    }

    /// Read complete append-ordered histories for selected Storyline identities
    /// from the same pinned manifest used for suffix discovery.
    pub async fn read_records_for_storylines(
        &self,
        session_ids: &BTreeSet<String>,
    ) -> Result<Vec<EventRecord>> {
        self.read_records_inner(Some(session_ids), None, None).await
    }

    /// Read selected Storyline histories while enforcing Catalog fallback
    /// budgets before retaining decoded events in memory.
    pub(crate) async fn read_records_for_storylines_bounded(
        &self,
        session_ids: &BTreeSet<String>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<EventRecord>> {
        anyhow::ensure!(max_rows > 0, "event fallback max_rows must be positive");
        anyhow::ensure!(max_bytes > 0, "event fallback max_bytes must be positive");
        self.read_records_inner(Some(session_ids), Some(max_rows), Some(max_bytes))
            .await
    }

    /// Read the complete pinned event snapshot with hard Catalog fallback
    /// limits.
    pub(crate) async fn read_records_bounded(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<EventRecord>> {
        anyhow::ensure!(max_rows > 0, "event fallback max_rows must be positive");
        anyhow::ensure!(max_bytes > 0, "event fallback max_bytes must be positive");
        self.read_records_inner(None, Some(max_rows), Some(max_bytes))
            .await
    }

    async fn read_records_inner(
        &self,
        session_ids: Option<&BTreeSet<String>>,
        max_rows: Option<usize>,
        max_bytes: Option<usize>,
    ) -> Result<Vec<EventRecord>> {
        if session_ids.is_some_and(BTreeSet::is_empty) {
            return Ok(Vec::new());
        }
        let predicate = session_ids.map(|session_ids| {
            format!(
                "session_id IN ({})",
                session_ids
                    .iter()
                    .map(|id| format!("'{}'", id.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        });
        let mut records = Vec::new();
        let mut retained_bytes = 0usize;
        for dataset in self.provider.datasets() {
            let mut scan = dataset.scan();
            if let Some(predicate) = &predicate {
                scan.filter(predicate)
                    .context("filter pinned events by Storyline identity")?;
            }
            scan.scan_in_order(true);
            scan.use_scalar_index(true);
            if let Some(max_bytes) = max_bytes {
                scan.batch_size_bytes(u64::try_from(max_bytes.min(8 * 1024 * 1024))?);
            }
            let mut batches = scan
                .try_into_stream()
                .await
                .context("scan pinned Storyline event histories")?;
            while let Some(batch) = batches
                .try_next()
                .await
                .context("read pinned Storyline event history batch")?
            {
                retained_bytes = retained_bytes
                    .checked_add(batch.get_array_memory_size())
                    .context("event fallback retained byte count overflow")?;
                if let Some(max_bytes) = max_bytes {
                    anyhow::ensure!(
                        retained_bytes <= max_bytes,
                        "canonical event fallback exceeds max_event_fallback_bytes {max_bytes}; build or sync a Storyline projection"
                    );
                }
                let rows = event_rows_from_batch(&batch)?;
                if let Some(max_rows) = max_rows {
                    anyhow::ensure!(
                        records.len().saturating_add(rows.len()) <= max_rows,
                        "canonical event fallback exceeds max_event_fallback_rows {max_rows}; build or sync a Storyline projection"
                    );
                }
                for row in rows {
                    records.push(event_row_to_event_record(&row)?);
                }
            }
        }
        if let Some(session_ids) = session_ids {
            anyhow::ensure!(
                records.iter().all(|record| record
                    .session_id
                    .as_ref()
                    .is_some_and(|id| session_ids.contains(id))),
                "Storyline event history scan returned an unrequested identity"
            );
        }
        Ok(records)
    }
}

fn combine_filters(filters: &[Expr]) -> Option<Expr> {
    let mut filters = filters.iter().cloned();
    let first = filters.next()?;
    Some(filters.fold(first, Expr::and))
}
