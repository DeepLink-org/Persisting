//! Deterministic AnalysisSpec → read-only SQL compiler.
//!
//! The model writes a spec. This module is the only place that produces SQL
//! on the main Analyze path. It does not talk to DataFusion; the server
//! supplies a live schema snapshot and physically plans the compiled SQL.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CompileError {
    pub code: String,
    pub message: String,
    pub field: Option<String>,
}

impl CompileError {
    fn new(code: &'static str, field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            field: field.map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompileScope {
    pub dataset: String,
    pub file: Option<String>,
    pub session_ids: Vec<String>,
    pub document_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisSpec {
    pub intent: String,
    pub grain: String,
    pub measure: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<SpecFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<Ranking>,
    pub output: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncomputable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecFilter {
    pub field: String,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledQuery {
    pub spec: AnalysisSpec,
    pub sql: String,
    pub assumptions: Vec<String>,
    pub identity_columns: Vec<String>,
    pub expected_columns: Vec<String>,
    pub output: String,
}

#[derive(Clone, Copy)]
struct GrainInfo {
    name: &'static str,
    table: &'static str,
    alias: &'static str,
    identity: &'static [&'static str],
}

const RUN: GrainInfo = GrainInfo {
    name: "run",
    table: "runs",
    alias: "r",
    identity: &["_file_", "session_id", "document_id", "agent_id"],
};
const STEP: GrainInfo = GrainInfo {
    name: "step",
    table: "steps",
    alias: "s",
    identity: &["_file_", "session_id", "step_id", "document_id"],
};
const TOOL: GrainInfo = GrainInfo {
    name: "tool_call",
    table: "tool_calls",
    alias: "t",
    identity: &[
        "_file_",
        "session_id",
        "step_id",
        "tool_call_id",
        "function_name",
    ],
};

#[derive(Clone, Copy)]
enum MeasureKind {
    Count,
    Column(&'static str),
    PerRunCount { from: GrainInfo },
}

struct MeasureInfo {
    name: &'static str,
    grains: &'static [&'static str],
    kind: MeasureKind,
    assumption: &'static str,
}

const MEASURES: &[MeasureInfo] = &[
    MeasureInfo {
        name: "row_count",
        grains: &["run", "step", "tool_call"],
        kind: MeasureKind::Count,
        assumption: "row_count is COUNT(*) at the selected grain",
    },
    MeasureInfo {
        name: "tool_call_count",
        grains: &["tool_call"],
        kind: MeasureKind::Count,
        assumption: "tool_call_count is COUNT(*) of tool_calls rows",
    },
    MeasureInfo {
        name: "step_count_per_run",
        grains: &["run"],
        kind: MeasureKind::PerRunCount { from: STEP },
        assumption: "step_count_per_run counts steps rows sharing _file_ and session_id",
    },
    MeasureInfo {
        name: "tool_call_count_per_run",
        grains: &["run"],
        kind: MeasureKind::PerRunCount { from: TOOL },
        assumption: "tool_call_count_per_run counts tool_calls rows sharing _file_ and session_id",
    },
    MeasureInfo {
        name: "step_latency_ms",
        grains: &["step"],
        kind: MeasureKind::Column("latency"),
        assumption: "step_latency_ms reads milliseconds from latency and ignores NULL",
    },
    MeasureInfo {
        name: "step_ttft_ms",
        grains: &["step"],
        kind: MeasureKind::Column("ttft"),
        assumption: "step_ttft_ms reads milliseconds from ttft and ignores NULL",
    },
    MeasureInfo {
        name: "tool_duration_ms",
        grains: &["tool_call"],
        kind: MeasureKind::Column("duration"),
        assumption: "tool_duration_ms reads milliseconds from duration and ignores NULL",
    },
];

const RUN_DIMENSIONS: &[&str] = &[
    "agent_id",
    "agent_name",
    "agent_version",
    "agent_model_name",
    "_file_",
];
const STEP_DIMENSIONS: &[&str] = &["source", "effective_kind", "model_name", "had_tool_calls"];
const TOOL_DIMENSIONS: &[&str] = &["function_name"];

const INTENTS: &[&str] = &[
    "distribution",
    "compare",
    "rank_outlier",
    "composition",
    "drilldown",
];

pub fn compile(
    spec: AnalysisSpec,
    schema: &[TableSchema],
    scope: &CompileScope,
) -> Result<CompiledQuery, CompileError> {
    let intent = validate_intent(&spec.intent)?;
    let grain = validate_grain(&spec.grain)?;
    let measure = validate_measure(&spec.measure, grain)?;
    let output = validate_output(&spec.output, intent)?.to_string();
    if scope.dataset.trim().is_empty() {
        return Err(CompileError::new(
            "invalid_scope",
            Some("scope"),
            "scope dataset is required",
        ));
    }
    if spec
        .filters
        .iter()
        .any(|filter| filter.field.ends_with("_json"))
        || spec.measure.ends_with("_json")
    {
        return Err(uncomputable_json());
    }
    let grain_table = qualified_table(&scope.dataset, grain.table);
    let grain_schema = require_table(schema, &grain_table)?;
    let identity_columns: Vec<String> = grain
        .identity
        .iter()
        .copied()
        .filter(|column| has_column(grain_schema, column))
        .map(str::to_owned)
        .collect();

    let dimension = match spec
        .dimension
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(dimension) => Some(resolve_dimension(dimension, grain, schema, &scope.dataset)?),
        None => {
            if matches!(intent, "compare" | "composition") {
                return Err(CompileError::new(
                    "missing_dimension",
                    Some("dimension"),
                    format!("{intent} requires a registered dimension"),
                ));
            }
            None
        }
    };

    if let MeasureKind::Column(column) = measure.kind {
        require_column(grain_schema, column, "measure")?;
    }

    let mut assumptions = vec![measure.assumption.to_string()];
    let mut predicates = scope_predicates(grain.alias, scope);
    predicates.extend(compile_filters(&spec.filters, grain, grain_schema)?);
    if let MeasureKind::Column(column) = measure.kind {
        predicates.push(format!("{}.{column} IS NOT NULL", grain.alias));
    }

    let join = dimension
        .as_ref()
        .and_then(|dimension| dimension.join_clause.clone());
    if let Some(dim) = dimension.as_ref() {
        require_column_in(schema, &dim.table_name, &dim.column, "dimension")?;
    }

    let ranking = resolve_ranking(spec.ranking.as_ref(), intent, measure)?;
    let measure_expr = measure_expression(measure, grain, intent);
    let measure_select = format!("{measure_expr} AS {}", measure.name);

    let mut select = Vec::new();
    let include_identity = matches!(intent, "drilldown" | "rank_outlier");
    if include_identity {
        for column in &identity_columns {
            select.push(format!("{}.{}", grain.alias, quote_ident(column)));
        }
    }
    if let Some(dim) = dimension.as_ref()
        && (!include_identity || !identity_columns.iter().any(|column| column == &dim.column))
    {
        select.push(format!("{} AS {}", dim.qualified, dim.column));
    }
    select.push(measure_select);

    let mut sql = String::new();
    sql.push_str("SELECT ");
    sql.push_str(&select.join(", "));
    sql.push_str("\nFROM ");
    sql.push_str(&format!(
        "{} AS {}",
        quote_qualified(&grain_table),
        grain.alias
    ));
    if let Some(join) = join {
        sql.push('\n');
        sql.push_str(&join);
    }
    if let Some(per_run) = per_run_count_join(measure, grain, &scope.dataset) {
        sql.push('\n');
        sql.push_str(&per_run);
    }
    if !predicates.is_empty() {
        sql.push_str("\nWHERE ");
        sql.push_str(&predicates.join(" AND "));
    }
    if matches!(intent, "compare" | "composition")
        && let Some(dim) = dimension.as_ref()
    {
        sql.push_str("\nGROUP BY ");
        sql.push_str(&dim.qualified);
    }
    if let Some(ranking) = ranking {
        match ranking {
            RankingKind::TopN { n } => {
                sql.push_str(&format!("\nORDER BY {} DESC\nLIMIT {n}", measure.name));
            }
            RankingKind::BottomN { n } => {
                sql.push_str(&format!("\nORDER BY {} ASC\nLIMIT {n}", measure.name));
            }
            RankingKind::Outlier => {
                let MeasureKind::Column(column) = measure.kind else {
                    return Err(CompileError::new(
                        "invalid_ranking",
                        Some("ranking"),
                        "outlier ranking requires a numeric column measure",
                    ));
                };
                let mut p95_predicates = scope_predicates(grain.alias, scope);
                p95_predicates.push(format!("{}.{column} IS NOT NULL", grain.alias));
                let p95 = format!(
                    "{}.{} > (SELECT approx_percentile_cont({}.{column}, 0.95) FROM {} AS {} WHERE {})",
                    grain.alias,
                    column,
                    grain.alias,
                    quote_qualified(&grain_table),
                    grain.alias,
                    p95_predicates.join(" AND ")
                );
                if predicates.is_empty() {
                    sql.push_str("\nWHERE ");
                    sql.push_str(&p95);
                } else {
                    sql.push_str(" AND ");
                    sql.push_str(&p95);
                }
                assumptions.push(format!(
                    "{} outlier keeps rows above the in-scope P95",
                    measure.name
                ));
            }
        }
    }

    let mut expected_columns = Vec::new();
    if include_identity {
        expected_columns.extend(identity_columns.iter().cloned());
    }
    if let Some(dim) = dimension.as_ref()
        && !expected_columns.iter().any(|column| column == &dim.column)
    {
        expected_columns.push(dim.column.clone());
    }
    expected_columns.push(measure.name.to_string());

    let mut compiled_spec = spec;
    compiled_spec.assumptions = assumptions.clone();
    compiled_spec.identity_columns = identity_columns.clone();
    compiled_spec.uncomputable_reason = None;
    compiled_spec.output = output.clone();

    Ok(CompiledQuery {
        spec: compiled_spec,
        sql,
        assumptions,
        identity_columns,
        expected_columns,
        output,
    })
}

fn validate_intent(intent: &str) -> Result<&str, CompileError> {
    let intent = intent.trim();
    if INTENTS.contains(&intent) {
        return Ok(intent);
    }
    Err(CompileError::new(
        "unknown_intent",
        Some("intent"),
        "intent must be distribution, compare, rank_outlier, composition, or drilldown; causal questions are not SQL targets",
    ))
}

fn validate_grain(grain: &str) -> Result<GrainInfo, CompileError> {
    match grain.trim() {
        "run" => Ok(RUN),
        "step" => Ok(STEP),
        "tool_call" => Ok(TOOL),
        _ => Err(CompileError::new(
            "unknown_grain",
            Some("grain"),
            "grain must be run, step, or tool_call",
        )),
    }
}

fn validate_measure(name: &str, grain: GrainInfo) -> Result<&'static MeasureInfo, CompileError> {
    let name = name.trim();
    if let Some(reason) = uncomputable_measure(name) {
        return Err(CompileError::new("uncomputable", Some("measure"), reason));
    }
    let Some(measure) = MEASURES.iter().find(|measure| measure.name == name) else {
        return Err(CompileError::new(
            "unknown_measure",
            Some("measure"),
            format!("measure '{name}' is not registered"),
        ));
    };
    if !measure.grains.contains(&grain.name) {
        return Err(CompileError::new(
            "invalid_measure",
            Some("measure"),
            format!(
                "measure '{}' is not available at {} grain",
                measure.name, grain.name
            ),
        ));
    }
    Ok(measure)
}

fn validate_output<'a>(output: &'a str, intent: &str) -> Result<&'a str, CompileError> {
    let output = output.trim();
    let allowed = match intent {
        "compare" => &["comparison", "table"][..],
        "distribution" => &["distribution", "table"][..],
        _ => &["table"][..],
    };
    if allowed.contains(&output) {
        return Ok(output);
    }
    Err(CompileError::new(
        "invalid_output",
        Some("output"),
        format!("output '{output}' is not valid for intent {intent}"),
    ))
}

