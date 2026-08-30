//! Query-only, format-shaped document tables.
//!
//! Each peripheral format is exposed as a deliberately small two-column
//! table: a stable `id` and the original format document in Lance JSONB
//! (`data`).  Keeping the JSON extension metadata on the Arrow field is
//! important: Lance's JSON UDFs can then address paths without first turning
//! the value back into text.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::{Column, ScalarValue};
use datafusion::datasource::MemTable;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{
    BinaryExpr, Expr, Operator, TableProviderFilterPushDown, TableType,
};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use lance::deps::arrow_array::{Array, LargeBinaryArray, RecordBatch, StringArray};
use lance::deps::arrow_schema::{Field, Schema as ArrowSchema, SchemaRef};
use lance_arrow::json::{encode_json, json_field};

use crate::format::DocumentFormat;

/// Names are intentionally SQL-safe.  Hyphenated `DocumentFormat::as_str`
/// values would require quoting (`dataset.openai-msg`) in every query.
pub(crate) fn table_name(format: DocumentFormat) -> Option<&'static str> {
    Some(match format {
        DocumentFormat::CanonicalEvent => "events",
        DocumentFormat::Atif => "atif",
        DocumentFormat::Storyline => "storyline",
        DocumentFormat::Actf => "actf",
        DocumentFormat::OpenaiMsg => "openai_msg",
        DocumentFormat::Codex => "codex",
        DocumentFormat::ClaudeCode => "claude",
        DocumentFormat::AgenticMd => "markdown",
        DocumentFormat::StorylineLance => return None,
    })
}

pub(crate) fn formats() -> impl Iterator<Item = DocumentFormat> {
    // Canonical Event already owns the stable `events` table (with its
    // denormalized columns and payload_json); registering another provider
    // under the same name would make catalog registration fail.
    [
        DocumentFormat::Atif,
        DocumentFormat::Storyline,
        DocumentFormat::Actf,
        DocumentFormat::OpenaiMsg,
        DocumentFormat::Codex,
        DocumentFormat::ClaudeCode,
        DocumentFormat::AgenticMd,
    ]
    .into_iter()
}

