#![allow(dead_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::analysis_session::{
    AnalysisInterpretation, AnalysisPlan, AnalysisScope, AnalysisScopeItem, AnalysisSpec,
    EvidenceReference, SuggestedView,
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

#[derive(Clone)]
pub struct PlanRequest {
    pub config: LlmConfig,
    pub catalog: QueryCatalog,
    pub scope: AnalysisScope,
    pub question: String,
    pub plan_id: u64,
    pub previous_plan: Option<AnalysisPlan>,
    pub previous_spec: Option<AnalysisSpec>,
    pub compile_error: Option<String>,
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

pub async fn generate_spec(request: PlanRequest) -> Result<AnalysisSpec, AnalysisAgentError> {
    let system = spec_system_prompt(
        &request.catalog,
        &request.scope,
        request.previous_spec.as_ref(),
        request.compile_error.as_deref(),
        request.refinement.as_ref(),
    )?;
    let messages = vec![json!({
        "role": "user",
        "content": serde_json::to_string(&json!({"question": request.question}))?,
    })];
    let content = request_json_content(&request.config, &system, messages).await?;
    match parse_spec_content(&content) {
        Ok(spec) => Ok(spec),
        Err(first_error) => {
            let repair_messages = vec![
                json!({"role":"user", "content": content}),
                json!({
                    "role":"user",
                    "content": format!(
                        "Return one corrected AnalysisSpec JSON object only. Validation error: {}",
                        first_error.message
                    ),
                }),
            ];
            let repaired = request_json_content(&request.config, &system, repair_messages).await?;
            parse_spec_content(&repaired)
        }
    }
}

pub async fn generate_plan(request: PlanRequest) -> Result<AnalysisPlan, AnalysisAgentError> {
    let spec = generate_spec(request).await?;
    Err(AnalysisAgentError::new(format!(
        "Legacy SQL plan generation is unavailable; received analysis intent {}",
        spec.intent
    )))
}

pub async fn interpret(
    request: InterpretationRequest,
) -> Result<AnalysisInterpretation, AnalysisAgentError> {
    let system = interpretation_system_prompt();
    let messages = vec![json!({
        "role": "user",
        "content": serde_json::to_string(&evidence_digest_prompt_value(&request.digest))?,
    })];
    let content = request_json_content(&request.config, &system, messages).await?;
    match parse_interpretation_content(&content)
        .and_then(|interpretation| prepare_interpretation(interpretation, &request.digest))
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
                .and_then(|interpretation| prepare_interpretation(interpretation, &request.digest))
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

pub fn ensure_truncation_limitation(
    interpretation: &mut AnalysisInterpretation,
    digest: &EvidenceDigest,
) {
    if !(digest.query_truncated || digest.digest_truncated)
        || interpretation
            .limitations
            .iter()
            .any(|limitation| describes_incomplete_coverage(limitation))
    {
        return;
    }
    interpretation.limitations.insert(
        0,
        "This summary covers only the limited result rows sent to the model because the query result or analysis input was truncated."
            .into(),
    );
}

fn describes_incomplete_coverage(limitation: &str) -> bool {
    let limitation = limitation.to_ascii_lowercase();
    limitation.contains("truncat")
        || limitation.contains("incomplete coverage")
        || limitation.contains("partial coverage")
}

pub fn spec_system_prompt(
    catalog: &QueryCatalog,
    scope: &AnalysisScope,
    previous_spec: Option<&AnalysisSpec>,
    compile_error: Option<&str>,
    refinement: Option<&AnalysisRefinement>,
) -> Result<String, AnalysisAgentError> {
    let catalog = catalog_prompt_value(catalog);
    let scope = scope_prompt_value(scope);
    let previous_spec = previous_spec.map(serde_json::to_value).transpose()?;
    let refinement = refinement.map(serde_json::to_value).transpose()?;
    let context = json!({
        "catalog": catalog,
        "scope": scope,
        "prior_spec": previous_spec,
        "compile_error": compile_error,
        "refinement": refinement,
        "allowed_intents": ["distribution", "compare", "rank_outlier", "composition", "drilldown"],
        "allowed_grains": ["run", "step", "tool_call"],
        "allowed_measures": [
            "row_count",
            "step_count_per_run",
            "tool_call_count_per_run",
            "tool_call_count",
            "step_latency_ms",
            "step_ttft_ms",
            "tool_duration_ms"
        ],
        "server_budgets": {
            "max_rows": INTERACTIVE_MAX_ROWS,
            "max_bytes": INTERACTIVE_MAX_BYTES,
        },
    });
    Ok(format!(
        "You write an AnalysisSpec for pChronicle. Never write SQL. Return one JSON object with intent, grain, measure, optional dimension, optional filters, optional ranking, and output. intent must be one of distribution, compare, rank_outlier, composition, drilldown. grain must be run, step, or tool_call. ranking, if present, must be an object {{\"kind\":\"top_n\"|\"bottom_n\"|\"outlier\",\"n\":20}}; omit ranking or use null when it does not apply. Never emit ranking as an array. Use only registered measures and live catalog columns. Do not use status, tokens, or *_json fields. Causal questions are not intents. If compile_error is present, revise the spec to address it.\n\nPlanning context:\n{}",
        serde_json::to_string(&context)?
    ))
}

pub fn plan_system_prompt(
    catalog: &QueryCatalog,
    scope: &AnalysisScope,
    previous_plan: Option<&AnalysisPlan>,
    refinement: Option<&AnalysisRefinement>,
) -> Result<String, AnalysisAgentError> {
    spec_system_prompt(
        catalog,
        scope,
        None,
        previous_plan.and_then(|_| None),
        refinement,
    )
}

pub fn interpretation_system_prompt() -> String {
    "AnalysisInterpretation\nInterpret only the supplied evidence digest and AnalysisSpec. Do not add facts not present in that digest. Do not judge task success or failure unless those values appear as result columns. Return one JSON object with the required arrays observations, inferences, limitations, follow_ups, and references. Keep observations separate from inferences; references must identify digest rows or scope coordinates. If query_truncated or digest_truncated is true, limitations must explicitly describe that incomplete coverage."
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
    if !has_allowed_sql_keyword(sql) || !has_at_most_one_sql_statement(sql) {
        return Err(AnalysisAgentError::new(
            "AnalysisPlan SQL must contain one SELECT, WITH, or EXPLAIN statement.",
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

fn parse_spec_content(raw: &str) -> Result<AnalysisSpec, AnalysisAgentError> {
    require_json_object(raw)?;
    let spec: AnalysisSpec = serde_json::from_str(raw)
        .map_err(|error| AnalysisAgentError::new(format!("Invalid analysis plan JSON: {error}")))?;
    require_text("intent", &spec.intent)?;
    require_text("grain", &spec.grain)?;
    require_text("measure", &spec.measure)?;
    require_text("output", &spec.output)?;
    if spec.intent.contains(';') || spec.measure.contains("SELECT") {
        return Err(AnalysisAgentError::new(
            "Analysis plan fields cannot contain SQL.",
        ));
    }
    Ok(spec)
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
            "The result summary must describe incomplete data in limitations.",
        ));
    }
    for reference in &interpretation.references {
        validate_reference(reference, digest)?;
    }
    Ok(interpretation)
}

fn prepare_interpretation(
    mut interpretation: AnalysisInterpretation,
    digest: &EvidenceDigest,
) -> Result<AnalysisInterpretation, AnalysisAgentError> {
    ensure_truncation_limitation(&mut interpretation, digest);
    validate_interpretation(interpretation, digest)
}

fn validate_reference(
    reference: &EvidenceReference,
    digest: &EvidenceDigest,
) -> Result<(), AnalysisAgentError> {
    if let Some(row_index) = reference.row_index {
        let row = digest.rows.get(row_index).ok_or_else(|| {
            AnalysisAgentError::new("AnalysisInterpretation reference row_index is out of range.")
        })?;
        if !reference_matches_row(reference, row) {
            return Err(AnalysisAgentError::new(
                "AnalysisInterpretation reference coordinates do not match its digest row.",
            ));
        }
        return Ok(());
    }
    if !reference_has_coordinates(reference) {
        return Ok(());
    }
    if digest
        .rows
        .iter()
        .any(|row| reference_matches_row(reference, row))
    {
        return Ok(());
    }
    if reference.turn_id.is_none()
        && digest
            .scope
            .items
            .iter()
            .any(|item| reference_matches_scope_item(reference, item))
    {
        return Ok(());
    }
    Err(AnalysisAgentError::new(
        "Result summary references do not match the supplied result rows.",
    ))
}

fn reference_has_coordinates(reference: &EvidenceReference) -> bool {
    reference.dataset.is_some()
        || reference.file.is_some()
        || reference.run_id.is_some()
        || reference.agent_id.is_some()
        || reference.session_id.is_some()
        || reference.root_session_id.is_some()
        || reference.turn_id.is_some()
}

fn reference_matches_row(reference: &EvidenceReference, row: &Value) -> bool {
    matches_row_text(row, "dataset", reference.dataset.as_deref())
        && matches_row_text(row, "file", reference.file.as_deref())
        && matches_row_text(row, "run_id", reference.run_id.as_deref())
        && matches_row_text(row, "agent_id", reference.agent_id.as_deref())
        && matches_row_text(row, "session_id", reference.session_id.as_deref())
        && matches_row_text(row, "root_session_id", reference.root_session_id.as_deref())
        && matches_row_turn(row, reference.turn_id)
}

fn matches_row_text(row: &Value, name: &str, expected: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        row.get(name)
            .or_else(|| (name == "file").then(|| row.get("_file_")).flatten())
            .and_then(Value::as_str)
            == Some(expected)
    })
}

fn matches_row_turn(row: &Value, expected: Option<i64>) -> bool {
    expected.is_none_or(|expected| row.get("turn_id").and_then(Value::as_i64) == Some(expected))
}

fn reference_matches_scope_item(reference: &EvidenceReference, item: &AnalysisScopeItem) -> bool {
    match item {
        AnalysisScopeItem::Dataset { name } => {
            matches_optional_text(reference.dataset.as_deref(), name)
                && reference.file.is_none()
                && reference.run_id.is_none()
                && reference.agent_id.is_none()
                && reference.session_id.is_none()
                && reference.root_session_id.is_none()
        }
        AnalysisScopeItem::Root {
            dataset,
            file,
            root_session_id,
        } => {
            matches_optional_text(reference.dataset.as_deref(), dataset)
                && matches_optional_text(reference.file.as_deref(), file)
                && matches_optional_text(reference.root_session_id.as_deref(), root_session_id)
                && reference.run_id.is_none()
                && reference.agent_id.is_none()
                && reference.session_id.is_none()
        }
        AnalysisScopeItem::Run { run } => {
            matches_optional_text(reference.dataset.as_deref(), &run.dataset)
                && matches_optional_text(reference.file.as_deref(), &run.file)
                && matches_optional_optional_text(
                    reference.run_id.as_deref(),
                    run.run_id.as_deref(),
                )
                && matches_optional_text(reference.agent_id.as_deref(), &run.agent_id)
                && matches_optional_text(reference.session_id.as_deref(), &run.session_id)
                && matches_optional_optional_text(
                    reference.root_session_id.as_deref(),
                    run.root_session_id.as_deref(),
                )
        }
    }
}

fn matches_optional_text(expected: Option<&str>, actual: &str) -> bool {
    expected.is_none_or(|expected| expected == actual)
}

fn matches_optional_optional_text(expected: Option<&str>, actual: Option<&str>) -> bool {
    expected.is_none_or(|expected| actual == Some(expected))
}

fn has_allowed_sql_keyword(sql: &str) -> bool {
    let sql = sql.trim_start().to_ascii_uppercase();
    ["SELECT", "WITH", "EXPLAIN"].iter().any(|keyword| {
        sql.strip_prefix(keyword).is_some_and(|rest| {
            rest.chars().next().is_none_or(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '$'))
            })
        })
    })
}

