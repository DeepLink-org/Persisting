//! DataFusion datasource for the canonical Lance event log.

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
use lance::deps::arrow_schema::{Schema as ArrowSchema, SchemaRef};
use lance::Dataset;

use super::raw_event_manifest::EventManifest;

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
    version: u64,
    provider: Arc<RawEventTableProvider>,
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

    pub(crate) fn version(&self) -> u64 {
        self.manifest.revision
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
        let uri = uri.as_ref().to_string();
        let manifest = super::raw_event_lance::pin_visible_snapshot(&uri)
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
        let datasets =
            super::raw_event_lance::open_pinned_snapshot(snapshot.uri(), &snapshot.manifest)
                .await?;
        let version = snapshot.version();
        Ok(Self {
            uri: snapshot.uri,
            version,
            provider: Arc::new(RawEventTableProvider::new(datasets, options)?),
        })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn version(&self) -> u64 {
        self.version
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
}

fn combine_filters(filters: &[Expr]) -> Option<Expr> {
    let mut filters = filters.iter().cloned();
    let first = filters.next()?;
    Some(filters.fold(first, Expr::and))
}
