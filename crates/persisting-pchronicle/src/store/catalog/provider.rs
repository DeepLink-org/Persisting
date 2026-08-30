use super::*;

/// A small, conservative subset of JSONB predicates that can be evaluated on
/// the normalized `runs` table before a virtual row is assembled.  The
/// original predicate is still forwarded to the virtual provider afterwards;
/// this is therefore an optimization, not a semantic rewrite.
fn normalized_json_filter(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            let left = normalized_json_filter(binary.left.as_ref());
            let right = normalized_json_filter(binary.right.as_ref());
            match (left, right) {
                (Some(left), Some(right)) => Some(Expr::BinaryExpr(
                    datafusion::logical_expr::BinaryExpr::new(
                        Box::new(left),
                        Operator::And,
                        Box::new(right),
                    ),
                )),
                (Some(expr), None) | (None, Some(expr)) => Some(expr),
                (None, None) => None,
            }
        }
        Expr::BinaryExpr(binary) => normalized_json_comparison(
            binary.left.as_ref(),
            binary.op,
            binary.right.as_ref(),
        ),
        _ => None,
    }
}

fn normalized_json_comparison(left: &Expr, op: Operator, right: &Expr) -> Option<Expr> {
    let Expr::ScalarFunction(function) = left else {
        return None;
    };
    if !matches!(
        function.name(),
        "json_get_string" | "json_get_int" | "json_get_float" | "json_get_bool"
    ) || function.args.len() != 2
    {
        return None;
    }
    let Expr::Column(column) = &function.args[0] else {
        return None;
    };
    if column.name != "data" {
        return None;
    }
    let Expr::Literal(ScalarValue::Utf8(Some(path)), _) = &function.args[1] else {
        return None;
    };
    let Expr::Literal(value, _) = right else {
        return None;
    };

    // Keep this mapping intentionally limited to fields whose virtual JSON
    // representation is sourced directly from the run row.  Step/tool-call
    // paths and arbitrary `extra` values still use the exact virtual-table
    // predicate after materialization.
    let path = path
        .trim()
        .trim_start_matches("$.")
        .trim_start_matches('$')
        .trim_start_matches('.')
        .replace('/', ".");
    let normalized_column = match path.as_str() {
        "document_id" => "document_id",
        "session" | "session_id" => "session_id",
        "run" | "run_id" => "run_id",
        "attempt" | "attempt_id" => "attempt_id",
        "agent.id" | "agent_id" => "agent_id",
        "agent.name" | "agent_name" => "agent_name",
        "agent.ver" | "agent.version" | "agent_version" => "agent_version",
        "agent.model" | "agent.model_name" | "agent_model_name" => "agent_model_name",
        // ATIF's trajectory_id is the normalized document identity only when
        // it was explicitly present.  The extra predicate preserves the
        // distinction between a missing trajectory_id and a session fallback.
        "trajectory" | "trajectory_id" => {
            if op != Operator::Eq {
                return None;
            }
            return Some(Expr::BinaryExpr(
                datafusion::logical_expr::BinaryExpr::new(
                    Box::new(Expr::BinaryExpr(
                        datafusion::logical_expr::BinaryExpr::new(
                            Box::new(Expr::Column(datafusion::common::Column::new_unqualified(
                                "trajectory_id_explicit",
                            ))),
                            Operator::Eq,
                            Box::new(Expr::Literal(ScalarValue::Boolean(Some(true)), None)),
                        ),
                    )),
                    Operator::And,
                    Box::new(Expr::BinaryExpr(
                        datafusion::logical_expr::BinaryExpr::new(
                            Box::new(Expr::Column(datafusion::common::Column::new_unqualified(
                                "document_id",
                            ))),
                            op,
                            Box::new(Expr::Literal(value.clone(), None)),
                        ),
                    )),
                ),
            ));
        }
        _ => return None,
    };
    Some(Expr::BinaryExpr(
        datafusion::logical_expr::BinaryExpr::new(
            Box::new(Expr::Column(datafusion::common::Column::new_unqualified(
                normalized_column,
            ))),
            op,
            Box::new(Expr::Literal(value.clone(), None)),
        ),
    ))
}

#[derive(Debug)]
pub(super) struct CatalogVirtualDocumentTableProvider {
    sources: Vec<Arc<LazySource>>,
    format: DocumentFormat,
    max_concurrent_sources: usize,
}

