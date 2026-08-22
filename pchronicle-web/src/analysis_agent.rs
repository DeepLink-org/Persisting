#![allow(dead_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::analysis_session::{
    AnalysisInterpretation, AnalysisPlan, AnalysisScope, AnalysisScopeItem, EvidenceReference,
    SuggestedView,
};
use crate::llm::{self, CompletionRequest, LlmConfig};
use crate::model::QueryCatalog;
use crate::result_profile::{AnalysisRefinement, ColumnProfile};

pub const EVIDENCE_DIGEST_BYTES: usize = 64 * 1024;
const SQL_DIGEST_CHARS: usize = 8 * 1024;
const CELL_DIGEST_CHARS: usize = 512;
const MAX_DIGEST_ROWS: usize = 50;
const INTERACTIVE_MAX_ROWS: usize = 100;
const INTERACTIVE_MAX_BYTES: usize = 4 * 1024 * 1024;
const QUESTION_DIGEST_CHARS: usize = 4 * 1024;
const SCOPE_TEXT_DIGEST_CHARS: usize = 512;
const PROFILE_TEXT_DIGEST_CHARS: usize = 512;
const MAX_SCOPE_ITEMS: usize = 16;
const MAX_DIGEST_COLUMNS: usize = 64;

pub struct PlanRequest {
    pub config: LlmConfig,
    pub catalog: QueryCatalog,
    pub scope: AnalysisScope,
    pub question: String,
    pub plan_id: u64,
    pub previous_plan: Option<AnalysisPlan>,
    pub refinement: Option<AnalysisRefinement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDigest {
    pub question: String,
    pub scope: AnalysisScope,
    pub sql: String,
    pub columns: Vec<String>,
    pub profiles: Vec<ColumnProfile>,
    pub rows: Vec<Value>,
    pub returned_rows: usize,
    pub query_truncated: bool,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub digest_truncated: bool,
}

pub struct InterpretationRequest {
    pub config: LlmConfig,
    pub revision_id: u64,
    pub digest: EvidenceDigest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisAgentError {
    pub message: String,
}

impl AnalysisAgentError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for AnalysisAgentError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub async fn generate_plan(request: PlanRequest) -> Result<AnalysisPlan, AnalysisAgentError> {
    let system = plan_system_prompt(
        &request.catalog,
        &request.scope,
        request.previous_plan.as_ref(),
        request.refinement.as_ref(),
    )?;
    let messages = vec![json!({
        "role": "user",
        "content": serde_json::to_string(&json!({"question": request.question}))?,
    })];
    let content = request_json_content(&request.config, &system, messages).await?;
    match parse_plan_content(&content, request.plan_id, &request.question) {
        Ok(plan) => Ok(plan),
        Err(first_error) => {
            let repair_messages = vec![
                json!({"role":"user", "content": content}),
                json!({
                    "role":"user",
                    "content": format!(
                        "Return one corrected AnalysisPlan JSON object only. Validation error: {}",
                        first_error.message
                    ),
                }),
            ];
            let repaired = request_json_content(&request.config, &system, repair_messages).await?;
            parse_plan_content(&repaired, request.plan_id, &request.question)
        }
    }
}

pub async fn interpret(
    request: InterpretationRequest,
) -> Result<AnalysisInterpretation, AnalysisAgentError> {
    let system = interpretation_system_prompt();
    let messages = vec![json!({
        "role": "user",
        "content": serde_json::to_string(&request.digest)?,
    })];
    let content = request_json_content(&request.config, &system, messages).await?;
    match parse_interpretation_content(&content)
        .and_then(|interpretation| validate_interpretation(interpretation, &request.digest))
    {
        Ok(interpretation) => Ok(interpretation),
        Err(first_error) => {
            let repair_messages = vec![
                json!({"role":"user", "content": content}),
                json!({
                    "role":"user",
                    "content": format!(
                        "Return one corrected AnalysisInterpretation JSON object only. Validation error: {}",
                        first_error.message
                    ),
                }),
            ];
            let repaired = request_json_content(&request.config, &system, repair_messages).await?;
            parse_interpretation_content(&repaired)
                .and_then(|interpretation| validate_interpretation(interpretation, &request.digest))
        }
    }
}

pub fn build_evidence_digest(
    plan: &AnalysisPlan,
    scope: &AnalysisScope,
    evidence: &crate::model::QueryEvidence,
    profiles: &[ColumnProfile],
) -> EvidenceDigest {
    let (question, question_truncated) = clamp_text(&plan.question, QUESTION_DIGEST_CHARS);
    let (sql, sql_truncated) = clamp_text(&plan.sql, SQL_DIGEST_CHARS);
    let (scope, scope_truncated) = compact_scope(scope);
    let (profiles, profiles_truncated) = compact_profiles(profiles);
    let (columns, columns_truncated) = digest_columns(&evidence.rows);
    let mut digest = EvidenceDigest {
        question,
        scope,
        sql,
        columns,
        profiles,
        rows: Vec::new(),
        returned_rows: evidence.returned_rows,
        query_truncated: evidence.truncated,
        max_rows: evidence.max_rows,
        max_bytes: evidence.max_bytes,
        digest_truncated: question_truncated
            || sql_truncated
            || scope_truncated
            || profiles_truncated
            || columns_truncated,
    };

    fit_metadata(&mut digest);
    for row in evidence.rows.iter().take(MAX_DIGEST_ROWS) {
        let (row, cells_truncated) = clamp_row(row);
        let mut candidate = digest.clone();
        candidate.rows.push(row);
        candidate.digest_truncated |= cells_truncated;
        if serialized_len(&candidate) <= EVIDENCE_DIGEST_BYTES {
            digest = candidate;
        } else {
            digest.digest_truncated = true;
            break;
        }
    }
    if evidence.rows.len() > digest.rows.len() {
        digest.digest_truncated = true;
    }
    fit_metadata(&mut digest);
    digest
}

pub fn plan_system_prompt(
    catalog: &QueryCatalog,
    scope: &AnalysisScope,
    previous_plan: Option<&AnalysisPlan>,
    refinement: Option<&AnalysisRefinement>,
) -> Result<String, AnalysisAgentError> {
    let catalog = catalog_prompt_value(catalog);
    let scope = serde_json::to_value(scope)?;
    let previous_plan = previous_plan.map(serde_json::to_value).transpose()?;
    let refinement = refinement.map(serde_json::to_value).transpose()?;
    let context = json!({
        "catalog": catalog,
        "scope": scope,
        "prior_plan": previous_plan,
        "refinement": refinement,
        "server_budgets": {
            "max_rows": INTERACTIVE_MAX_ROWS,
            "max_bytes": INTERACTIVE_MAX_BYTES,
        },
    });
    Ok(format!(
        "You create reviewable analysis plans for pChronicle. Never execute SQL; only return an AnalysisPlan proposal. You have no tools and must only propose read-only SQL for later user-confirmed execution. Return a single JSON object with intent_summary, scope_summary, filters, groupings, measures, expected_columns, suggested_view, sql, and warnings. SQL must begin with SELECT, WITH, or EXPLAIN. Use only the catalog and scope below; do not invent schema or evidence.\n\nPlanning context:\n{}",
        serde_json::to_string(&context)?
    ))
}

pub fn interpretation_system_prompt() -> String {
    "AnalysisInterpretation\nInterpret only the supplied evidence digest. Do not add facts not present in that digest. Return one JSON object with the required arrays observations, inferences, limitations, follow_ups, and references. Keep observations separate from inferences; references must identify digest rows or scope coordinates. If query_truncated or digest_truncated is true, limitations must explicitly describe that incomplete coverage."
        .into()
}

async fn request_json_content(
    config: &LlmConfig,
    system: &str,
    messages: Vec<Value>,
) -> Result<String, AnalysisAgentError> {
    let message = match llm::complete(
        config,
        CompletionRequest {
            system: system.into(),
            messages: messages.clone(),
            tools: None,
            response_format: Some(json!({"type": "json_object"})),
            temperature: 0.1,
        },
    )
    .await
    {
        Ok(message) => message,
        Err(error) if error.suggests_response_format_unsupported() => llm::complete(
            config,
            CompletionRequest {
                system: system.into(),
                messages,
                tools: None,
                response_format: None,
                temperature: 0.1,
            },
        )
        .await
        .map_err(completion_error)?,
        Err(error) => return Err(completion_error(error)),
    };
    if message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
    {
        return Err(AnalysisAgentError::new(
            "LLM returned tool calls, but structured analysis agents do not use tools.",
        ));
    }
    message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AnalysisAgentError::new("LLM returned an empty structured response."))
}

fn completion_error(error: llm::CompletionError) -> AnalysisAgentError {
    AnalysisAgentError::new(error.message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanPayload {
    intent_summary: String,
    scope_summary: String,
    filters: Vec<String>,
    groupings: Vec<String>,
    measures: Vec<String>,
    expected_columns: Vec<String>,
    suggested_view: SuggestedView,
    sql: String,
    warnings: Vec<String>,
}

fn parse_plan_content(
    raw: &str,
    plan_id: u64,
    question: &str,
) -> Result<AnalysisPlan, AnalysisAgentError> {
    require_json_object(raw)?;
    let payload: PlanPayload = serde_json::from_str(raw)
        .map_err(|error| AnalysisAgentError::new(format!("Invalid AnalysisPlan JSON: {error}")))?;
    require_text("intent_summary", &payload.intent_summary)?;
    require_text("scope_summary", &payload.scope_summary)?;
    validate_text_array("filters", &payload.filters)?;
    validate_text_array("groupings", &payload.groupings)?;
    validate_text_array("measures", &payload.measures)?;
    validate_text_array("expected_columns", &payload.expected_columns)?;
    validate_text_array("warnings", &payload.warnings)?;
    let sql = payload.sql.trim();
    let sql_upper = sql.to_ascii_uppercase();
    if sql.is_empty()
        || !["SELECT", "WITH", "EXPLAIN"]
            .iter()
            .any(|prefix| sql_upper.starts_with(prefix))
    {
        return Err(AnalysisAgentError::new(
            "AnalysisPlan SQL must start with SELECT, WITH, or EXPLAIN.",
        ));
    }
    Ok(AnalysisPlan {
        id: plan_id,
        question: question.into(),
        intent_summary: payload.intent_summary,
        scope_summary: payload.scope_summary,
        filters: payload.filters,
        groupings: payload.groupings,
        measures: payload.measures,
        expected_columns: payload.expected_columns,
        suggested_view: payload.suggested_view,
        sql: sql.into(),
        warnings: payload.warnings,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterpretationPayload {
    observations: Vec<String>,
    inferences: Vec<String>,
    limitations: Vec<String>,
    follow_ups: Vec<String>,
    references: Vec<EvidenceReferencePayload>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceReferencePayload {
    label: String,
    row_index: Option<usize>,
    dataset: Option<String>,
    file: Option<String>,
    run_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
    turn_id: Option<i64>,
}

fn parse_interpretation_content(raw: &str) -> Result<AnalysisInterpretation, AnalysisAgentError> {
    require_json_object(raw)?;
    let payload: InterpretationPayload = serde_json::from_str(raw).map_err(|error| {
        AnalysisAgentError::new(format!("Invalid AnalysisInterpretation JSON: {error}"))
    })?;
    validate_text_array("observations", &payload.observations)?;
    validate_text_array("inferences", &payload.inferences)?;
    validate_text_array("limitations", &payload.limitations)?;
    validate_text_array("follow_ups", &payload.follow_ups)?;
    let references = payload
        .references
        .into_iter()
        .map(|reference| {
            require_text("references[].label", &reference.label)?;
            Ok(EvidenceReference {
                label: reference.label,
                row_index: reference.row_index,
                dataset: reference.dataset,
                file: reference.file,
                run_id: reference.run_id,
                agent_id: reference.agent_id,
                session_id: reference.session_id,
                root_session_id: reference.root_session_id,
                turn_id: reference.turn_id,
            })
        })
        .collect::<Result<Vec<_>, AnalysisAgentError>>()?;
    Ok(AnalysisInterpretation {
        observations: payload.observations,
        inferences: payload.inferences,
        limitations: payload.limitations,
        follow_ups: payload.follow_ups,
        references,
    })
}

fn validate_interpretation(
    interpretation: AnalysisInterpretation,
    digest: &EvidenceDigest,
) -> Result<AnalysisInterpretation, AnalysisAgentError> {
    if (digest.query_truncated || digest.digest_truncated) && interpretation.limitations.is_empty()
    {
        return Err(AnalysisAgentError::new(
            "AnalysisInterpretation must describe truncated evidence in limitations.",
        ));
    }
    Ok(interpretation)
}

fn require_json_object(raw: &str) -> Result<(), AnalysisAgentError> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err(AnalysisAgentError::new(
            "Expected one raw JSON object without Markdown or surrounding prose.",
        ));
    }
    Ok(())
}

fn require_text(name: &str, value: &str) -> Result<(), AnalysisAgentError> {
    if value.trim().is_empty() {
        return Err(AnalysisAgentError::new(format!(
            "{name} must not be empty."
        )));
    }
    Ok(())
}

fn validate_text_array(name: &str, values: &[String]) -> Result<(), AnalysisAgentError> {
    for value in values {
        require_text(name, value)?;
    }
    Ok(())
}

fn catalog_prompt_value(catalog: &QueryCatalog) -> Value {
    json!({
        "snapshot_id": catalog.snapshot_id,
        "read_only": catalog.read_only,
        "database": catalog.database,
        "storage_path": catalog.storage_path,
        "path_column": catalog.path_column,
        "datasets": catalog.datasets.iter().map(|dataset| json!({
            "name": dataset.name,
            "uri": dataset.uri,
            "ready_sources": dataset.ready_sources,
            "error_sources": dataset.error_sources,
        })).collect::<Vec<_>>(),
        "tables": catalog.tables.iter().map(|table| json!({
            "name": table.name,
            "description": table.description,
            "grain": table.grain,
            "fields": table.fields.iter().map(|field| json!({
                "name": field.name,
                "data_type": field.data_type,
                "description": field.description,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn digest_columns(rows: &[Value]) -> (Vec<String>, bool) {
    let columns = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|row| row.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut truncated = columns.len() > MAX_DIGEST_COLUMNS;
    let columns = columns
        .into_iter()
        .take(MAX_DIGEST_COLUMNS)
        .map(|column| {
            let (column, was_truncated) = clamp_text(&column, CELL_DIGEST_CHARS);
            truncated |= was_truncated;
            column
        })
        .collect();
    (columns, truncated)
}

fn compact_scope(scope: &AnalysisScope) -> (AnalysisScope, bool) {
    let (database, mut truncated) = clamp_text(&scope.database, SCOPE_TEXT_DIGEST_CHARS);
    let (storage_path, storage_truncated) =
        clamp_text(&scope.storage_path, SCOPE_TEXT_DIGEST_CHARS);
    let (snapshot_id, snapshot_truncated) = clamp_text(&scope.snapshot_id, SCOPE_TEXT_DIGEST_CHARS);
    truncated |= storage_truncated || snapshot_truncated || scope.items.len() > MAX_SCOPE_ITEMS;
    let items = scope
        .items
        .iter()
        .take(MAX_SCOPE_ITEMS)
        .map(|item| compact_scope_item(item, &mut truncated))
        .collect();
    (
        AnalysisScope {
            database,
            storage_path,
            snapshot_id,
            items,
        },
        truncated,
    )
}

fn compact_scope_item(item: &AnalysisScopeItem, truncated: &mut bool) -> AnalysisScopeItem {
    match item {
        AnalysisScopeItem::Dataset { name } => {
            let (name, was_truncated) = clamp_text(name, SCOPE_TEXT_DIGEST_CHARS);
            *truncated |= was_truncated;
            AnalysisScopeItem::Dataset { name }
        }
        AnalysisScopeItem::Root {
            dataset,
            file,
            root_session_id,
        } => {
            let (dataset, dataset_truncated) = clamp_text(dataset, SCOPE_TEXT_DIGEST_CHARS);
            let (file, file_truncated) = clamp_text(file, SCOPE_TEXT_DIGEST_CHARS);
            let (root_session_id, root_truncated) =
                clamp_text(root_session_id, SCOPE_TEXT_DIGEST_CHARS);
            *truncated |= dataset_truncated || file_truncated || root_truncated;
            AnalysisScopeItem::Root {
                dataset,
                file,
                root_session_id,
            }
        }
        AnalysisScopeItem::Run { run } => {
            let mut run = run.clone();
            clamp_run_text(&mut run, truncated);
            AnalysisScopeItem::Run { run }
        }
    }
}

fn clamp_run_text(run: &mut crate::model::RunSummary, truncated: &mut bool) {
    for value in [
        &mut run.dataset,
        &mut run.file,
        &mut run.agent_id,
        &mut run.session_id,
        &mut run.path,
        &mut run.status,
    ] {
        let (clamped, was_truncated) = clamp_text(value, SCOPE_TEXT_DIGEST_CHARS);
        *value = clamped;
        *truncated |= was_truncated;
    }
    for value in [
        &mut run.run_id,
        &mut run.model_name,
        &mut run.root_session_id,
    ] {
        if let Some(value) = value {
            let (clamped, was_truncated) = clamp_text(value, SCOPE_TEXT_DIGEST_CHARS);
            *value = clamped;
            *truncated |= was_truncated;
        }
    }
}

fn compact_profiles(profiles: &[ColumnProfile]) -> (Vec<ColumnProfile>, bool) {
    let mut truncated = false;
    let profiles = profiles
        .iter()
        .cloned()
        .map(|mut profile| {
            let (name, was_truncated) = clamp_text(&profile.name, PROFILE_TEXT_DIGEST_CHARS);
            profile.name = name;
            truncated |= was_truncated;
            for value in &mut profile.top_values {
                let (label, was_truncated) = clamp_text(&value.label, PROFILE_TEXT_DIGEST_CHARS);
                value.label = label;
                truncated |= was_truncated;
            }
            let mut type_counts = std::collections::BTreeMap::new();
            for (name, count) in profile.type_counts {
                let (name, was_truncated) = clamp_text(&name, PROFILE_TEXT_DIGEST_CHARS);
                truncated |= was_truncated;
                *type_counts.entry(name).or_insert(0) += count;
            }
            profile.type_counts = type_counts;
            profile
        })
        .collect();
    (profiles, truncated)
}

fn clamp_row(row: &Value) -> (Value, bool) {
    let Some(object) = row.as_object() else {
        return clamp_cell(row);
    };
    let mut truncated = false;
    let mut clamped = serde_json::Map::new();
    for (name, value) in object {
        let (name, name_truncated) = clamp_text(name, CELL_DIGEST_CHARS);
        let (value, value_truncated) = clamp_cell(value);
        truncated |= name_truncated || value_truncated;
        clamped.insert(name, value);
    }
    (Value::Object(clamped), truncated)
}

fn clamp_cell(value: &Value) -> (Value, bool) {
    if let Some(value) = value.as_str() {
        let (value, truncated) = clamp_text(value, CELL_DIGEST_CHARS);
        return (Value::String(value), truncated);
    }
    if serialized_len(value) <= CELL_DIGEST_CHARS {
        return (value.clone(), false);
    }
    let text = serde_json::to_string(value).expect("JSON values serialize");
    let (text, _) = clamp_text(&text, CELL_DIGEST_CHARS);
    (Value::String(text), true)
}

fn clamp_text(value: &str, max_chars: usize) -> (String, bool) {
    if value.chars().count() <= max_chars {
        return (value.into(), false);
    }
    let mut end = value.len();
    let mut seen = 0;
    for (index, _) in value.char_indices() {
        if seen == max_chars {
            end = index;
            break;
        }
        seen += 1;
    }
    (value[..end].into(), true)
}

fn fit_metadata(digest: &mut EvidenceDigest) {
    while serialized_len(digest) > EVIDENCE_DIGEST_BYTES {
        digest.digest_truncated = true;
        if digest.rows.pop().is_some() {
            continue;
        }
        if digest.profiles.pop().is_some() {
            continue;
        }
        if digest.columns.pop().is_some() {
            continue;
        }
        if digest.scope.items.pop().is_some() {
            continue;
        }
        let current_chars = digest.question.chars().count();
        if current_chars > 1 {
            digest.question = clamp_text(&digest.question, current_chars / 2).0;
            continue;
        }
        break;
    }
}

fn serialized_len<T: Serialize>(value: &T) -> usize {
    serde_json::to_vec(value)
        .expect("digest values serialize")
        .len()
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::analysis_session::{AnalysisPlan, AnalysisScope, AnalysisScopeItem, SuggestedView};
    use crate::model::{QueryCatalog, QueryEvidence, QueryFieldSummary, QueryTableSummary};
    use crate::result_profile::profile_rows;

    #[test]
    fn plan_parser_accepts_only_complete_structured_content() {
        let raw = r#"{
          "intent_summary":"Compare outcomes",
          "scope_summary":"current dataset",
          "filters":[],
          "groupings":["status"],
          "measures":["run count"],
          "expected_columns":["status","run_count"],
          "suggested_view":"distribution",
          "sql":"SELECT status, COUNT(*) AS run_count FROM default.runs GROUP BY status",
          "warnings":[]
        }"#;
        let plan = parse_plan_content(raw, 7, "compare outcomes").unwrap();
        assert_eq!(plan.id, 7);
        assert_eq!(plan.question, "compare outcomes");
        assert!(plan.sql.starts_with("SELECT"));
    }

    #[test]
    fn plan_parser_rejects_markdown_wrapped_json() {
        let raw = "```json\n{\"sql\":\"SELECT 1\"}\n```";
        assert!(parse_plan_content(raw, 1, "question").is_err());
    }

    #[test]
    fn interpretation_parser_requires_all_structured_sections() {
        let raw = r#"{
          "observations":["One run is failed."],
          "inferences":["Failures may warrant follow-up."],
          "limitations":["Only the returned rows are available."],
          "follow_ups":["Inspect failed runs."],
          "references":[{
            "label":"failed row",
            "row_index":0,
            "dataset":"default",
            "file":null,
            "run_id":null,
            "agent_id":null,
            "session_id":null,
            "root_session_id":null,
            "turn_id":null
          }]
        }"#;
        let interpretation = parse_interpretation_content(raw).unwrap();
        assert_eq!(interpretation.observations, vec!["One run is failed."]);
        assert_eq!(interpretation.references[0].row_index, Some(0));

        assert!(parse_interpretation_content(
            r#"{"observations":[],"inferences":[],"limitations":[],"follow_ups":[]}"#
        )
        .is_err());
    }

    #[test]
    fn truncated_digest_requires_a_nonempty_limitation() {
        let interpretation = parse_interpretation_content(
            r#"{
              "observations":[],
              "inferences":[],
              "limitations":[],
              "follow_ups":[],
              "references":[]
            }"#,
        )
        .unwrap();
        let digest = build_evidence_digest(&plan(), &scope(), &evidence(Vec::new(), true), &[]);
        assert!(validate_interpretation(interpretation, &digest).is_err());
    }

    #[test]
    fn evidence_digest_is_bounded_and_marks_truncation() {
        let huge = "轨".repeat(80_000);
        let evidence = evidence(vec![json!({"message": huge})], true);
        let digest =
            build_evidence_digest(&plan(), &scope(), &evidence, &profile_rows(&evidence.rows));
        let encoded = serde_json::to_vec(&digest).unwrap();
        assert!(encoded.len() <= EVIDENCE_DIGEST_BYTES);
        assert!(digest.digest_truncated);
        assert!(digest.query_truncated);
    }

    #[test]
    fn plan_prompt_keeps_catalog_descriptions_and_no_execution_rule() {
        let prompt = plan_system_prompt(&catalog(), &scope(), None, None).unwrap();
        assert!(prompt.contains("Never execute SQL; only return an AnalysisPlan proposal."));
        assert!(prompt.contains("default.runs"));
        assert!(prompt.contains("Status of each recorded run"));
        assert!(prompt.contains("Stable identifier for a recorded run"));
        assert!(prompt.contains("one row per recorded run"));
    }

    fn catalog() -> QueryCatalog {
        QueryCatalog {
            snapshot_id: "snapshot-a".into(),
            read_only: true,
            database: "default".into(),
            storage_path: "tmp/test/".into(),
            path_column: "_file_".into(),
            datasets: Vec::new(),
            tables: vec![QueryTableSummary {
                name: "default.runs".into(),
                description: "Recorded agent runs".into(),
                grain: "one row per recorded run".into(),
                fields: vec![
                    QueryFieldSummary {
                        name: "status".into(),
                        data_type: "VARCHAR".into(),
                        description: "Status of each recorded run".into(),
                    },
                    QueryFieldSummary {
                        name: "run_id".into(),
                        data_type: "VARCHAR".into(),
                        description: "Stable identifier for a recorded run".into(),
                    },
                ],
            }],
        }
    }

    fn scope() -> AnalysisScope {
        AnalysisScope {
            database: "default".into(),
            storage_path: "tmp/test/".into(),
            snapshot_id: "snapshot-a".into(),
            items: vec![AnalysisScopeItem::Dataset {
                name: "default".into(),
            }],
        }
    }

    fn plan() -> AnalysisPlan {
        AnalysisPlan {
            id: 1,
            question: "compare outcomes".into(),
            intent_summary: "Compare outcomes".into(),
            scope_summary: "current dataset".into(),
            filters: Vec::new(),
            groupings: vec!["status".into()],
            measures: vec!["run count".into()],
            expected_columns: vec!["status".into(), "run_count".into()],
            suggested_view: SuggestedView::Distribution,
            sql: "SELECT status, COUNT(*) AS run_count FROM default.runs GROUP BY status".into(),
            warnings: Vec::new(),
        }
    }

    fn evidence(rows: Vec<Value>, truncated: bool) -> QueryEvidence {
        QueryEvidence {
            returned_rows: rows.len(),
            rows,
            truncated,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}
