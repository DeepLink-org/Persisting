//! DataFusion datasource for one committed Storyline Lance snapshot.
//!
//! Opening the datasource resolves `CURRENT` once and pins all three datasets
//! to the exact version tuple recorded there. pChronicle builds the Lance scan with projection,
//! filter, limit and scalar-index pushdown, plus unordered fragment reads for
//! parallel query execution (ordered queries should use SQL `ORDER BY`).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::execution::TaskContext;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::execution_plan::PlanProperties;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, ExecutionPlan, SendableRecordBatchStream,
};
use datafusion::prelude::SessionContext;
use futures::StreamExt;
use lance::deps::arrow_schema::{Schema as ArrowSchema, SchemaRef};
use lance::Dataset;

use super::storyline_content::{
    content_columns, hydrate_selected_batches, open_objects, preview_selected_batches,
};
use super::{StorylineLanceStore, StorylineTablePaths};

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
pub enum StorylineContentReadMode {
    /// Return complete content and perform Blob reads only for referenced columns.
    Full,
    /// Return descriptor previews without reading Blob payloads.
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorylineDataSourceOptions {
    /// Use Lance scalar indices for pushed-down filters.
    pub use_scalar_indexes: bool,
    /// Preserve physical fragment order. Disabled by default for parallelism.
    pub scan_in_order: bool,
    /// Choose full late materialization or metadata-only preview output.
    pub content_read_mode: StorylineContentReadMode,
}

impl Default for StorylineDataSourceOptions {
    fn default() -> Self {
        Self {
            use_scalar_indexes: true,
            scan_in_order: false,
            content_read_mode: StorylineContentReadMode::Full,
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
    objects: Arc<Dataset>,
    schema: SchemaRef,
    options: StorylineDataSourceOptions,
}

impl StorylineTableProvider {
    fn new(
        kind: StorylineTableKind,
        dataset: Arc<Dataset>,
        objects: Arc<Dataset>,
        options: StorylineDataSourceOptions,
    ) -> Self {
        let schema = Arc::new(ArrowSchema::from(dataset.schema()));
        Self {
            kind,
            dataset,
            objects,
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
        let safe_filters = filters
            .iter()
            .filter(|filter| !filter_uses_content(self.kind, filter))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(filter) = combine_filters(&safe_filters) {
            scan.filter_expr(filter);
        }
        let has_content_filter = filters
            .iter()
            .any(|filter| filter_uses_content(self.kind, filter));
        if has_content_filter && self.options.content_read_mode == StorylineContentReadMode::Preview
        {
            return Err(DataFusionError::Plan(
                "content predicates require full Storyline content mode".into(),
            ));
        }
        scan.limit(
            (!has_content_filter)
                .then_some(limit)
                .flatten()
                .map(|value| value as i64),
            None,
        )
        .map_err(DataFusionError::from)?;
        scan.scan_in_order(self.options.scan_in_order);
        scan.use_scalar_index(self.options.use_scalar_indexes);
        let plan = scan.create_plan().await.map_err(DataFusionError::from)?;
        let selected = selected_content_columns(self.kind, projection, &self.schema);
        if selected.is_empty() {
            Ok(plan)
        } else {
            Ok(Arc::new(ContentHydrationExec::new(
                plan,
                selected,
                match self.options.content_read_mode {
                    StorylineContentReadMode::Full => {
                        ContentMaterializationMode::Full(self.objects.clone())
                    }
                    StorylineContentReadMode::Preview => ContentMaterializationMode::Preview,
                },
            )))
        }
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|filter| {
                if filter_uses_content(self.kind, filter) {
                    match self.options.content_read_mode {
                        StorylineContentReadMode::Full => TableProviderFilterPushDown::Unsupported,
                        // Preview mode deliberately accepts the predicate into
                        // `scan` so it can fail closed instead of evaluating it
                        // against truncated values above the provider.
                        StorylineContentReadMode::Preview => TableProviderFilterPushDown::Exact,
                    }
                } else {
                    TableProviderFilterPushDown::Exact
                }
            })
            .collect())
    }
}

fn filter_uses_content(kind: StorylineTableKind, filter: &Expr) -> bool {
    let content = content_columns(kind)
        .iter()
        .map(|(name, _)| *name)
        .collect::<HashSet<_>>();
    filter
        .column_refs()
        .iter()
        .any(|column| content.contains(column.name.as_str()))
}

fn selected_content_columns(
    kind: StorylineTableKind,
    projection: Option<&Vec<usize>>,
    schema: &SchemaRef,
) -> HashSet<&'static str> {
    let projected = projection.map(|projection| {
        projection
            .iter()
            .map(|index| schema.field(*index).name().as_str())
            .collect::<HashSet<_>>()
    });
    content_columns(kind)
        .iter()
        .filter_map(|(name, _)| {
            projected
                .as_ref()
                .is_none_or(|projected| projected.contains(name))
                .then_some(*name)
        })
        .collect()
}

#[derive(Debug)]
struct ContentHydrationExec {
    input: Arc<dyn ExecutionPlan>,
    selected: HashSet<&'static str>,
    mode: ContentMaterializationMode,
    properties: Arc<PlanProperties>,
}

#[derive(Debug, Clone)]
enum ContentMaterializationMode {
    Full(Arc<Dataset>),
    Preview,
}

impl ContentHydrationExec {
    fn new(
        input: Arc<dyn ExecutionPlan>,
        selected: HashSet<&'static str>,
        mode: ContentMaterializationMode,
    ) -> Self {
        let properties = input.properties().clone();
        Self {
            input,
            selected,
            mode,
            properties,
        }
    }
}

impl DisplayAs for ContentHydrationExec {
    fn fmt_as(
        &self,
        _display_type: DisplayFormatType,
        formatter: &mut std::fmt::Formatter,
    ) -> std::fmt::Result {
        let mut selected = self.selected.iter().copied().collect::<Vec<_>>();
        selected.sort_unstable();
        let mode = match self.mode {
            ContentMaterializationMode::Full(_) => "full",
            ContentMaterializationMode::Preview => "preview",
        };
        write!(
            formatter,
            "ContentHydrationExec: mode={mode}, columns=[{}]",
            selected.join(",")
        )
    }
}

impl ExecutionPlan for ContentHydrationExec {
    fn name(&self) -> &str {
        "ContentHydrationExec"
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![&self.input]
    }