fn uncomputable_measure(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("status") || lower.contains("success") || lower.contains("fail") {
        return Some("runs has no unified status; v1 cannot group by success or failure");
    }
    if lower.contains("token") {
        return Some("token measures are not first-class columns in v1");
    }
    if lower.contains("json") || lower == "final_metrics" {
        return Some("JSON extraction is not a registered measure");
    }
    None
}

fn uncomputable_json() -> CompileError {
    CompileError::new(
        "uncomputable",
        Some("measure"),
        "JSON extraction is not a registered measure",
    )
}

fn qualified_table(dataset: &str, table: &str) -> String {
    format!("{}.{}", dataset.trim(), table)
}

fn require_table<'a>(
    schema: &'a [TableSchema],
    name: &str,
) -> Result<&'a TableSchema, CompileError> {
    schema
        .iter()
        .find(|table| table.name == name)
        .ok_or_else(|| {
            CompileError::new(
                "unknown_table",
                Some("grain"),
                format!("schema does not contain {name}"),
            )
        })
}

fn has_column(table: &TableSchema, column: &str) -> bool {
    table.columns.iter().any(|name| name == column)
}

fn require_column(table: &TableSchema, column: &str, field: &str) -> Result<(), CompileError> {
    if has_column(table, column) {
        Ok(())
    } else {
        Err(CompileError::new(
            "unknown_column",
            Some(field),
            format!("{} has no column {column}", table.name),
        ))
    }
}