impl CatalogVirtualDocumentTableProvider {
    pub(super) fn new(
        sources: Vec<Arc<LazySource>>,
        format: DocumentFormat,
        max_concurrent_sources: usize,
    ) -> Self {
        Self {
            sources,
            format,
            max_concurrent_sources,
        }
    }
}

#[async_trait]
impl TableProvider for CatalogVirtualDocumentTableProvider {
    fn schema(&self) -> SchemaRef {
        crate::store::virtual_document::schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let selected = self
            .sources
            .iter()
            .filter(|source| source.format_hint().is_none_or(|hint| hint == self.format))
            .cloned()
            .collect::<Vec<_>>();
        let format = self.format;
        let normalized_filter = filters
            .iter()
            .filter_map(normalized_json_filter)
            .reduce(|left, right| {
                Expr::BinaryExpr(datafusion::logical_expr::BinaryExpr::new(
                    Box::new(left),
                    Operator::And,
                    Box::new(right),
                ))
            });
        let rows = stream::iter(selected)
            .map(|source| {
                let normalized_filter = normalized_filter.clone();
                async move {
                let resolved = source
                    .resolve()
                    .await
                    .map_err(|error| crate::store::datafusion_bridge::into_datafusion(error))?;
                let candidate_ids = match normalized_filter.as_ref() {
                    Some(filter) => resolved
                        .document_ids_for_virtual_filter(format, filter)
                        .await
                        .map_err(|error| {
                            crate::store::datafusion_bridge::into_datafusion(
                                error.context("push down virtual JSON filter"),
                            )
                        })?,
                    None => None,
                };
                resolved
                    .virtual_document_rows_filtered(format, candidate_ids.as_ref())
                    .await
                    .map(|rows| {
                        rows.into_iter()
                            .map(|(id, data)| (format!("{}::{id}", source.file()), data))
                            .collect::<Vec<_>>()
                    })
                    .map_err(|error| crate::store::datafusion_bridge::into_datafusion(error))
                }
            })
            .buffered(self.max_concurrent_sources)
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let table = crate::store::virtual_document::provider(&rows)
            .map_err(|error| crate::store::datafusion_bridge::into_datafusion(error))?;
        table.scan(state, projection, filters, limit).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|_| TableProviderFilterPushDown::Inexact)
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogTableKind {
    Runs,
    Steps,
    ToolCalls,
    Events,
}

impl CatalogTableKind {
    pub(super) const ALL: [Self; 4] = [Self::Runs, Self::Steps, Self::ToolCalls, Self::Events];

    pub(super) fn table_name(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Steps => "steps",
            Self::ToolCalls => "tool_calls",
            Self::Events => "events",
        }
    }

    fn base_schema(self) -> SchemaRef {
        match self {
            Self::Runs => story_runs_arrow_schema(),
            Self::Steps => story_steps_arrow_schema(),
            Self::ToolCalls => story_tool_calls_arrow_schema(),
            Self::Events => raw_event_arrow_schema(),
        }
    }
}

/// One Dataset-level provider per stable table. It evaluates catalog-owned
/// `_file_` predicates before resolving a source, then delegates the remaining
/// projection/filter/limit pushdown to that source's native provider.
#[derive(Debug)]
pub(super) struct CatalogTableProvider {
    sources: Vec<Arc<LazySource>>,
    kind: CatalogTableKind,
    schema: SchemaRef,
    max_concurrent_sources: usize,
}

impl CatalogTableProvider {
    pub(super) fn new(
        sources: Vec<Arc<LazySource>>,
        kind: CatalogTableKind,
        max_concurrent_sources: usize,
    ) -> Self {
        Self {
            sources,
            kind,
            schema: catalog_schema(&kind.base_schema()),
            max_concurrent_sources,
        }
    }

