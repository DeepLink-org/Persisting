use super::*;

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
            DataFusionError::Execution(format!(
                "resolve Dataset source '{}': {error:#}",
                source.file()
            ))
        })?;
        let Some(table) = resolved
            .table(self.kind, event_session_ids)
            .await
            .map_err(|error| {
                DataFusionError::Execution(format!(
                    "prepare Dataset source '{}' table {}: {error:#}",
                    source.file(),
                    self.kind.table_name()
                ))
            })?
        else {
            return Ok(None);
        };
        let plan = if table.carries_file_column {
            let source_projection = file_source_projection(projection, self.schema.fields().len());
            table
                .provider
                .scan(state, Some(&source_projection), filters, limit)
                .await?
        } else {
            let base_projection = base_projection(projection);
            let business_filters = business_filters(filters);
            let input = table
                .provider
                .scan(state, base_projection.as_ref(), &business_filters, limit)
                .await?;
            project_catalog_source(input, source.file(), projection, &self.schema)?
        };
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
    } else if let Expr::BinaryExpr(binary) = expr {
        if binary.op == Operator::And {
            collect_business_conjuncts(&binary.left, output);
            collect_business_conjuncts(&binary.right, output);
        }
    }
}

fn base_projection(projection: Option<&Vec<usize>>) -> Option<Vec<usize>> {
    projection.map(|projection| {
        projection
            .iter()
            .filter_map(|index| index.checked_sub(1))
            .collect()
    })
}

fn file_source_projection(projection: Option<&Vec<usize>>, catalog_width: usize) -> Vec<usize> {
    let file_column = catalog_width.saturating_sub(1);
    projection
        .cloned()
        .unwrap_or_else(|| (0..catalog_width).collect())
        .into_iter()
        .map(|index| if index == 0 { file_column } else { index - 1 })
        .collect()
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
            } else {
                physical_col(field.name(), input.schema().as_ref())?
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
    context.register_table(
        TableReference::partial(dataset.to_string(), table.to_string()),
        provider.clone(),
    )?;
    if register_default {
        context.register_table(TableReference::bare(table.to_string()), provider)?;
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
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_count, \
                    (SELECT array_agg(s.step_id ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_ids, \
                    (SELECT array_agg(s.source ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_sources, \
                    (SELECT array_agg(s.message_json ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS messages_json, \
                    (SELECT COUNT(*) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_call_count, \
                    (SELECT array_agg(t.function_name ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_names, \
                    (SELECT array_agg(t.arguments_json ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_arguments_json, \
                    (SELECT array_agg(t.results_json ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_results_json \
             FROM {dataset}.runs r"
        ),
    )
    .await
}

pub(super) async fn execute_ddl(context: &SessionContext, sql: &str) -> Result<()> {
    context
        .sql(sql)
        .await
        .with_context(|| format!("plan Catalog DDL: {sql}"))?
        .collect()
        .await
        .with_context(|| format!("execute Catalog DDL: {sql}"))?;
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