fn require_column_in(
    schema: &[TableSchema],
    table_name: &str,
    column: &str,
    field: &str,
) -> Result<(), CompileError> {
    let table = require_table(schema, table_name)?;
    require_column(table, column, field)
}

struct OwnedDimension {
    column: String,
    qualified: String,
    table_name: String,
    join_clause: Option<String>,
}

fn resolve_dimension(
    dimension: &str,
    grain: GrainInfo,
    schema: &[TableSchema],
    dataset: &str,
) -> Result<OwnedDimension, CompileError> {
    let dimension = dimension.trim();
    if dimension.ends_with("_json") {
        return Err(CompileError::new(
            "uncomputable",
            Some("dimension"),
            "JSON fields are not registered dimensions",
        ));
    }
    let same_grain = match grain.name {
        "run" => RUN_DIMENSIONS.contains(&dimension),
        "step" => STEP_DIMENSIONS.contains(&dimension),
        "tool_call" => TOOL_DIMENSIONS.contains(&dimension),
        _ => false,
    };
    if same_grain {
        let table_name = qualified_table(dataset, grain.table);
        require_column_in(schema, &table_name, dimension, "dimension")?;
        return Ok(OwnedDimension {
            qualified: format!("{}.{}", grain.alias, quote_ident(dimension)),
            table_name,
            column: dimension.to_string(),
            join_clause: None,
        });
    }
    if RUN_DIMENSIONS.contains(&dimension) && grain.name != "run" {
        let runs = qualified_table(dataset, RUN.table);
        require_column_in(schema, &runs, dimension, "dimension")?;
        let join = format!(
            "INNER JOIN {} AS r ON r._file_ = {}._file_ AND r.session_id = {}.session_id",
            quote_qualified(&runs),
            grain.alias,
            grain.alias
        );
        return Ok(OwnedDimension {
            column: dimension.to_string(),
            qualified: format!("r.{}", quote_ident(dimension)),
            table_name: runs,
            join_clause: Some(join),
        });
    }
    Err(CompileError::new(
        "unknown_column",
        Some("dimension"),
        format!(
            "dimension '{dimension}' is not registered for {} grain",
            grain.name
        ),
    ))
}