    async fn scan_source(
        &self,
        source: &LazySource,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
        event_session_ids: Option<&BTreeSet<String>>,
    ) -> datafusion::common::Result<Option<Arc<dyn ExecutionPlan>>> {
        let resolved = source.resolve().await.map_err(|error| {
            crate::store::datafusion_bridge::into_datafusion(
                error.context("resolve Dataset source"),
            )
        })?;
        let Some(table) = resolved
            .table(self.kind, event_session_ids)
            .await
            .map_err(|error| {
                crate::store::datafusion_bridge::into_datafusion(
                    error.context("prepare Dataset source table"),
                )
            })?
        else {
            return Ok(None);
        };
        let physical_schema = table.provider.schema();
        let source_projection = if table.carries_file_column {
            file_source_projection(projection, self.schema.as_ref(), physical_schema.as_ref())?
        } else {
            physical_projection(projection, self.schema.as_ref(), physical_schema.as_ref())?
        };
        let forwarded_filters = if table.carries_file_column {
            filters.to_vec()
        } else {
            business_filters(filters)
        };
        let input = table
            .provider
            .scan(state, source_projection.as_ref(), &forwarded_filters, limit)
            .await?;
        let plan = project_catalog_source(input, source.file(), projection, &self.schema)?;
        Ok(Some(plan))
    }
}

#[async_trait]
impl TableProvider for CatalogTableProvider {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let output_schema = projected_schema(&self.schema, projection)?;
        let event_session_ids = required_string_values(filters, "session_id");
        let event_session_ids = event_session_ids.as_ref();
        let selected = self
            .sources
            .iter()
            .filter(|source| {
                source.supports(self.kind)
                    && filters
                        .iter()
                        .all(|filter| evaluate_file_filter(filter, source.file()).unwrap_or(true))
            })
            .cloned()
            .collect::<Vec<_>>();
        // Resolution is bounded and order-preserving. A ready Source that fails
        // during late resolution still fails the query rather than disappearing.
        let planned = stream::iter(selected)
            .map(|source| async move {
                self.scan_source(
                    source.as_ref(),
                    state,
                    projection,
                    filters,
                    limit,
                    event_session_ids,
                )
                .await
            })
            .buffered(self.max_concurrent_sources)
            .try_collect::<Vec<_>>()
            .await?;
        let mut plans = planned.into_iter().flatten().collect::<Vec<_>>();

        let selected_source_count = plans.len();
        let plan: Arc<dyn ExecutionPlan> = match selected_source_count {
            0 => Arc::new(EmptyExec::new(output_schema)),
            1 => plans.pop().ok_or_else(|| {
                DataFusionError::Internal(
                    "Catalog planned one source but produced no execution plan".into(),
                )
            })?,
            _ => UnionExec::try_new(plans)?,
        };
        Ok(match limit {
            Some(limit) if selected_source_count > 1 => {
                Arc::new(GlobalLimitExec::new(plan, 0, Some(limit)))
            }
            _ => plan,
        })
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|filter| {
                let columns = filter.column_refs();
                if !columns.is_empty()
                    && columns
                        .iter()
                        .all(|column| column.name == SOURCE_FILE_COLUMN)
                    && evaluate_file_filter(filter, "").is_some()
                {
                    TableProviderFilterPushDown::Exact
                } else {
                    // The provider still forwards safe business predicates to
                    // native sources, while DataFusion retains the expression
                    // above us for cross-format semantic correctness.
                    TableProviderFilterPushDown::Inexact
                }
            })
            .collect())
    }
}

fn evaluate_file_filter(expr: &Expr, path: &str) -> Option<bool> {
    if let Some(value) = matches_file_filter(expr, path) {
        return Some(value);
    }
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            let left = evaluate_file_filter(&binary.left, path);
            let right = evaluate_file_filter(&binary.right, path);
            match (left, right) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            }
        }
        Expr::BinaryExpr(binary) if binary.op == Operator::Or => {
            let left = evaluate_file_filter(&binary.left, path);
            let right = evaluate_file_filter(&binary.right, path);
            match (left, right) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            }
        }
        Expr::Not(inner) => evaluate_file_filter(inner, path).map(|value| !value),
        Expr::Literal(ScalarValue::Boolean(value), _) => *value,
        _ => None,
    }
}

fn required_string_values(filters: &[Expr], column: &str) -> Option<BTreeSet<String>> {
    filters
        .iter()
        .filter_map(|filter| required_string_values_expr(filter, column))
        .reduce(|left, right| left.intersection(&right).cloned().collect())
}