fn has_at_most_one_sql_statement(sql: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Normal,
        SingleQuoted,
        DoubleQuoted,
        BacktickQuoted,
        LineComment,
        BlockComment,
    }

    let mut characters = sql.chars().peekable();
    let mut state = State::Normal;
    let mut terminated = false;
    while let Some(character) = characters.next() {
        match state {
            State::Normal if terminated => match character {
                whitespace if whitespace.is_whitespace() => {}
                '-' if characters.next_if_eq(&'-').is_some() => state = State::LineComment,
                '/' if characters.next_if_eq(&'*').is_some() => state = State::BlockComment,
                _ => return false,
            },
            State::Normal => match character {
                '\'' => state = State::SingleQuoted,
                '"' => state = State::DoubleQuoted,
                '`' => state = State::BacktickQuoted,
                '-' if characters.next_if_eq(&'-').is_some() => state = State::LineComment,
                '/' if characters.next_if_eq(&'*').is_some() => state = State::BlockComment,
                ';' => terminated = true,
                _ => {}
            },
            State::SingleQuoted => {
                if character == '\\' {
                    let _ = characters.next();
                } else if character == '\'' {
                    if characters.next_if_eq(&'\'').is_none() {
                        state = State::Normal;
                    }
                }
            }
            State::DoubleQuoted => {
                if character == '"' && characters.next_if_eq(&'"').is_none() {
                    state = State::Normal;
                }
            }
            State::BacktickQuoted => {
                if character == '`' && characters.next_if_eq(&'`').is_none() {
                    state = State::Normal;
                }
            }
            State::LineComment if character == '\n' || character == '\r' => state = State::Normal,
            State::LineComment => {}
            State::BlockComment if character == '*' && characters.next_if_eq(&'/').is_some() => {
                state = State::Normal;
            }
            State::BlockComment => {}
        }
    }
    matches!(state, State::Normal | State::LineComment)
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
        "tables": crate::model::queryable_tables(catalog).iter().map(|table| json!({
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

fn scope_prompt_value(scope: &AnalysisScope) -> Value {
    json!({
        "database": scope.database,
        "items": scope.items.iter().map(scope_item_prompt_value).collect::<Vec<_>>(),
    })
}

fn scope_item_prompt_value(item: &AnalysisScopeItem) -> Value {
    match item {
        AnalysisScopeItem::Dataset { name } => json!({
            "kind": "dataset",
            "name": name,
        }),
        AnalysisScopeItem::Root {
            dataset,
            file,
            root_session_id,
        } => json!({
            "kind": "root",
            "dataset": dataset,
            "file": file,
            "root_session_id": root_session_id,
        }),
        AnalysisScopeItem::Run { run } => json!({
            "kind": "run",
            "run": {
                "dataset": run.dataset,
                "file": run.file,
                "run_id": run.run_id,
                "agent_id": run.agent_id,
                "session_id": run.session_id,
                "root_session_id": run.root_session_id,
            },
        }),
    }
}

fn evidence_digest_prompt_value(digest: &EvidenceDigest) -> Value {
    json!({
        "question": digest.question,
        "scope": scope_prompt_value(&digest.scope),
        "sql": digest.sql,
        "columns": digest.columns,
        "profiles": digest.profiles,
        "rows": digest.rows,
        "returned_rows": digest.returned_rows,
        "query_truncated": digest.query_truncated,
        "max_rows": digest.max_rows,
        "max_bytes": digest.max_bytes,
        "digest_truncated": digest.digest_truncated,
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
    use serde_json::{Value, json};

    use super::*;
    use crate::analysis_session::{AnalysisPlan, AnalysisScope, AnalysisScopeItem, SuggestedView};
    use crate::model::{
        QueryCatalog, QueryDatasetSummary, QueryEvidence, QueryFieldSummary, QueryTableSummary,
        RunSummary,
    };
    use crate::result_profile::profile_rows;

    #[test]
    fn spec_parser_accepts_registered_analysis_spec() {
        let raw = r#"{
          "intent":"composition",
          "grain":"tool_call",
          "measure":"tool_call_count",
          "dimension":"function_name",
          "output":"table"
        }"#;
        let spec = parse_spec_content(raw).unwrap();
        assert_eq!(spec.intent, "composition");
        assert_eq!(spec.measure, "tool_call_count");
    }

    #[test]
    fn spec_parser_treats_empty_ranking_array_as_absent() {
        let raw = r#"{
          "intent":"compare",
          "grain":"run",
          "measure":"step_count_per_run",
          "dimension":"agent_model_name",
          "ranking":[],
          "output":"comparison"
        }"#;
        let spec = parse_spec_content(raw).expect("empty ranking array should not fail parse");
        assert_eq!(spec.ranking, None);
        assert_eq!(spec.intent, "compare");
    }

    #[test]
    fn spec_parser_accepts_tuple_ranking() {
        let raw = r#"{
          "intent":"rank_outlier",
          "grain":"step",
          "measure":"step_latency_ms",
          "ranking":["top_n", 10],
          "output":"table"
        }"#;
        let spec = parse_spec_content(raw).unwrap();
        assert_eq!(
            spec.ranking,
            Some(crate::analysis_session::Ranking {
                kind: "top_n".into(),
                n: Some(10),
            })
        );
    }

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
    fn plan_parser_rejects_keyword_prefixes_and_multiple_sql_statements() {
        assert!(parse_plan_content(&plan_payload("SELECTED 1"), 1, "question").is_err());
        assert!(
            parse_plan_content(
                &plan_payload("SELECT 1; DELETE FROM default.runs"),
                1,
                "question"
            )
            .is_err()
        );
        assert!(
            parse_plan_content(&plan_payload("SELECT 1; 'not a comment'"), 1, "question").is_err()
        );
    }

    #[test]
    fn plan_parser_allows_quoted_and_commented_semicolons_with_one_terminator() {
        let sql = "SELECT ';' AS value, \"semi;identifier\" FROM default.runs /* ; */; -- ;\n";
        assert!(parse_plan_content(&plan_payload(sql), 1, "question").is_ok());
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

        assert!(
            parse_interpretation_content(
                r#"{"observations":[],"inferences":[],"limitations":[],"follow_ups":[]}"#
            )
            .is_err()
        );
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
    fn truncated_digest_gets_a_deterministic_limitation_when_the_model_omits_it() {
        let mut interpretation = AnalysisInterpretation {
            limitations: vec!["Latency was not selected by this query.".into()],
            ..AnalysisInterpretation::default()
        };
        let digest = build_evidence_digest(&plan(), &scope(), &evidence(Vec::new(), true), &[]);

        ensure_truncation_limitation(&mut interpretation, &digest);

        assert_eq!(
            interpretation.limitations,
            vec![
                "This summary covers only the limited result rows sent to the model because the query result or analysis input was truncated.",
                "Latency was not selected by this query.",
            ]
        );
    }

    #[test]
    fn interpretation_references_reject_out_of_range_and_fabricated_coordinates() {
        let digest = digest_with_rows(vec![json!({
            "dataset":"default",
            "file":"source.json",
            "run_id":"run-1",
            "agent_id":"agent-1",
            "session_id":"session-1",
            "root_session_id":"root-1",
            "turn_id":4
        })]);
        let out_of_range = interpretation_with_reference(EvidenceReference {
            label: "outside rows".into(),
            row_index: Some(1),
            dataset: Some("default".into()),
            file: None,
            run_id: None,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: None,
        });
        assert!(validate_interpretation(out_of_range, &digest).is_err());

        let fabricated = interpretation_with_reference(EvidenceReference {
            label: "fabricated run".into(),
            row_index: Some(0),
            dataset: Some("default".into()),
            file: None,
            run_id: Some("other-run".into()),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: None,
        });
        assert!(validate_interpretation(fabricated, &digest).is_err());

        let ungrounded = interpretation_with_reference(EvidenceReference {
            label: "not in scope or rows".into(),
            row_index: None,
            dataset: Some("other".into()),
            file: None,
            run_id: None,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: None,
        });
        assert!(validate_interpretation(ungrounded, &digest).is_err());
    }

    #[test]
    fn interpretation_references_accept_grounded_rows_scope_and_labels() {
        let digest = digest_with_rows(vec![json!({
            "dataset":"default",
            "file":"source.json",
            "run_id":"run-1",
            "turn_id":4
        })]);
        let row_reference = interpretation_with_reference(EvidenceReference {
            label: "row match".into(),
            row_index: Some(0),
            dataset: Some("default".into()),
            file: Some("source.json".into()),
            run_id: Some("run-1".into()),
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: Some(4),
        });
        assert!(validate_interpretation(row_reference, &digest).is_ok());

        let scope_reference = interpretation_with_reference(EvidenceReference {
            label: "scope match".into(),
            row_index: None,
            dataset: Some("default".into()),
            file: None,
            run_id: None,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: None,
        });
        assert!(validate_interpretation(scope_reference, &digest).is_ok());

        let label_only = interpretation_with_reference(EvidenceReference {
            label: "plain label".into(),
            row_index: None,
            dataset: None,
            file: None,
            run_id: None,
            agent_id: None,
            session_id: None,
            root_session_id: None,
            turn_id: None,
        });
        assert!(validate_interpretation(label_only, &digest).is_ok());
    }

    #[test]
    fn interpretation_reference_accepts_the_result_explorer_file_coordinate() {
        let digest = digest_with_rows(vec![json!({
            "dataset":"default",
            "_file_":"source.json",
            "run_id":"run-1",
            "agent_id":"agent-1",
            "session_id":"session-1",
            "root_session_id":"root-1"
        })]);
        let reference = interpretation_with_reference(EvidenceReference {
            label: "grounded run".into(),
            row_index: Some(0),
            dataset: Some("default".into()),
            file: Some("source.json".into()),
            run_id: Some("run-1".into()),
            agent_id: Some("agent-1".into()),
            session_id: Some("session-1".into()),
            root_session_id: Some("root-1".into()),
            turn_id: None,
        });

        assert!(validate_interpretation(reference, &digest).is_ok());
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
        assert!(prompt.contains("Never write SQL."));
        assert!(prompt.contains("default.runs"));
        assert!(prompt.contains("Status of each recorded run"));
        assert!(prompt.contains("Stable identifier for a recorded run"));
        assert!(prompt.contains("one row per recorded run"));
    }

    #[test]
    fn plan_prompt_sends_only_approved_catalog_and_scope_context() {
        let mut catalog = catalog();
        catalog.datasets = vec![QueryDatasetSummary {
            name: "private-dataset".into(),
            uri: "s3://secret-bucket/?token=private".into(),
            ready_sources: 17,
            error_sources: 4,
        }];
        let scope = private_scope();

        let prompt = plan_system_prompt(&catalog, &scope, None, None).unwrap();
        let (_, context) = prompt.split_once("Planning context:\n").unwrap();
        let context: Value = serde_json::from_str(context).unwrap();

        assert_eq!(
            context,
            json!({
                "catalog": {
                    "tables": [{
                        "name": "default.runs",
                        "description": "Recorded agent runs",
                        "grain": "one row per recorded run",
                        "fields": [
                            {
                                "name": "status",
                                "data_type": "VARCHAR",
                                "description": "Status of each recorded run",
                            },
                            {
                                "name": "run_id",
                                "data_type": "VARCHAR",
                                "description": "Stable identifier for a recorded run",
                            },
                        ],
                    }],
                },
                "scope": {
                    "database": "default",
                    "items": [
                        {
                            "kind": "dataset",
                            "name": "default",
                        },
                        {
                            "kind": "root",
                            "dataset": "default",
                            "file": "source.json",
                            "root_session_id": "root-1",
                        },
                        {
                            "kind": "run",
                            "run": {
                                "dataset": "default",
                                "file": "source.json",
                                "run_id": "run-1",
                                "agent_id": "agent-1",
                                "session_id": "session-1",
                                "root_session_id": "root-1",
                            },
                        },
                    ],
                },
                "prior_spec": null,
                "compile_error": null,
                "refinement": null,
                "allowed_intents": ["distribution", "compare", "rank_outlier", "composition", "drilldown"],
                "allowed_grains": ["run", "step", "tool_call"],
                "allowed_measures": [
                    "row_count",
                    "step_count_per_run",
                    "tool_call_count_per_run",
                    "tool_call_count",
                    "step_latency_ms",
                    "step_ttft_ms",
                    "tool_duration_ms"
                ],
                "server_budgets": {
                    "max_rows": 100,
                    "max_bytes": 4 * 1024 * 1024,
                },
            })
        );
    }

    #[test]
    fn interpretation_digest_sends_only_approved_scope_context() {
        let mut digest = digest_with_rows(vec![json!({"status": "failed"})]);
        digest.scope = private_scope();
        digest.columns = vec!["status".into()];

        assert_eq!(
            evidence_digest_prompt_value(&digest),
            json!({
                "question": "compare outcomes",
                "scope": {
                    "database": "default",
                    "items": [
                        {
                            "kind": "dataset",
                            "name": "default",
                        },
                        {
                            "kind": "root",
                            "dataset": "default",
                            "file": "source.json",
                            "root_session_id": "root-1",
                        },
                        {
                            "kind": "run",
                            "run": {
                                "dataset": "default",
                                "file": "source.json",
                                "run_id": "run-1",
                                "agent_id": "agent-1",
                                "session_id": "session-1",
                                "root_session_id": "root-1",
                            },
                        },
                    ],
                },
                "sql": "SELECT 1",
                "columns": ["status"],
                "profiles": [],
                "rows": [{"status": "failed"}],
                "returned_rows": 1,
                "query_truncated": false,
                "max_rows": 100,
                "max_bytes": 4 * 1024 * 1024,
                "digest_truncated": false,
            })
        );
    }

    fn private_scope() -> AnalysisScope {
        AnalysisScope {
            database: "default".into(),
            storage_path: "/Users/alice/private-trajectories".into(),
            snapshot_id: "snapshot-secret".into(),
            items: vec![
                AnalysisScopeItem::Dataset {
                    name: "default".into(),
                },
                AnalysisScopeItem::Root {
                    dataset: "default".into(),
                    file: "source.json".into(),
                    root_session_id: "root-1".into(),
                },
                AnalysisScopeItem::Run {
                    run: RunSummary {
                        dataset: "default".into(),
                        file: "source.json".into(),
                        run_id: Some("run-1".into()),
                        agent_id: "agent-1".into(),
                        model_name: Some("private-model".into()),
                        session_id: "session-1".into(),
                        root_session_id: Some("root-1".into()),
                        path: "private/internal/path".into(),
                        row_count: 99,
                        duplicate_event_ids: 3,
                        status: "private-status".into(),
                    },
                },
            ],
        }
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

    fn plan_payload(sql: &str) -> String {
        json!({
            "intent_summary": "Compare outcomes",
            "scope_summary": "current dataset",
            "filters": [],
            "groupings": ["status"],
            "measures": ["run count"],
            "expected_columns": ["status", "run_count"],
            "suggested_view": "distribution",
            "sql": sql,
            "warnings": [],
        })
        .to_string()
    }

    fn digest_with_rows(rows: Vec<Value>) -> EvidenceDigest {
        EvidenceDigest {
            question: "compare outcomes".into(),
            scope: scope(),
            sql: "SELECT 1".into(),
            columns: Vec::new(),
            profiles: Vec::new(),
            returned_rows: rows.len(),
            rows,
            query_truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
            digest_truncated: false,
        }
    }

    fn interpretation_with_reference(reference: EvidenceReference) -> AnalysisInterpretation {
        AnalysisInterpretation {
            observations: Vec::new(),
            inferences: Vec::new(),
            limitations: Vec::new(),
            follow_ups: Vec::new(),
            references: vec![reference],
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