fn scope_predicates(alias: &str, scope: &CompileScope) -> Vec<String> {
    let mut predicates = Vec::new();
    if let Some(file) = scope
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        predicates.push(format!("{alias}._file_ = {}", sql_string(file)));
    }
    let mut session_ids = scope.session_ids.clone();
    session_ids.sort();
    session_ids.dedup();
    match session_ids.as_slice() {
        [] => {}
        [session_id] => predicates.push(format!("{alias}.session_id = {}", sql_string(session_id))),
        ids => {
            let list = ids
                .iter()
                .map(|id| sql_string(id))
                .collect::<Vec<_>>()
                .join(", ");
            predicates.push(format!("{alias}.session_id IN ({list})"));
        }
    }
    if let Some(document_id) = scope
        .document_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        predicates.push(format!("{alias}.document_id = {}", sql_string(document_id)));
    }
    predicates
}

fn compile_filters(
    filters: &[SpecFilter],
    grain: GrainInfo,
    table: &TableSchema,
) -> Result<Vec<String>, CompileError> {
    let mut predicates = Vec::new();
    for filter in filters {
        if filter.field.trim().is_empty() {
            return Err(CompileError::new(
                "invalid_filter",
                Some("filters"),
                "filter field is required",
            ));
        }
        if filter.field.ends_with("_json") {
            return Err(uncomputable_json());
        }
        require_column(table, &filter.field, "filters")?;
        let left = format!("{}.{}", grain.alias, quote_ident(&filter.field));
        predicates.push(compile_predicate(&left, &filter.op, filter.value.as_ref())?);
    }
    Ok(predicates)
}