fn required_string_values_expr(expr: &Expr, column: &str) -> Option<BTreeSet<String>> {
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::Eq => {
            let value = if is_named_column(&binary.left, column) {
                string_scalar(&binary.right)
            } else if is_named_column(&binary.right, column) {
                string_scalar(&binary.left)
            } else {
                None
            }?;
            Some(BTreeSet::from([value.to_string()]))
        }
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            match (
                required_string_values_expr(&binary.left, column),
                required_string_values_expr(&binary.right, column),
            ) {
                (Some(left), Some(right)) => Some(left.intersection(&right).cloned().collect()),
                (Some(values), None) | (None, Some(values)) => Some(values),
                (None, None) => None,
            }
        }
        Expr::BinaryExpr(binary) if binary.op == Operator::Or => {
            let left = required_string_values_expr(&binary.left, column)?;
            let right = required_string_values_expr(&binary.right, column)?;
            Some(left.union(&right).cloned().collect())
        }
        Expr::InList(list) if !list.negated && is_named_column(&list.expr, column) => list
            .list
            .iter()
            .map(string_scalar)
            .map(|value| value.map(str::to_string))
            .collect(),
        _ => None,
    }
}

fn is_named_column(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::Column(column) if column.name == name)
}

fn string_scalar(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Literal(ScalarValue::Utf8(Some(value)), _)
        | Expr::Literal(ScalarValue::LargeUtf8(Some(value)), _)
        | Expr::Literal(ScalarValue::Utf8View(Some(value)), _) => Some(value),
        _ => None,
    }
}

fn business_filters(filters: &[Expr]) -> Vec<Expr> {
    let mut output = Vec::new();
    for filter in filters {
        collect_business_conjuncts(filter, &mut output);
    }
    output
}

fn collect_business_conjuncts(expr: &Expr, output: &mut Vec<Expr>) {
    if !expr
        .column_refs()
        .iter()
        .any(|column| column.name == SOURCE_FILE_COLUMN)
    {
        output.push(expr.clone());
    } else if let Expr::BinaryExpr(binary) = expr
        && binary.op == Operator::And
    {
        collect_business_conjuncts(&binary.left, output);
        collect_business_conjuncts(&binary.right, output);
    }
}

fn physical_projection(
    projection: Option<&Vec<usize>>,
    catalog_schema: &Schema,
    physical_schema: &Schema,
) -> datafusion::common::Result<Option<Vec<usize>>> {
    let Some(projection) = projection else {
        return Ok(None);
    };
    let mut physical = Vec::with_capacity(projection.len());
    for &index in projection {
        if index == 0 {
            continue;
        }
        let name = catalog_schema.field(index).name();
        if let Ok(physical_index) = physical_schema.index_of(name) {
            physical.push(physical_index);
        }
    }
    Ok(Some(physical))
}

fn null_literal(
    data_type: &DataType,
) -> datafusion::common::Result<Arc<dyn datafusion::physical_expr::PhysicalExpr>> {
    Ok(Arc::new(Literal::new(
        ScalarValue::try_from(data_type).map_err(|error| {
            DataFusionError::Internal(format!(
                "catalog cannot synthesize a null for {data_type}: {error}"
            ))
        })?,
    )))
}

fn file_source_projection(
    projection: Option<&Vec<usize>>,
    catalog_schema: &Schema,
    physical_schema: &Schema,
) -> datafusion::common::Result<Option<Vec<usize>>> {
    let Some(projection) = projection else {
        return Ok(None);
    };
    let physical = projection
        .iter()
        .filter_map(|index| {
            let name = catalog_schema.field(*index).name();
            physical_schema.index_of(name).ok()
        })
        .collect::<Vec<_>>();
    Ok(Some(physical))
}

fn projected_schema(
    schema: &SchemaRef,
    projection: Option<&Vec<usize>>,
) -> datafusion::common::Result<SchemaRef> {
    Ok(match projection {
        Some(projection) => Arc::new(schema.project(projection)?),
        None => schema.clone(),
    })
}