    fn with_new_children(
        self: Arc<Self>,
        mut children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        if children.len() != 1 {
            return Err(DataFusionError::Internal(format!(
                "ContentHydrationExec expected one child, got {}",
                children.len()
            )));
        }
        Ok(Arc::new(Self::new(
            children.swap_remove(0),
            self.selected.clone(),
            self.mode.clone(),
        )))
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> datafusion::common::Result<SendableRecordBatchStream> {
        let input = self.input.execute(partition, context)?;
        let mode = self.mode.clone();
        let selected = self.selected.clone();
        let stream = input.then(move |batch| {
            let mode = mode.clone();
            let selected = selected.clone();
            async move {
                let batch = batch?;
                let mut batches = match mode {
                    ContentMaterializationMode::Full(objects) => {
                        hydrate_selected_batches(&objects, vec![batch], &selected).await
                    }
                    ContentMaterializationMode::Preview => {
                        preview_selected_batches(vec![batch], &selected)
                    }
                }
                .map_err(|error| DataFusionError::Execution(error.to_string()))?;
                batches.pop().ok_or_else(|| {
                    DataFusionError::Internal("content hydration returned no batch".into())
                })
            }
        });
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.input.schema(),
            stream,
        )))
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
        let root = root.as_ref();
        let root = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance root is not valid UTF-8"))?;
        let store = StorylineLanceStore::open_uri_unchecked(root).await?;
        Self::from_store(&store).await
    }

    pub async fn open_with_options(
        root: impl AsRef<Path>,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let root = root.as_ref();
        let root = root
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance root is not valid UTF-8"))?;
        let store = StorylineLanceStore::open_uri_unchecked(root).await?;
        Self::from_store_with_options(&store, options).await
    }

    pub async fn open_uri(root: impl AsRef<str>) -> Result<Self> {
        let store = StorylineLanceStore::open_uri_unchecked(root).await?;
        Self::from_store(&store).await
    }

    pub async fn open_uri_with_options(
        root: impl AsRef<str>,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let store = StorylineLanceStore::open_uri_unchecked(root).await?;
        Self::from_store_with_options(&store, options).await
    }

    pub async fn from_store(store: &StorylineLanceStore) -> Result<Self> {
        Self::from_store_with_options(store, StorylineDataSourceOptions::default()).await
    }

    pub async fn from_store_with_options(
        store: &StorylineLanceStore,
        options: StorylineDataSourceOptions,
    ) -> Result<Self> {
        let paths = store
            .resolve_current_table_paths()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Storyline Lance store has no committed generation"))?;
        let (runs, steps, tool_calls, objects) = tokio::try_join!(
            open_dataset(&paths.runs, paths.runs_version),
            open_dataset(&paths.steps, paths.steps_version),
            open_dataset(&paths.tool_calls, paths.tool_calls_version),
            open_objects(&paths.objects, paths.objects_version),
        )?;
        let objects = Arc::new(objects);
        Ok(Self {
            paths,
            runs: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::Runs,
                Arc::new(runs),
                objects.clone(),
                options,
            )),
            steps: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::Steps,
                Arc::new(steps),
                objects.clone(),
                options,
            )),
            tool_calls: Arc::new(StorylineTableProvider::new(
                StorylineTableKind::ToolCalls,
                Arc::new(tool_calls),
                objects,
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

async fn open_dataset(path: &Path, version: u64) -> Result<Dataset> {
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline DataFusion table {}", path.display()))?;
    dataset.checkout_version(version).await.with_context(|| {
        format!(
            "open Storyline DataFusion table {} at version {version}",
            path.display()
        )
    })
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