fn compile_predicate(
    left: &str,
    op: &str,
    value: Option<&serde_json::Value>,
) -> Result<String, CompileError> {
    match op.trim() {
        "eq" => Ok(format!("{left} = {}", literal(value)?)),
        "neq" => Ok(format!("{left} <> {}", literal(value)?)),
        "gt" => Ok(format!("{left} > {}", literal(value)?)),
        "gte" => Ok(format!("{left} >= {}", literal(value)?)),
        "lt" => Ok(format!("{left} < {}", literal(value)?)),
        "lte" => Ok(format!("{left} <= {}", literal(value)?)),
        "like" => Ok(format!("{left} LIKE {}", literal(value)?)),
        "not_null" => Ok(format!("{left} IS NOT NULL")),
        "is_null" => Ok(format!("{left} IS NULL")),
        "in" => {
            let Some(serde_json::Value::Array(values)) = value else {
                return Err(CompileError::new(
                    "invalid_filter",
                    Some("filters"),
                    "in filters require an array value",
                ));
            };
            if values.is_empty() {
                return Err(CompileError::new(
                    "invalid_filter",
                    Some("filters"),
                    "in filters require at least one value",
                ));
            }
            let list = values
                .iter()
                .map(|value| literal(Some(value)))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            Ok(format!("{left} IN ({list})"))
        }
        other => Err(CompileError::new(
            "invalid_filter",
            Some("filters"),
            format!("filter operator '{other}' is not allowed"),
        )),
    }
}

fn literal(value: Option<&serde_json::Value>) -> Result<String, CompileError> {
    match value {
        Some(serde_json::Value::String(text)) => {
            if text.contains(';') || text.contains("--") {
                return Err(CompileError::new(
                    "invalid_filter",
                    Some("filters"),
                    "filter values cannot contain SQL fragments",
                ));
            }
            Ok(sql_string(text))
        }
        Some(serde_json::Value::Number(number)) => Ok(number.to_string()),
        Some(serde_json::Value::Bool(true)) => Ok("TRUE".into()),
        Some(serde_json::Value::Bool(false)) => Ok("FALSE".into()),
        Some(serde_json::Value::Null) | None => Err(CompileError::new(
            "invalid_filter",
            Some("filters"),
            "filter value is required",
        )),
        Some(_) => Err(CompileError::new(
            "invalid_filter",
            Some("filters"),
            "filter values must be a string, number, or boolean",
        )),
    }
}

fn measure_expression(measure: &MeasureInfo, grain: GrainInfo, intent: &str) -> String {
    let aggregated = matches!(intent, "compare" | "composition");
    match measure.kind {
        MeasureKind::Count => "COUNT(*)".into(),
        MeasureKind::Column(column) => {
            let expr = format!("{}.{}", grain.alias, quote_ident(column));
            if aggregated {
                format!("AVG({expr})")
            } else {
                expr
            }
        }
        MeasureKind::PerRunCount { .. } => {
            let expr = format!("COALESCE(per_run.{}, 0)", measure.name);
            if aggregated {
                format!("AVG({expr})")
            } else {
                expr
            }
        }
    }
}

fn per_run_count_join(measure: &MeasureInfo, grain: GrainInfo, dataset: &str) -> Option<String> {
    let MeasureKind::PerRunCount { from } = measure.kind else {
        return None;
    };
    let from_table = quote_qualified(&qualified_table(dataset, from.table));
    Some(format!(
        "LEFT JOIN (\n  SELECT {child}._file_, {child}.session_id, COUNT(*) AS {name}\n  FROM {from_table} AS {child}\n  GROUP BY {child}._file_, {child}.session_id\n) AS per_run ON per_run._file_ = {parent}._file_ AND per_run.session_id = {parent}.session_id",
        child = from.alias,
        parent = grain.alias,
        name = measure.name,
    ))
}

enum RankingKind {
    TopN { n: u32 },
    BottomN { n: u32 },
    Outlier,
}