pub(crate) fn schema() -> SchemaRef {
    Arc::new(ArrowSchema::new(vec![
        Field::new("id", lance::deps::arrow_schema::DataType::Utf8, false),
        json_field("data", false),
    ]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalizedVirtualTable {
    Runs,
    Steps,
    ToolCalls,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedJsonPredicate {
    pub(crate) table: NormalizedVirtualTable,
    pub(crate) filter: Expr,
}

/// Convert the safe subset of a virtual JSON predicate into predicates over
/// the normalized three-table model. Callers retain the original JSON
/// predicate, so these expressions only prune documents before format
/// encoding; unsupported paths simply produce no pushdown.
pub(crate) fn normalized_json_predicates(
    format: DocumentFormat,
    expr: &Expr,
) -> Vec<NormalizedJsonPredicate> {
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            let mut predicates = normalized_json_predicates(format, binary.left.as_ref());
            predicates.extend(normalized_json_predicates(format, binary.right.as_ref()));
            predicates
        }
        Expr::BinaryExpr(binary) => normalized_json_comparison(
            format,
            binary.left.as_ref(),
            binary.op,
            binary.right.as_ref(),
        )
        .or_else(|| {
            normalized_json_comparison(
                format,
                binary.right.as_ref(),
                reverse_operator(binary.op)?,
                binary.left.as_ref(),
            )
        })
        .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn normalized_json_comparison(
    format: DocumentFormat,
    left: &Expr,
    op: Operator,
    right: &Expr,
) -> Option<Vec<NormalizedJsonPredicate>> {
    let Expr::ScalarFunction(function) = left else {
        return None;
    };
    if !matches!(
        function.name(),
        "json_get_string" | "json_get_int" | "json_get_bool" | "json_extract"
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
    let path = match &function.args[1] {
        Expr::Literal(ScalarValue::Utf8(Some(path)), _)
        | Expr::Literal(ScalarValue::Utf8View(Some(path)), _)
        | Expr::Literal(ScalarValue::LargeUtf8(Some(path)), _) => path,
        _ => return None,
    };
    let Expr::Literal(value, _) = right else {
        return None;
    };
    let (function_name, value) = if function.name() == "json_extract" {
        let (ScalarValue::Utf8(Some(encoded))
        | ScalarValue::Utf8View(Some(encoded))
        | ScalarValue::LargeUtf8(Some(encoded))) = value
        else {
            return None;
        };
        match serde_json::from_str::<serde_json::Value>(encoded).ok()? {
            serde_json::Value::String(value) => ("json_get_string", ScalarValue::Utf8(Some(value))),
            serde_json::Value::Number(value) => {
                ("json_get_int", ScalarValue::Int64(Some(value.as_i64()?)))
            }
            serde_json::Value::Bool(value) => ("json_get_bool", ScalarValue::Boolean(Some(value))),
            _ => return None,
        }
    } else {
        (function.name(), value.clone())
    };
    let path = path
        .trim()
        .trim_start_matches("$.")
        .trim_start_matches('$')
        .trim_start_matches('.')
        .replace('/', ".")
        .replace('[', ".")
        .replace(']', "");
    let segments = path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if function.name() != "json_extract" && segments.len() > 1 {
        return None;
    }
    if let Some(predicate) = normalized_run_predicate(function_name, &segments, op, &value) {
        return Some(vec![predicate]);
    }
    if matches!(format, DocumentFormat::Atif | DocumentFormat::Storyline)
        && let Some(predicates) = normalized_step_predicates(function_name, &segments, op, &value)
    {
        return Some(predicates);
    }
    None
}

fn normalized_run_predicate(
    function: &str,
    path: &[&str],
    op: Operator,
    value: &ScalarValue,
) -> Option<NormalizedJsonPredicate> {
    if function != "json_get_string" || !is_string(value) {
        return None;
    }
    let normalized_column = match path {
        ["document_id"] => "document_id",
        ["session"] | ["session_id"] => "session_id",
        ["run"] | ["run_id"] => "run_id",
        ["attempt"] | ["attempt_id"] => "attempt_id",
        ["agent", "id"] | ["agent_id"] => "agent_id",
        ["agent", "name"] | ["agent_name"] => "agent_name",
        ["agent", "ver"] | ["agent", "version"] | ["agent_version"] => "agent_version",
        ["agent", "model"] | ["agent", "model_name"] | ["agent_model_name"] => "agent_model_name",
        // The normalized document id equals trajectory_id only when the input
        // explicitly carried one; otherwise it falls back to session_id.
        ["trajectory"] | ["trajectory_id"] if op == Operator::Eq => {
            return Some(NormalizedJsonPredicate {
                table: NormalizedVirtualTable::Runs,
                filter: binary_expr(
                    binary_expr(
                        Expr::Column(Column::new_unqualified("trajectory_id_explicit")),
                        Operator::Eq,
                        Expr::Literal(ScalarValue::Boolean(Some(true)), None),
                    ),
                    Operator::And,
                    binary_expr(
                        Expr::Column(Column::new_unqualified("document_id")),
                        op,
                        Expr::Literal(value.clone(), None),
                    ),
                ),
            });
        }
        _ => return None,
    };
    Some(NormalizedJsonPredicate {
        table: NormalizedVirtualTable::Runs,
        filter: binary_expr(
            Expr::Column(Column::new_unqualified(normalized_column)),
            op,
            Expr::Literal(value.clone(), None),
        ),
    })
}

fn normalized_step_predicates(
    function: &str,
    path: &[&str],
    op: Operator,
    value: &ScalarValue,
) -> Option<Vec<NormalizedJsonPredicate>> {
    let [collection @ ("steps" | "turns"), turn, tail @ ..] = path else {
        return None;
    };
    let _ = collection;
    let turn_ordinal = turn.parse::<i64>().ok()?;
    let turn_filter = binary_expr(
        Expr::Column(Column::new_unqualified("turn_ordinal")),
        Operator::Eq,
        Expr::Literal(ScalarValue::Int64(Some(turn_ordinal)), None),
    );

    if let ["tool_calls", call, field] = tail {
        if function != "json_get_string" || !is_string(value) {
            return None;
        }
        let call_index = call.parse::<i64>().ok()?;
        let column = match *field {
            "function_name" | "fn" => "function_name",
            "tool_call_id" | "tcid" => "tool_call_id",
            "kind" => "kind",
            _ => return None,
        };
        return Some(vec![
            NormalizedJsonPredicate {
                table: NormalizedVirtualTable::Steps,
                filter: turn_filter,
            },
            NormalizedJsonPredicate {
                table: NormalizedVirtualTable::ToolCalls,
                filter: binary_expr(
                    binary_expr(
                        Expr::Column(Column::new_unqualified("call_index")),
                        Operator::Eq,
                        Expr::Literal(ScalarValue::Int64(Some(call_index)), None),
                    ),
                    Operator::And,
                    binary_expr(
                        Expr::Column(Column::new_unqualified(column)),
                        op,
                        Expr::Literal(value.clone(), None),
                    ),
                ),
            },
        ]);
    }

    let [field] = tail else {
        return None;
    };
    let column = match (function, *field) {
        ("json_get_string", "source" | "src") if is_string(value) => "source",
        ("json_get_string", "kind") if is_string(value) => "kind",
        ("json_get_string", "model" | "model_name") if is_string(value) => "model_name",
        ("json_get_string", "reason" | "reasoning_content") if is_string(value) => {
            "reasoning_content"
        }
        ("json_get_int", "step_id") if is_integer(value) => "step_id",
        ("json_get_int", "llm_call_count" | "nllm") if is_integer(value) => "llm_call_count",
        ("json_get_bool", "is_copied_context" | "copied")
            if matches!(value, ScalarValue::Boolean(Some(_))) =>
        {
            "is_copied_context"
        }
        _ => return None,
    };
    Some(vec![NormalizedJsonPredicate {
        table: NormalizedVirtualTable::Steps,
        filter: binary_expr(
            turn_filter,
            Operator::And,
            binary_expr(
                Expr::Column(Column::new_unqualified(column)),
                op,
                Expr::Literal(value.clone(), None),
            ),
        ),
    }])
}

fn is_string(value: &ScalarValue) -> bool {
    matches!(
        value,
        ScalarValue::Utf8(Some(_))
            | ScalarValue::Utf8View(Some(_))
            | ScalarValue::LargeUtf8(Some(_))
    )
}

fn is_integer(value: &ScalarValue) -> bool {
    matches!(
        value,
        ScalarValue::Int8(Some(_))
            | ScalarValue::Int16(Some(_))
            | ScalarValue::Int32(Some(_))
            | ScalarValue::Int64(Some(_))
            | ScalarValue::UInt8(Some(_))
            | ScalarValue::UInt16(Some(_))
            | ScalarValue::UInt32(Some(_))
            | ScalarValue::UInt64(Some(_))
    )
}

fn binary_expr(left: Expr, op: Operator, right: Expr) -> Expr {
    Expr::BinaryExpr(BinaryExpr::new(Box::new(left), op, Box::new(right)))
}

fn reverse_operator(op: Operator) -> Option<Operator> {
    Some(match op {
        Operator::Eq | Operator::NotEq => op,
        Operator::Lt => Operator::Gt,
        Operator::LtEq => Operator::GtEq,
        Operator::Gt => Operator::Lt,
        Operator::GtEq => Operator::LtEq,
        _ => return None,
    })
}

pub(crate) async fn matching_document_ids(
    table: Arc<dyn TableProvider>,
    filter: &Expr,
) -> Result<BTreeSet<String>> {
    anyhow::ensure!(
        table.schema().index_of("document_id").is_ok(),
        "normalized table has no document_id"
    );
    // A DataFrame preserves the TableProvider filter contract: unsupported
    // predicates remain in a FilterExec while supported ones are still pushed
    // down into the native scan.
    let batches = SessionContext::new()
        .read_table(table)?
        .filter(filter.clone())?
        .select_columns(&["document_id"])?
        .collect()
        .await?;
    let mut ids = BTreeSet::new();
    for batch in batches {
        let values = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("normalized document_id is not Utf8")?;
        for row in 0..values.len() {
            if !values.is_null(row) {
                ids.insert(values.value(row).to_string());
            }
        }
    }
    Ok(ids)
}

/// Build a DataFusion provider while preserving Lance's JSONB extension type.
pub(crate) fn provider(rows: &[(String, String)]) -> Result<Arc<dyn TableProvider>> {
    let ids = StringArray::from(rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>());
    let data = rows
        .iter()
        .map(|(_, value)| encode_json(value).map_err(|error| anyhow::anyhow!(error.to_string())))
        .collect::<Result<Vec<_>>>()?;
    let data = LargeBinaryArray::from(data.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let batch = RecordBatch::try_new(schema(), vec![Arc::new(ids), Arc::new(data)])
        .context("build format virtual table batch")?;
    Ok(Arc::new(MemTable::try_new(schema(), vec![vec![batch]])?))
}

/// Lazy file-backed provider.  Registering a virtual table must not parse a
/// whole directory; decoding is deferred until the table is actually scanned.
#[derive(Debug)]
pub(crate) struct FileProvider {
    format: DocumentFormat,
    manifest: Arc<super::LocalQueryManifest>,
    max_file_bytes: u64,
    runs: Arc<dyn TableProvider>,
    steps: Arc<dyn TableProvider>,
    tool_calls: Arc<dyn TableProvider>,
}

impl FileProvider {
    pub(crate) fn new(
        format: DocumentFormat,
        manifest: Arc<super::LocalQueryManifest>,
        max_file_bytes: u64,
        runs: Arc<dyn TableProvider>,
        steps: Arc<dyn TableProvider>,
        tool_calls: Arc<dyn TableProvider>,
    ) -> Self {
        Self {
            format,
            manifest,
            max_file_bytes,
            runs,
            steps,
            tool_calls,
        }
    }
}

#[async_trait]
impl TableProvider for FileProvider {
    fn schema(&self) -> SchemaRef {
        schema()
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
        let normalized_predicates = filters
            .iter()
            .flat_map(|filter| normalized_json_predicates(self.format, filter))
            .collect::<Vec<_>>();
        let mut candidate_ids: Option<BTreeSet<String>> = None;
        if !matches!(
            self.format,
            DocumentFormat::Codex | DocumentFormat::ClaudeCode
        ) {
            for predicate in normalized_predicates {
                let table = match predicate.table {
                    NormalizedVirtualTable::Runs => self.runs.clone(),
                    NormalizedVirtualTable::Steps => self.steps.clone(),
                    NormalizedVirtualTable::ToolCalls => self.tool_calls.clone(),
                };
                let matching = matching_document_ids(table, &predicate.filter)
                    .await
                    .map_err(|error| DataFusionError::Execution(error.to_string()))?;
                candidate_ids = Some(match candidate_ids {
                    Some(current) => current.intersection(&matching).cloned().collect(),
                    None => matching,
                });
            }
        }
        let rows = super::document_source::virtual_rows_for_files(
            self.format,
            &self.manifest,
            self.max_file_bytes,
            candidate_ids.as_ref(),
        )
        .map_err(|error| DataFusionError::Execution(error.to_string()))?;
        let table =
            provider(&rows).map_err(|error| DataFusionError::Execution(error.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::logical_expr::ScalarUDF;
    use datafusion::logical_expr::expr::ScalarFunction;
    use lance_datafusion::udf::json::{json_extract_udf, json_get_string_udf};

    fn json_call(function: ScalarUDF, path: &str) -> Expr {
        Expr::ScalarFunction(ScalarFunction {
            func: Arc::new(function),
            args: vec![
                Expr::Column(Column::new_unqualified("data")),
                Expr::Literal(ScalarValue::Utf8(Some(path.to_owned())), None),
            ],
        })
    }

    fn string_literal(value: &str) -> Expr {
        Expr::Literal(ScalarValue::Utf8(Some(value.to_owned())), None)
    }

    #[test]
    fn virtual_table_names_use_query_friendly_aliases() {
        assert_eq!(table_name(DocumentFormat::CanonicalEvent), Some("events"));
        assert_eq!(table_name(DocumentFormat::AgenticMd), Some("markdown"));
        assert_eq!(table_name(DocumentFormat::ClaudeCode), Some("claude"));
        assert_eq!(table_name(DocumentFormat::Atif), Some("atif"));
    }

    #[test]
    fn root_json_predicate_routes_to_runs() {
        let filter = binary_expr(
            json_call(json_get_string_udf(), "session_id"),
            Operator::Eq,
            string_literal("session-1"),
        );
        let predicates = normalized_json_predicates(DocumentFormat::Atif, &filter);

        assert_eq!(predicates.len(), 1);
        assert_eq!(predicates[0].table, NormalizedVirtualTable::Runs);
        assert!(predicates[0].filter.to_string().contains("session_id"));
    }

    #[test]
    fn nested_tool_json_predicate_routes_to_step_and_tool_call_tables() {
        let filter = binary_expr(
            json_call(json_extract_udf(), "$.steps[4].tool_calls[0].function_name"),
            Operator::Eq,
            string_literal("\"knowledge_search\""),
        );
        let predicates = normalized_json_predicates(DocumentFormat::Atif, &filter);

        assert_eq!(predicates.len(), 2);
        assert_eq!(predicates[0].table, NormalizedVirtualTable::Steps);
        assert!(predicates[0].filter.to_string().contains("turn_ordinal"));
        assert_eq!(predicates[1].table, NormalizedVirtualTable::ToolCalls);
        let tool_filter = predicates[1].filter.to_string();
        assert!(tool_filter.contains("call_index"));
        assert!(tool_filter.contains("function_name"));
    }

    #[test]
    fn nested_step_scalar_predicates_route_to_steps() {
        let source_filter = binary_expr(
            json_call(json_extract_udf(), "$.steps[4].source"),
            Operator::Eq,
            string_literal("\"agent\""),
        );
        let id_filter = binary_expr(
            json_call(json_extract_udf(), "$.steps[4].step_id"),
            Operator::Eq,
            string_literal("5"),
        );
        let copied_filter = binary_expr(
            json_call(json_extract_udf(), "$.steps[4].is_copied_context"),
            Operator::Eq,
            string_literal("false"),
        );

        for filter in [source_filter, id_filter, copied_filter] {
            let predicates = normalized_json_predicates(DocumentFormat::Storyline, &filter);
            assert_eq!(predicates.len(), 1);
            assert_eq!(predicates[0].table, NormalizedVirtualTable::Steps);
            assert!(predicates[0].filter.to_string().contains("turn_ordinal"));
        }
    }

    #[test]
    fn reversed_root_comparison_is_normalized() {
        let filter = binary_expr(
            string_literal("session-1"),
            Operator::Eq,
            json_call(json_get_string_udf(), "session_id"),
        );
        let predicates = normalized_json_predicates(DocumentFormat::Atif, &filter);

        assert_eq!(predicates.len(), 1);
        assert_eq!(predicates[0].table, NormalizedVirtualTable::Runs);
    }

    #[test]
    fn or_predicate_is_left_for_exact_json_evaluation() {
        let left = binary_expr(
            json_call(json_get_string_udf(), "session_id"),
            Operator::Eq,
            string_literal("session-1"),
        );
        let right = binary_expr(
            json_call(json_get_string_udf(), "session_id"),
            Operator::Eq,
            string_literal("session-2"),
        );
        let filter = binary_expr(left, Operator::Or, right);

        assert!(normalized_json_predicates(DocumentFormat::Atif, &filter).is_empty());
    }

    #[test]
    fn nested_predicates_are_not_pushed_for_non_row_formats() {
        let filter = binary_expr(
            json_call(json_extract_udf(), "$.steps[4].source"),
            Operator::Eq,
            string_literal("\"agent\""),
        );

        assert!(normalized_json_predicates(DocumentFormat::Codex, &filter).is_empty());
        assert!(normalized_json_predicates(DocumentFormat::ClaudeCode, &filter).is_empty());
    }
}