fn project_catalog_source(
    input: Arc<dyn ExecutionPlan>,
    file: &str,
    projection: Option<&Vec<usize>>,
    schema: &SchemaRef,
) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
    let requested = projection
        .cloned()
        .unwrap_or_else(|| (0..schema.fields().len()).collect());
    let expressions = requested
        .into_iter()
        .map(|index| {
            let field = schema.field(index);
            let expr: Arc<dyn datafusion::physical_expr::PhysicalExpr> = if index == 0 {
                Arc::new(Literal::new(ScalarValue::Utf8(Some(file.to_string()))))
            } else if input.schema().index_of(field.name()).is_ok() {
                physical_col(field.name(), input.schema().as_ref())?
            } else {
                null_literal(field.data_type())?
            };
            Ok(ProjectionExpr {
                expr,
                alias: field.name().clone(),
            })
        })
        .collect::<datafusion::common::Result<Vec<_>>>()?;
    Ok(Arc::new(ProjectionExec::try_new(expressions, input)?))
}

pub(super) fn register_catalog_provider(
    context: &SessionContext,
    dataset: &str,
    table: &str,
    provider: Arc<dyn TableProvider>,
    register_default: bool,
) -> Result<()> {
    context
        .register_table(
            TableReference::partial(dataset.to_string(), table.to_string()),
            provider.clone(),
        )
        .map_err(|error| {
            crate::store::datafusion_bridge::from_datafusion("register Dataset table", error)
        })?;
    if register_default {
        context
            .register_table(TableReference::bare(table.to_string()), provider)
            .map_err(|error| {
                crate::store::datafusion_bridge::from_datafusion(
                    "register default Dataset table",
                    error,
                )
            })?;
    }
    Ok(())
}

pub(super) async fn create_trajectories_view(
    context: &SessionContext,
    dataset: &str,
) -> Result<()> {
    execute_ddl(
        context,
        &format!(
            "CREATE VIEW {dataset}.trajectories AS \
             SELECT r.*, \
                    (SELECT COUNT(*) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.document_id = r.document_id) AS step_count, \
                    (SELECT array_agg(s.step_id ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.document_id = r.document_id) AS step_ids, \
                    (SELECT array_agg(s.source ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.document_id = r.document_id) AS step_sources, \
                    (SELECT array_agg(s.message_value ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.document_id = r.document_id) AS messages_value, \
                    (SELECT COUNT(*) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.document_id = r.document_id) AS tool_call_count, \
                    (SELECT array_agg(t.function_name ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.document_id = r.document_id) AS tool_names, \
                    (SELECT array_agg(t.arguments ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.document_id = r.document_id) AS tool_arguments_json, \
                    (SELECT array_agg(t.results ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.document_id = r.document_id) AS tool_results_json \
             FROM {dataset}.runs r"
        ),
    )
    .await
}

pub(super) async fn execute_ddl(context: &SessionContext, sql: &str) -> Result<()> {
    context
        .sql(sql)
        .await
        .map_err(|error| {
            crate::store::datafusion_bridge::from_datafusion("plan Catalog DDL", error)
        })?
        .collect()
        .await
        .map_err(|error| {
            crate::store::datafusion_bridge::from_datafusion("execute Catalog DDL", error)
        })?;
    Ok(())
}

pub(super) fn sources_table_provider(
    sources: &[DiscoveredSource],
) -> Result<Arc<dyn TableProvider>> {
    let schema = sources_schema();
    let snapshot_refs = sources
        .iter()
        .map(DiscoveredSource::snapshot_ref)
        .collect::<Vec<_>>();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from_iter_values(
                sources.iter().map(|source| source.file.as_str()),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.format.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from_iter_values(sources.iter().map(
                |source| match source.kind {
                    CatalogSourceKind::Store => "store",
                    CatalogSourceKind::File => "file",
                },
            ))),
            Arc::new(StringArray::from(
                snapshot_refs
                    .iter()
                    .map(Option::as_deref)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| {
                        source.projection_status.map(|status| match status {
                            CatalogProjectionStatus::Fresh => "fresh",
                            CatalogProjectionStatus::Stale => "stale",
                        })
                    })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.projection_generation.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from_iter_values(
                sources.iter().map(|source| source.projection_candidates),
            )),
            Arc::new(UInt64Array::from(
                sources
                    .iter()
                    .map(|source| source.size_bytes)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.last_modified.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from_iter_values(sources.iter().map(
                |source| match source.status {
                    CatalogSourceStatus::Ready => "ready",
                    CatalogSourceStatus::Error => "error",
                },
            ))),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.error.as_deref())
                    .collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
}