fn resolve_ranking(
    ranking: Option<&Ranking>,
    intent: &str,
    measure: &MeasureInfo,
) -> Result<Option<RankingKind>, CompileError> {
    let Some(ranking) = ranking else {
        if intent == "rank_outlier" {
            return Ok(Some(RankingKind::TopN { n: 20 }));
        }
        return Ok(None);
    };
    let n = ranking.n.unwrap_or(20).clamp(1, 100);
    match ranking.kind.trim() {
        "top_n" => Ok(Some(RankingKind::TopN { n })),
        "bottom_n" => Ok(Some(RankingKind::BottomN { n })),
        "outlier" => {
            if !matches!(measure.kind, MeasureKind::Column(_)) {
                return Err(CompileError::new(
                    "invalid_ranking",
                    Some("ranking"),
                    "outlier ranking requires a numeric column measure",
                ));
            }
            Ok(Some(RankingKind::Outlier))
        }
        other => Err(CompileError::new(
            "invalid_ranking",
            Some("ranking"),
            format!("ranking '{other}' is not supported"),
        )),
    }
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn quote_ident(name: &str) -> String {
    if is_simple_ident(name) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

fn quote_qualified(name: &str) -> String {
    name.split('.')
        .map(quote_ident)
        .collect::<Vec<_>>()
        .join(".")
}

fn is_simple_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Vec<TableSchema> {
        vec![
            TableSchema {
                name: "dataset.runs".into(),
                columns: vec![
                    "_file_".into(),
                    "document_id".into(),
                    "session_id".into(),
                    "agent_id".into(),
                    "agent_name".into(),
                    "agent_version".into(),
                    "agent_model_name".into(),
                    "trajectory_id_explicit".into(),
                    "final_metrics".into(),
                ],
            },
            TableSchema {
                name: "dataset.steps".into(),
                columns: vec![
                    "_file_".into(),
                    "document_id".into(),
                    "session_id".into(),
                    "step_id".into(),
                    "source".into(),
                    "effective_kind".into(),
                    "model_name".into(),
                    "had_tool_calls".into(),
                    "latency".into(),
                    "ttft".into(),
                ],
            },
            TableSchema {
                name: "dataset.tool_calls".into(),
                columns: vec![
                    "_file_".into(),
                    "document_id".into(),
                    "session_id".into(),
                    "step_id".into(),
                    "tool_call_id".into(),
                    "function_name".into(),
                    "duration".into(),
                ],
            },
        ]
    }

    fn dataset_scope() -> CompileScope {
        CompileScope {
            dataset: "dataset".into(),
            ..CompileScope::default()
        }
    }

    fn spec(intent: &str, grain: &str, measure: &str, output: &str) -> AnalysisSpec {
        AnalysisSpec {
            intent: intent.into(),
            grain: grain.into(),
            measure: measure.into(),
            dimension: None,
            filters: Vec::new(),
            ranking: None,
            output: output.into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        }
    }

    fn sql_of(spec: AnalysisSpec) -> String {
        compile(spec, &schema(), &dataset_scope())
            .unwrap_or_else(|error| panic!("{error}"))
            .sql
    }

    fn normalize(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn distribution_emits_non_null_latency_values() {
        assert_eq!(
            normalize(&sql_of(spec(
                "distribution",
                "step",
                "step_latency_ms",
                "distribution"
            ))),
            "SELECT s.latency AS step_latency_ms FROM dataset.steps AS s WHERE s.latency IS NOT NULL"
        );
    }

    #[test]
    fn compare_groups_step_count_by_model() {
        let mut spec = spec("compare", "run", "step_count_per_run", "comparison");
        spec.dimension = Some("agent_model_name".into());
        let sql = normalize(&sql_of(spec));
        assert!(
            !sql.contains("(SELECT COUNT(*)"),
            "DataFusion cannot execute correlated scalar subqueries: {sql}"
        );
        assert_eq!(
            sql,
            "SELECT r.agent_model_name AS agent_model_name, AVG(COALESCE(per_run.step_count_per_run, 0)) AS step_count_per_run FROM dataset.runs AS r LEFT JOIN ( SELECT s._file_, s.session_id, COUNT(*) AS step_count_per_run FROM dataset.steps AS s GROUP BY s._file_, s.session_id ) AS per_run ON per_run._file_ = r._file_ AND per_run.session_id = r.session_id GROUP BY r.agent_model_name"
        );
    }

    #[test]
    fn drilldown_step_count_joins_grouped_steps() {
        let sql = normalize(&sql_of(spec(
            "drilldown",
            "run",
            "step_count_per_run",
            "table",
        )));
        assert!(
            !sql.contains("(SELECT COUNT(*)"),
            "DataFusion cannot execute correlated scalar subqueries: {sql}"
        );
        assert_eq!(
            sql,
            "SELECT r._file_, r.session_id, r.document_id, r.agent_id, COALESCE(per_run.step_count_per_run, 0) AS step_count_per_run FROM dataset.runs AS r LEFT JOIN ( SELECT s._file_, s.session_id, COUNT(*) AS step_count_per_run FROM dataset.steps AS s GROUP BY s._file_, s.session_id ) AS per_run ON per_run._file_ = r._file_ AND per_run.session_id = r.session_id"
        );
    }

    #[test]
    fn compare_tool_call_count_per_run_joins_grouped_tool_calls() {
        let mut spec = spec("compare", "run", "tool_call_count_per_run", "comparison");
        spec.dimension = Some("agent_model_name".into());
        let sql = normalize(&sql_of(spec));
        assert!(
            !sql.contains("(SELECT COUNT(*)"),
            "DataFusion cannot execute correlated scalar subqueries: {sql}"
        );
        assert_eq!(
            sql,
            "SELECT r.agent_model_name AS agent_model_name, AVG(COALESCE(per_run.tool_call_count_per_run, 0)) AS tool_call_count_per_run FROM dataset.runs AS r LEFT JOIN ( SELECT t._file_, t.session_id, COUNT(*) AS tool_call_count_per_run FROM dataset.tool_calls AS t GROUP BY t._file_, t.session_id ) AS per_run ON per_run._file_ = r._file_ AND per_run.session_id = r.session_id GROUP BY r.agent_model_name"
        );
    }

    #[test]
    fn rank_outlier_orders_slowest_steps() {
        let mut spec = spec("rank_outlier", "step", "step_latency_ms", "table");
        spec.ranking = Some(Ranking {
            kind: "top_n".into(),
            n: Some(20),
        });
        let compiled = compile(spec, &schema(), &dataset_scope()).unwrap();
        assert_eq!(
            normalize(&compiled.sql),
            "SELECT s._file_, s.session_id, s.step_id, s.document_id, s.latency AS step_latency_ms FROM dataset.steps AS s WHERE s.latency IS NOT NULL ORDER BY step_latency_ms DESC LIMIT 20"
        );
        assert_eq!(
            compiled.identity_columns,
            ["_file_", "session_id", "step_id", "document_id"]
        );
    }

    #[test]
    fn composition_counts_tool_calls_by_function() {
        let mut spec = spec("composition", "tool_call", "tool_call_count", "table");
        spec.dimension = Some("function_name".into());
        assert_eq!(
            normalize(&sql_of(spec)),
            "SELECT t.function_name AS function_name, COUNT(*) AS tool_call_count FROM dataset.tool_calls AS t GROUP BY t.function_name"
        );
    }

    #[test]
    fn drilldown_keeps_identity_for_a_run() {
        let spec = spec("drilldown", "tool_call", "tool_duration_ms", "table");
        let compiled = compile(
            spec,
            &schema(),
            &CompileScope {
                dataset: "dataset".into(),
                file: Some("gateway.json".into()),
                session_ids: vec!["json-session".into()],
                document_id: Some("doc-1".into()),
            },
        )
        .unwrap();
        assert_eq!(
            normalize(&compiled.sql),
            "SELECT t._file_, t.session_id, t.step_id, t.tool_call_id, t.function_name, t.duration AS tool_duration_ms FROM dataset.tool_calls AS t WHERE t._file_ = 'gateway.json' AND t.session_id = 'json-session' AND t.document_id = 'doc-1' AND t.duration IS NOT NULL"
        );
    }

    #[test]
    fn cross_grain_dimension_joins_runs() {
        let mut spec = spec("composition", "tool_call", "tool_call_count", "table");
        spec.dimension = Some("agent_model_name".into());
        assert_eq!(
            normalize(&sql_of(spec)),
            "SELECT r.agent_model_name AS agent_model_name, COUNT(*) AS tool_call_count FROM dataset.tool_calls AS t INNER JOIN dataset.runs AS r ON r._file_ = t._file_ AND r.session_id = t.session_id GROUP BY r.agent_model_name"
        );
    }

    #[test]
    fn rejects_uncomputable_and_unknown_inputs() {
        for (mut spec, field, code) in [
            (
                spec("why_failed", "run", "row_count", "table"),
                "intent",
                "unknown_intent",
            ),
            (
                spec("compare", "run", "status", "comparison"),
                "measure",
                "uncomputable",
            ),
            (
                spec("distribution", "run", "prompt_tokens", "distribution"),
                "measure",
                "uncomputable",
            ),
            (
                spec("distribution", "run", "final_metrics", "distribution"),
                "measure",
                "uncomputable",
            ),
            (
                spec("distribution", "step", "unknown_ms", "distribution"),
                "measure",
                "unknown_measure",
            ),
        ] {
            if spec.intent == "compare" {
                spec.dimension = Some("agent_model_name".into());
            }
            let error = compile(spec, &schema(), &dataset_scope()).unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.field.as_deref(), Some(field));
        }
        let mut unknown_dim = spec("composition", "run", "row_count", "table");
        unknown_dim.dimension = Some("not_a_column".into());
        let error = compile(unknown_dim, &schema(), &dataset_scope()).unwrap_err();
        assert_eq!(error.code, "unknown_column");
        assert!(
            compile(
                spec("compare", "run", "row_count", "comparison"),
                &schema(),
                &dataset_scope()
            )
            .unwrap_err()
            .message
            .contains("dimension")
        );
    }

    #[test]
    fn same_spec_emits_the_same_sql() {
        let mut spec = spec("compare", "run", "step_count_per_run", "comparison");
        spec.dimension = Some("agent_model_name".into());
        let left = compile(spec.clone(), &schema(), &dataset_scope())
            .unwrap()
            .sql;
        let right = compile(spec, &schema(), &dataset_scope()).unwrap().sql;
        assert_eq!(left, right);
    }

    #[test]
    fn filter_equality_is_parameterized() {
        let mut spec = spec("drilldown", "step", "row_count", "table");
        spec.filters.push(SpecFilter {
            field: "source".into(),
            op: "eq".into(),
            value: Some(json!("agent")),
        });
        let sql = sql_of(spec);
        assert!(normalize(&sql).contains("s.source = 'agent'"));
        assert!(!sql.contains(';'));
    }

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn simple_identifiers_are_not_quoted(
                ident in proptest::string::string_regex("[A-Za-z_][A-Za-z0-9_]{0,32}").unwrap(),
            ) {
                prop_assert!(is_simple_ident(&ident));
                prop_assert_eq!(quote_ident(&ident), ident);
            }

            #[test]
            fn sql_string_escapes_quotes_without_injection(
                value in proptest::string::string_regex("[A-Za-z0-9 _.'-]{0,64}").unwrap(),
            ) {
                let escaped = sql_string(&value);
                prop_assert!(escaped.starts_with('\''));
                prop_assert!(escaped.ends_with('\''));
                prop_assert!(!escaped[1..escaped.len() - 1].contains(';'));
                prop_assert_eq!(escaped.matches('\'').count(), 2 + 2 * value.matches('\'').count());
            }

            #[test]
            fn scope_session_predicates_are_sorted_and_deduplicated(
                ids in proptest::collection::vec(
                    proptest::string::string_regex("[a-z0-9-]{1,12}").unwrap(),
                    0..20,
                ),
            ) {
                let scope = CompileScope {
                    dataset: "dataset".into(),
                    session_ids: ids.clone(),
                    ..CompileScope::default()
                };
                let predicates = scope_predicates("s", &scope);
                let mut expected = ids;
                expected.sort();
                expected.dedup();
                match expected.as_slice() {
                    [] => prop_assert!(predicates.is_empty()),
                    [id] => prop_assert_eq!(predicates, vec![format!("s.session_id = {}", sql_string(id))]),
                    ids => {
                        let expected = format!(
                            "s.session_id IN ({})",
                            ids.iter().map(|id| sql_string(id)).collect::<Vec<_>>().join(", ")
                        );
                        prop_assert_eq!(predicates, vec![expected]);
                    }
                }
            }

        }
    }
}
