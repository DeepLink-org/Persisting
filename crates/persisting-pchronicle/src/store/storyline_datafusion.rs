//! DataFusion datasource for one committed Storyline Lance generation.
//!
//! Opening the datasource resolves `CURRENT` once and pins all three datasets
//! to that generation. pChronicle builds the Lance scan with projection,
//! filter, limit and scalar-index pushdown, plus unordered fragment reads for
//! parallel query execution (ordered queries should use SQL `ORDER BY`).

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use lance::deps::arrow_schema::{Schema as ArrowSchema, SchemaRef};
use lance::Dataset;

use super::{LanceStorylineStore, StorylineTablePaths};

pub const DATAFUSION_RUNS_TABLE: &str = "runs";
pub const DATAFUSION_STEPS_TABLE: &str = "steps";
pub const DATAFUSION_TOOL_CALLS_TABLE: &str = "tool_calls";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorylineTableKind {
    Runs,
    Steps,
    ToolCalls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorylineDataSourceOptions {
    /// Use Lance scalar indices for pushed-down filters.
    pub use_scalar_indexes: bool,
    /// Preserve physical fragment order. Disabled by default for parallelism.
    pub scan_in_order: bool,
}

impl Default for StorylineDataSourceOptions {
    fn default() -> Self {
        Self {
            use_scalar_indexes: true,
            scan_in_order: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorylineDataFusionTableNames {
    pub runs: String,
    pub steps: String,
    pub tool_calls: String,
}

impl Default for StorylineDataFusionTableNames {
    fn default() -> Self {
        Self {
            runs: DATAFUSION_RUNS_TABLE.into(),
            steps: DATAFUSION_STEPS_TABLE.into(),
            tool_calls: DATAFUSION_TOOL_CALLS_TABLE.into(),
        }
    }
}

/// pChronicle-specific wrapper over Lance's native DataFusion provider.
///
/// The wrapper intentionally disables ordered Lance scans. Storyline ordering
/// is logical (`step_id`, `call_index`) and should be requested explicitly;
/// unordered scans allow DataFusion to consume fragments in parallel.
#[derive(Debug)]
pub struct StorylineTableProvider {
    kind: StorylineTableKind,
    dataset: Arc<Dataset>,
    schema: SchemaRef,
    options: StorylineDataSourceOptions,
}

impl StorylineTableProvider {
    fn new(
        kind: StorylineTableKind,
        dataset: Arc<Dataset>,
        options: StorylineDataSourceOptions,
    ) -> Self {
        let schema = Arc::new(ArrowSchema::from(dataset.schema()));
        Self {
            kind,
            dataset,
            schema,
            options,
        }
    }

    pub fn kind(&self) -> StorylineTableKind {
        self.kind
    }

    pub fn dataset(&self) -> Arc<Dataset> {
        self.dataset.clone()
    }
}

#[async_trait]
impl TableProvider for StorylineTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

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
        let mut scan = self.dataset.scan();
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
        scan.limit(limit.map(|value| value as i64), None)
            .map_err(DataFusionError::from)?;
        scan.scan_in_order(self.options.scan_in_order);
        scan.use_scalar_index(self.options.use_scalar_indexes);
        scan.create_plan().await.map_err(DataFusionError::from)
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

/// Snapshot-consistent DataFusion datasource for the three Storyline tables.
#[derive(Debug)]
pub struct StorylineDataSource {
    paths: StorylineTablePaths,
    runs: Arc<StorylineTableProvider>,
    steps: Arc<StorylineTableProvider>,
    tool_calls: Arc<StorylineTableProvider>,
}

impl StorylineDataSource {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        let store = LanceStorylineStore::open(root).await?;
        Self::from_store(&store).await
    }

    pub async fn open_with_options(
        root: impl AsRef<Path>,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let store = LanceStorylineStore::open(root).await?;
        Self::from_store_with_options(&store, options).await
    }

    pub async fn open_uri(root: impl AsRef<str>) -> Result<Self> {
        let store = LanceStorylineStore::open_uri(root).await?;
        Self::from_store(&store).await
    }

    pub async fn open_uri_with_options(
        root: impl AsRef<str>,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let store = LanceStorylineStore::open_uri(root).await?;
        Self::from_store_with_options(&store, options).await
    }

    pub async fn from_store(store: &LanceStorylineStore) -> Result<Self> {
        Self::from_store_with_options(store, StorylineDataSourceOptions::default()).await
    }

    pub async fn from_store_with_options(
        store: &LanceStorylineStore,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let paths = store
            .current_table_paths()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance store has no committed generation"))?;
        let (runs, steps, tool_calls) = tokio::try_join!(
            open_dataset(&paths.runs),
            open_dataset(&paths.steps),
            open_dataset(&paths.tool_calls)
        )?;
        Ok(Self {
            paths,
            runs: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::Runs,
                Arc::new(runs),
                options,
            )),
            steps: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::Steps,
                Arc::new(steps),
                options,
            )),
            tool_calls: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::ToolCalls,
                Arc::new(tool_calls),
                options,
            )),
        })
    }

    pub fn generation(&self) -> &str {
        &self.paths.generation
    }

    pub fn paths(&self) -> &StorylineTablePaths {
        &self.paths
    }

    pub fn provider(&self, kind: StorylineTableKind) -> Arc<StorylineTableProvider> {
        match kind {
            StorylineTableKind::Runs => self.runs.clone(),
            StorylineTableKind::Steps => self.steps.clone(),
            StorylineTableKind::ToolCalls => self.tool_calls.clone(),
        }
    }

    pub fn register(&self, context: &SessionContext) -> Result<()> {
        self.register_as(context, &StorylineDataFusionTableNames::default())
    }

    pub fn register_as(
        &self,
        context: &SessionContext,
        names: &StorylineDataFusionTableNames,
    ) -> Result<()> {
        validate_table_names(names)?;
        context
            .register_table(&names.runs, self.runs.clone())
            .with_context(|| format!("register DataFusion table '{}'", names.runs))?;
        context
            .register_table(&names.steps, self.steps.clone())
            .with_context(|| format!("register DataFusion table '{}'", names.steps))?;
        context
            .register_table(&names.tool_calls, self.tool_calls.clone())
            .with_context(|| format!("register DataFusion table '{}'", names.tool_calls))?;
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

async fn open_dataset(path: &Path) -> Result<Dataset> {
    Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline DataFusion table {}", path.display()))
}

fn validate_table_names(names: &StorylineDataFusionTableNames) -> Result<()> {
    let values = [&names.runs, &names.steps, &names.tool_calls];
    if values.iter().any(|name| name.trim().is_empty()) {
        anyhow::bail!("DataFusion table names must not be empty");
    }
    if names.runs == names.steps
        || names.runs == names.tool_calls
        || names.steps == names.tool_calls
    {
        anyhow::bail!("DataFusion table names must be distinct");
    }
    Ok(())
}