fn sources_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new(SOURCE_FILE_COLUMN, DataType::Utf8, false),
        Field::new("format", DataType::Utf8, true),
        Field::new("kind", DataType::Utf8, false),
        Field::new("snapshot_ref", DataType::Utf8, true),
        Field::new("projection_status", DataType::Utf8, true),
        Field::new("projection_generation", DataType::Utf8, true),
        Field::new("projection_candidates", DataType::UInt64, false),
        Field::new("size_bytes", DataType::UInt64, true),
        Field::new("last_modified", DataType::Utf8, true),
        Field::new("status", DataType::Utf8, false),
        Field::new("error", DataType::Utf8, true),
    ]))
}

fn catalog_schema(base: &SchemaRef) -> SchemaRef {
    let mut fields = Vec::with_capacity(base.fields().len() + 1);
    fields.push(Arc::new(Field::new(
        SOURCE_FILE_COLUMN,
        DataType::Utf8,
        false,
    )));
    fields.extend(base.fields().iter().cloned());
    Arc::new(Schema::new(fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs_schema_without_meta() -> SchemaRef {
        let fields = story_runs_arrow_schema()
            .fields()
            .iter()
            .filter(|field| field.name() != "meta")
            .cloned()
            .collect::<Vec<_>>();
        Arc::new(Schema::new(fields))
    }

    #[test]
    fn storyline_projection_follows_column_names_when_meta_is_absent() {
        let catalog = catalog_schema(&story_runs_arrow_schema());
        let physical = runs_schema_without_meta();
        let catalog_index = catalog
            .index_of("unknown_fields")
            .expect("catalog schema exposes unknown_fields");
        let physical_index = physical
            .index_of("unknown_fields")
            .expect("older Storyline runs still have unknown_fields");
        assert_ne!(
            catalog_index.checked_sub(1),
            Some(physical_index),
            "inserting meta must shift later catalog indexes"
        );

        let mapped =
            physical_projection(Some(&vec![0, catalog_index]), &catalog, physical.as_ref())
                .expect("older physical schema remains queryable");
        assert_eq!(mapped, Some(vec![physical_index]));
    }

    #[test]
    fn storyline_projection_skips_columns_missing_from_older_runs() {
        let catalog = catalog_schema(&story_runs_arrow_schema());
        let physical = runs_schema_without_meta();
        let meta = catalog
            .index_of("meta")
            .expect("current catalog schema exposes meta");
        let mapped =
            physical_projection(Some(&vec![meta]), &catalog, physical.as_ref()).expect("missing");
        assert_eq!(mapped, Some(Vec::new()));
    }
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use super::*;
    use datafusion::logical_expr::{col, lit};
    use proptest::prelude::*;

    fn catalog_index_strategy() -> impl Strategy<Value = usize> {
        let field_count = catalog_schema(&story_runs_arrow_schema()).fields().len();
        0usize..field_count
    }

    proptest! {
        #[test]
        fn physical_projection_never_forwards_the_virtual_file_column(
            catalog_index in catalog_index_strategy(),
        ) {
            let catalog = catalog_schema(&story_runs_arrow_schema());
            let physical = story_runs_arrow_schema();
            let mapped = physical_projection(
                Some(&vec![catalog_index]),
                catalog.as_ref(),
                physical.as_ref(),
            )
            .expect("schema projection should be valid")
            .expect("an explicit projection returns a mapping");

            if catalog_index == 0 {
                prop_assert!(mapped.is_empty());
            } else {
                let name = catalog.field(catalog_index).name();
                match physical.index_of(name) {
                    Ok(index) => prop_assert_eq!(mapped, vec![index]),
                    Err(_) => prop_assert!(mapped.is_empty()),
                }
            }
        }

        #[test]
        fn negated_file_filters_are_the_complement_of_their_base_expression(
            candidate in proptest::string::string_regex("[A-Za-z0-9_./-]{0,48}").unwrap(),
            expected in proptest::string::string_regex("[A-Za-z0-9_./-]{0,48}").unwrap(),
        ) {
            let expression = col(SOURCE_FILE_COLUMN).eq(lit(expected.clone()));
            let base = evaluate_file_filter(&expression, &candidate);
            let negated = Expr::Not(Box::new(expression));
            prop_assert_eq!(base, Some(candidate == expected));
            prop_assert_eq!(evaluate_file_filter(&negated, &candidate), base.map(|value| !value));
        }
    }
}
