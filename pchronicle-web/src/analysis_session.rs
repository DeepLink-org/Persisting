#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{QueryCatalog, QueryEvidence, RunSummary};
use crate::result_profile::ColumnProfile;

pub const MAX_ANALYSIS_SESSIONS: usize = 20;
pub const MAX_SESSION_BYTES: usize = 256 * 1024;
pub const STORAGE_PREFIX: &str = "pchronicle_analysis:";

pub type AnalysisOperationId = u64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnalysisScopeItem {
    Dataset {
        name: String,
    },
    Root {
        dataset: String,
        file: String,
        root_session_id: String,
    },
    Run {
        run: RunSummary,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisScope {
    pub database: String,
    pub storage_path: String,
    pub snapshot_id: String,
    pub items: Vec<AnalysisScopeItem>,
}

impl AnalysisScope {
    pub fn from_catalog(catalog: &QueryCatalog) -> Self {
        Self {
            database: catalog.database.clone(),
            storage_path: catalog.storage_path.clone(),
            snapshot_id: catalog.snapshot_id.clone(),
            items: vec![AnalysisScopeItem::Dataset {
                name: catalog.database.clone(),
            }],
        }
    }

    pub fn from_root(
        catalog: &QueryCatalog,
        dataset: impl Into<String>,
        file: impl Into<String>,
        root_session_id: impl Into<String>,
    ) -> Self {
        Self {
            database: catalog.database.clone(),
            storage_path: catalog.storage_path.clone(),
            snapshot_id: catalog.snapshot_id.clone(),
            items: vec![AnalysisScopeItem::Root {
                dataset: dataset.into(),
                file: file.into(),
                root_session_id: root_session_id.into(),
            }],
        }
    }

    pub fn from_run(catalog: &QueryCatalog, run: RunSummary) -> Self {
        Self::from_runs(catalog, vec![run])
    }

    pub fn from_runs(catalog: &QueryCatalog, runs: Vec<RunSummary>) -> Self {
        Self {
            database: catalog.database.clone(),
            storage_path: catalog.storage_path.clone(),
            snapshot_id: catalog.snapshot_id.clone(),
            items: runs
                .into_iter()
                .map(|run| AnalysisScopeItem::Run { run })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionState {
    Draft,
    GeneratingPlan,
    PlanReady,
    Executing,
    Interpreting,
    Complete,
    PlanError,
    QueryError,
    InterpretationError,
    Stale,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnalysisEffect {
    ExecuteSql {
        revision_id: u64,
        operation_id: AnalysisOperationId,
        sql: String,
    },
    Interpret {
        revision_id: u64,
        operation_id: AnalysisOperationId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedView {
    Table,
    Distribution,
    Trend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisPlan {
    pub id: u64,
    pub question: String,
    pub intent_summary: String,
    pub scope_summary: String,
    pub filters: Vec<String>,
    pub groupings: Vec<String>,
    pub measures: Vec<String>,
    pub expected_columns: Vec<String>,
    pub suggested_view: SuggestedView,
    pub sql: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReference {
    pub label: String,
    pub row_index: Option<usize>,
    pub dataset: Option<String>,
    pub file: Option<String>,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub turn_id: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnalysisInterpretation {
    pub observations: Vec<String>,
    pub inferences: Vec<String>,
    pub limitations: Vec<String>,
    pub follow_ups: Vec<String>,
    pub references: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionSummary {
    pub returned_rows: usize,
    pub truncated: bool,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub executed_at_ms: u64,
    pub profiles: Vec<ColumnProfile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisRevision {
    pub id: u64,
    pub question: String,
    pub scope: AnalysisScope,
    pub state: RevisionState,
    pub plan: Option<AnalysisPlan>,
    pub manually_edited: bool,
    pub execution: Option<ExecutionSummary>,
    pub interpretation: Option<AnalysisInterpretation>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub needs_rerun: bool,
    #[serde(skip)]
    pub evidence: Option<QueryEvidence>,
    #[serde(skip)]
    pub pending_effect: Option<AnalysisEffect>,
    #[serde(skip)]
    pub active_operation_id: Option<AnalysisOperationId>,
    #[serde(skip)]
    next_operation_id: AnalysisOperationId,
}

impl AnalysisRevision {
    pub fn draft(id: u64, question: impl Into<String>, scope: AnalysisScope) -> Self {
        let now = now_millis();
        Self {
            id,
            question: question.into(),
            scope,
            state: RevisionState::Draft,
            plan: None,
            manually_edited: false,
            execution: None,
            interpretation: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            needs_rerun: false,
            evidence: None,
            pending_effect: None,
            active_operation_id: None,
            next_operation_id: 0,
        }
    }

    pub fn begin_plan_generation(&mut self) -> Result<AnalysisOperationId, String> {
        match self.state {
            RevisionState::Draft | RevisionState::PlanError | RevisionState::Stale => {
                self.state = RevisionState::GeneratingPlan;
                self.error = None;
                self.pending_effect = None;
                self.touch();
                Ok(self.begin_operation())
            }
            _ => Err(
                "A plan can only be generated from a draft, plan error, or stale revision.".into(),
            ),
        }
    }

    pub fn finish_plan(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        plan: AnalysisPlan,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != RevisionState::GeneratingPlan {
            return Err("This revision is not waiting for a generated plan.".into());
        }
        self.plan = Some(plan);
        self.state = RevisionState::PlanReady;
        self.error = None;
        self.pending_effect = None;
        self.active_operation_id = None;
        self.touch();
        Ok(None)
    }

    pub fn confirm_execution(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            RevisionState::PlanReady | RevisionState::QueryError
        ) {
            return Err("Review a ready plan before running this analysis.".into());
        }
        let Some(plan) = self.plan.as_ref() else {
            return Err("A plan is required before running this analysis.".into());
        };
        let sql = plan.sql.clone();
        let operation_id = self.begin_operation();
        self.state = RevisionState::Executing;
        self.error = None;
        self.pending_effect = Some(AnalysisEffect::ExecuteSql {
            revision_id: self.id,
            operation_id,
            sql,
        });
        self.touch();
        Ok(())
    }

    pub fn finish_query(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        evidence: QueryEvidence,
        profiles: Vec<ColumnProfile>,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != RevisionState::Executing {
            return Err("This revision is not waiting for query results.".into());
        }
        let has_rows = !evidence.rows.is_empty();
        self.execution = Some(ExecutionSummary {
            returned_rows: evidence.returned_rows,
            truncated: evidence.truncated,
            max_rows: evidence.max_rows,
            max_bytes: evidence.max_bytes,
            executed_at_ms: now_millis(),
            profiles,
        });
        self.evidence = Some(evidence);
        self.error = None;
        self.needs_rerun = false;
        let effect = has_rows.then(|| AnalysisEffect::Interpret {
            revision_id: self.id,
            operation_id: self.begin_operation(),
        });
        self.pending_effect = effect.clone();
        self.state = if effect.is_some() {
            RevisionState::Interpreting
        } else {
            self.active_operation_id = None;
            RevisionState::Complete
        };
        self.touch();
        Ok(effect)
    }

    pub fn finish_interpretation(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        interpretation: AnalysisInterpretation,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != RevisionState::Interpreting {
            return Err("This revision is not waiting for an interpretation.".into());
        }
        self.interpretation = Some(interpretation);
        self.state = RevisionState::Complete;
        self.error = None;
        self.pending_effect = None;
        self.active_operation_id = None;
        self.touch();
        Ok(None)
    }

    pub fn fail_plan(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        error: impl Into<String>,
    ) -> Result<Option<AnalysisEffect>, String> {
        self.fail(
            revision_id,
            operation_id,
            RevisionState::GeneratingPlan,
            RevisionState::PlanError,
            error,
        )
    }

    pub fn fail_query(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        error: impl Into<String>,
    ) -> Result<Option<AnalysisEffect>, String> {
        self.fail(
            revision_id,
            operation_id,
            RevisionState::Executing,
            RevisionState::QueryError,
            error,
        )
    }

    pub fn fail_interpretation(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        error: impl Into<String>,
    ) -> Result<Option<AnalysisEffect>, String> {
        self.fail(
            revision_id,
            operation_id,
            RevisionState::Interpreting,
            RevisionState::InterpretationError,
            error,
        )
    }

    pub fn take_pending_effect(&mut self) -> Option<AnalysisEffect> {
        self.pending_effect.take()
    }

    pub fn retry_interpretation(&mut self) -> Result<AnalysisEffect, String> {
        if self.state != RevisionState::InterpretationError || self.evidence.is_none() {
            return Err("Interpretation can only be retried after an interpretation error.".into());
        }
        let operation_id = self.begin_operation();
        self.state = RevisionState::Interpreting;
        self.error = None;
        let effect = AnalysisEffect::Interpret {
            revision_id: self.id,
            operation_id,
        };
        self.pending_effect = Some(effect.clone());
        self.touch();
        Ok(effect)
    }

    fn accepts(&self, revision_id: u64, operation_id: AnalysisOperationId) -> bool {
        self.id == revision_id && self.active_operation_id == Some(operation_id)
    }

    fn fail(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        expected: RevisionState,
        failed: RevisionState,
        error: impl Into<String>,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != expected {
            return Err("This revision is no longer waiting for that result.".into());
        }
        self.state = failed;
        self.error = Some(error.into());
        self.pending_effect = None;
        self.active_operation_id = None;
        self.touch();
        Ok(None)
    }

    fn touch(&mut self) {
        self.updated_at_ms = now_millis();
    }

    fn begin_operation(&mut self) -> AnalysisOperationId {
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        self.active_operation_id = Some(self.next_operation_id);
        self.next_operation_id
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisSession {
    pub id: String,
    pub title: String,
    pub storage_fingerprint: String,
    pub revisions: Vec<AnalysisRevision>,
    pub active_revision_id: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl AnalysisSession {
    pub fn with_revision(revision: AnalysisRevision) -> Self {
        let now = now_millis();
        Self {
            id: format!("analysis-{}", now_nanos()),
            title: revision.question.clone(),
            storage_fingerprint: revision.scope.storage_path.clone(),
            active_revision_id: revision.id,
            revisions: vec![revision],
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    pub fn new_revision(
        &mut self,
        question: impl Into<String>,
        scope: AnalysisScope,
    ) -> &mut AnalysisRevision {
        let id = self
            .revisions
            .iter()
            .map(|revision| revision.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.revisions
            .push(AnalysisRevision::draft(id, question, scope));
        self.active_revision_id = id;
        self.updated_at_ms = now_millis();
        self.revisions
            .last_mut()
            .expect("a revision was just pushed")
    }

    pub fn active_revision_mut(&mut self) -> Option<&mut AnalysisRevision> {
        self.revisions
            .iter_mut()
            .find(|revision| revision.id == self.active_revision_id)
    }

    pub fn persisted_bytes(&self) -> Result<Vec<u8>, String> {
        let mut persisted = self.clone();
        prepare_for_storage(&mut persisted);
        fit_session_budget(&mut persisted)?;
        serde_json::to_vec(&persisted)
            .map_err(|error| format!("Could not prepare the analysis session for storage: {error}"))
    }
}

pub fn trim_sessions(sessions: &mut Vec<AnalysisSession>) {
    sessions.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
    });
    sessions.truncate(MAX_ANALYSIS_SESSIONS);
}

pub fn load_sessions(storage_fingerprint: &str) -> Result<Vec<AnalysisSession>, String> {
    let storage = local_storage()?;
    let raw = storage
        .get_item(&storage_key(storage_fingerprint))
        .map_err(|_| {
            "Could not restore local analysis sessions from browser storage.".to_string()
        })?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let mut sessions: Vec<AnalysisSession> = serde_json::from_str(&raw).map_err(|_| {
        "Could not restore local analysis sessions because the saved data is invalid.".to_string()
    })?;
    for session in &mut sessions {
        for revision in &mut session.revisions {
            revision.evidence = None;
            revision.pending_effect = None;
            if revision.execution.is_some() {
                revision.needs_rerun = true;
            }
        }
    }
    trim_sessions(&mut sessions);
    Ok(sessions)
}

pub fn save_sessions(
    storage_fingerprint: &str,
    sessions: &[AnalysisSession],
) -> Result<(), String> {
    let storage = local_storage()?;
    let raw = serialized_sessions(sessions)?;
    storage
        .set_item(&storage_key(storage_fingerprint), &raw)
        .map_err(|_| {
            "Could not save local analysis sessions; they will not survive a refresh.".to_string()
        })
}

pub fn clear_sessions(storage_fingerprint: &str) -> Result<(), String> {
    local_storage()?
        .remove_item(&storage_key(storage_fingerprint))
        .map_err(|_| "Could not clear local analysis sessions from browser storage.".to_string())
}

pub fn analysis_href(scope: &AnalysisScope) -> String {
    let encoded = serde_json::to_string(scope).expect("analysis scopes are serializable");
    format!(
        "?page=tools&analysis_scope={}",
        urlencoding::encode(&encoded)
    )
}

pub fn scope_from_query(query: &str) -> Result<AnalysisScope, String> {
    let query = query.strip_prefix('?').unwrap_or(query);
    let encoded = query
        .split('&')
        .find_map(|parameter| {
            parameter
                .split_once('=')
                .filter(|(key, _)| *key == "analysis_scope")
                .map(|(_, value)| value)
        })
        .ok_or_else(|| "The Analyze link has no scope.".to_string())?;
    let decoded = urlencoding::decode(encoded)
        .map_err(|_| "The Analyze link has an invalid scope.".to_string())?;
    let scope: AnalysisScope = serde_json::from_str(&decoded)
        .map_err(|_| "The Analyze link has an invalid scope.".to_string())?;
    if scope.database.is_empty() || scope.storage_path.is_empty() || scope.items.is_empty() {
        return Err("The Analyze link has an incomplete scope.".into());
    }
    Ok(scope)
}

fn serialized_sessions(sessions: &[AnalysisSession]) -> Result<String, String> {
    let mut persisted = sessions.to_vec();
    trim_sessions(&mut persisted);
    for session in &mut persisted {
        prepare_for_storage(session);
        fit_session_budget(session)?;
    }
    serde_json::to_string(&persisted)
        .map_err(|error| format!("Could not prepare analysis sessions for storage: {error}"))
}

fn prepare_for_storage(session: &mut AnalysisSession) {
    for revision in &mut session.revisions {
        revision.evidence = None;
        revision.pending_effect = None;
        revision.active_operation_id = None;
        revision.next_operation_id = 0;
        revision.needs_rerun = revision.execution.is_some();
    }
}

fn fit_session_budget(session: &mut AnalysisSession) -> Result<(), String> {
    compact_session(session);
    while serde_json::to_vec(&*session)
        .map_err(|error| format!("Could not prepare the analysis session for storage: {error}"))?
        .len()
        > MAX_SESSION_BYTES
    {
        if discard_oldest_derived_data(session) {
            continue;
        }
        if session.revisions.len() <= 1 {
            return Err(
                "Analysis session exceeds the local storage budget and could not be compacted."
                    .into(),
            );
        }
        session.revisions.remove(0);
        session.active_revision_id = session
            .revisions
            .last()
            .map(|revision| revision.id)
            .unwrap_or_default();
    }
    Ok(())
}

fn discard_oldest_derived_data(session: &mut AnalysisSession) -> bool {
    for revision in &mut session.revisions {
        if let Some(execution) = &mut revision.execution {
            if !execution.profiles.is_empty() {
                execution.profiles.clear();
                return true;
            }
        }
        if revision.interpretation.take().is_some() {
            return true;
        }
    }
    false
}

fn compact_session(session: &mut AnalysisSession) {
    truncate_text(&mut session.id, 256);
    truncate_text(&mut session.title, 4 * 1024);
    truncate_text(&mut session.storage_fingerprint, 4 * 1024);
    for revision in &mut session.revisions {
        truncate_text(&mut revision.question, 8 * 1024);
        truncate_text(&mut revision.scope.database, 1024);
        truncate_text(&mut revision.scope.storage_path, 4 * 1024);
        truncate_text(&mut revision.scope.snapshot_id, 1024);
        revision.scope.items.truncate(64);
        for item in &mut revision.scope.items {
            match item {
                AnalysisScopeItem::Dataset { name } => truncate_text(name, 1024),
                AnalysisScopeItem::Root {
                    dataset,
                    file,
                    root_session_id,
                } => {
                    truncate_text(dataset, 1024);
                    truncate_text(file, 4 * 1024);
                    truncate_text(root_session_id, 1024);
                }
                AnalysisScopeItem::Run { run } => compact_run(run),
            }
        }
        if let Some(plan) = &mut revision.plan {
            compact_plan(plan);
        }
        if let Some(error) = &mut revision.error {
            truncate_text(error, 4 * 1024);
        }
    }
}

fn compact_run(run: &mut RunSummary) {
    truncate_text(&mut run.dataset, 1024);
    truncate_text(&mut run.file, 4 * 1024);
    if let Some(run_id) = &mut run.run_id {
        truncate_text(run_id, 1024);
    }
    truncate_text(&mut run.agent_id, 1024);
    if let Some(model_name) = &mut run.model_name {
        truncate_text(model_name, 1024);
    }
    truncate_text(&mut run.session_id, 1024);
    if let Some(root_session_id) = &mut run.root_session_id {
        truncate_text(root_session_id, 1024);
    }
    truncate_text(&mut run.path, 4 * 1024);
    truncate_text(&mut run.status, 1024);
}

fn compact_plan(plan: &mut AnalysisPlan) {
    for text in [
        &mut plan.question,
        &mut plan.intent_summary,
        &mut plan.scope_summary,
        &mut plan.sql,
    ] {
        truncate_text(text, 8 * 1024);
    }
    for values in [
        &mut plan.filters,
        &mut plan.groupings,
        &mut plan.measures,
        &mut plan.expected_columns,
        &mut plan.warnings,
    ] {
        values.truncate(64);
        for value in values {
            truncate_text(value, 1024);
        }
    }
}

fn truncate_text(value: &mut String, max_chars: usize) {
    let Some((index, _)) = value.char_indices().nth(max_chars) else {
        return;
    };
    value.truncate(index);
}

fn storage_key(storage_fingerprint: &str) -> String {
    format!("{STORAGE_PREFIX}{storage_fingerprint}")
}

fn local_storage() -> Result<web_sys::Storage, String> {
    let window = web_sys::window()
        .ok_or_else(|| "Local analysis sessions are unavailable outside a browser.".to_string())?;
    window
        .local_storage()
        .map_err(|_| "Could not access browser storage for analysis sessions.".to_string())?
        .ok_or_else(|| "Browser storage is unavailable for analysis sessions.".to_string())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(1)
        .max(1)
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QueryEvidence;

    #[test]
    fn generated_plan_waits_for_explicit_execution() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let plan_operation = revision.begin_plan_generation().unwrap();
        revision.finish_plan(1, plan_operation, plan()).unwrap();
        assert_eq!(revision.state, RevisionState::PlanReady);
        assert!(revision.pending_effect.is_none());

        revision.confirm_execution().unwrap();
        assert_eq!(
            revision.pending_effect,
            Some(AnalysisEffect::ExecuteSql {
                revision_id: 1,
                operation_id: 2,
                sql: "SELECT status, COUNT(*) FROM default.runs GROUP BY status".into(),
            })
        );
    }

    #[test]
    fn query_result_rows_are_not_persisted() {
        let mut revision = AnalysisRevision::draft(1, "question", scope());
        revision.evidence = Some(QueryEvidence {
            rows: vec![serde_json::json!({"secret-row":"not persisted"})],
            returned_rows: 1,
            truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        });
        let encoded = serde_json::to_string(&AnalysisSession::with_revision(revision)).unwrap();
        assert!(!encoded.contains("secret-row"));
    }

    #[test]
    fn empty_query_result_skips_interpretation() {
        let (mut revision, query_operation) = executing_revision();
        let effect = revision
            .finish_query(1, query_operation, empty_evidence(), Vec::new())
            .unwrap();
        assert_eq!(revision.state, RevisionState::Complete);
        assert_eq!(effect, None);
    }

    #[test]
    fn stale_async_result_is_ignored_without_changing_state() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let plan_operation = revision.begin_plan_generation().unwrap();

        let effect = revision.finish_plan(2, plan_operation, plan()).unwrap();

        assert_eq!(effect, None);
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        assert!(revision.plan.is_none());
    }

    #[test]
    fn query_error_requires_explicit_retry() {
        let (mut revision, query_operation) = executing_revision();
        revision
            .fail_query(1, query_operation, "query timed out")
            .unwrap();
        assert_eq!(revision.state, RevisionState::QueryError);
        assert!(revision.take_pending_effect().is_none());

        revision.confirm_execution().unwrap();

        assert_eq!(revision.state, RevisionState::Executing);
        assert_eq!(
            revision.take_pending_effect(),
            Some(AnalysisEffect::ExecuteSql {
                revision_id: 1,
                operation_id: query_operation + 1,
                sql: "SELECT status, COUNT(*) FROM default.runs GROUP BY status".into(),
            })
        );
    }

    #[test]
    fn analysis_href_round_trips_dataset_root_run_and_multi_run_scopes() {
        let scopes = vec![
            scope(),
            AnalysisScope {
                database: "default".into(),
                storage_path: "tmp/test/".into(),
                snapshot_id: "snapshot-a".into(),
                items: vec![AnalysisScopeItem::Root {
                    dataset: "default".into(),
                    file: "source.json".into(),
                    root_session_id: "root-a".into(),
                }],
            },
            AnalysisScope {
                database: "default".into(),
                storage_path: "tmp/test/".into(),
                snapshot_id: "snapshot-a".into(),
                items: vec![AnalysisScopeItem::Run { run: run("one") }],
            },
            AnalysisScope {
                database: "default".into(),
                storage_path: "tmp/test/".into(),
                snapshot_id: "snapshot-a".into(),
                items: vec![
                    AnalysisScopeItem::Run { run: run("one") },
                    AnalysisScopeItem::Run { run: run("two") },
                ],
            },
        ];

        for scope in scopes {
            assert_eq!(scope_from_query(&analysis_href(&scope)).unwrap(), scope);
        }
    }

    #[test]
    fn delayed_plan_result_cannot_complete_a_regenerated_attempt() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let first_operation = revision.begin_plan_generation().unwrap();
        revision
            .fail_plan(1, first_operation, "provider unavailable")
            .unwrap();
        let second_operation = revision.begin_plan_generation().unwrap();

        assert_eq!(
            revision.finish_plan(1, first_operation, plan()).unwrap(),
            None
        );
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        assert!(revision.plan.is_none());

        revision.finish_plan(1, second_operation, plan()).unwrap();
        assert_eq!(revision.state, RevisionState::PlanReady);
    }

    #[test]
    fn delayed_query_result_cannot_complete_a_retried_attempt() {
        let (mut revision, first_operation) = executing_revision();
        revision
            .fail_query(1, first_operation, "query timed out")
            .unwrap();
        revision.confirm_execution().unwrap();
        let second_operation = take_execute_operation(&mut revision);

        assert_eq!(
            revision
                .finish_query(1, first_operation, empty_evidence(), Vec::new())
                .unwrap(),
            None
        );
        assert_eq!(revision.state, RevisionState::Executing);
        assert!(revision.execution.is_none());

        revision
            .finish_query(1, second_operation, empty_evidence(), Vec::new())
            .unwrap();
        assert_eq!(revision.state, RevisionState::Complete);
    }

    #[test]
    fn delayed_interpretation_result_cannot_complete_a_retried_attempt() {
        let (mut revision, query_operation) = executing_revision();
        let first_operation = match revision
            .finish_query(1, query_operation, evidence_with_rows(), Vec::new())
            .unwrap()
        {
            Some(AnalysisEffect::Interpret { operation_id, .. }) => operation_id,
            effect => panic!("expected an interpretation effect, got {effect:?}"),
        };
        revision
            .fail_interpretation(1, first_operation, "provider unavailable")
            .unwrap();
        let second_operation = match revision.retry_interpretation().unwrap() {
            AnalysisEffect::Interpret { operation_id, .. } => operation_id,
            effect => panic!("expected an interpretation effect, got {effect:?}"),
        };

        assert_eq!(
            revision
                .finish_interpretation(1, first_operation, AnalysisInterpretation::default())
                .unwrap(),
            None
        );
        assert_eq!(revision.state, RevisionState::Interpreting);
        assert!(revision.interpretation.is_none());

        revision
            .finish_interpretation(1, second_operation, AnalysisInterpretation::default())
            .unwrap();
        assert_eq!(revision.state, RevisionState::Complete);
    }

    #[test]
    fn trim_sessions_keeps_the_newest_twenty() {
        let mut sessions = (0..21)
            .map(|id| {
                let mut session = AnalysisSession::with_revision(AnalysisRevision::draft(
                    id,
                    format!("question {id}"),
                    scope(),
                ));
                session.updated_at_ms = id;
                session
            })
            .collect::<Vec<_>>();

        trim_sessions(&mut sessions);

        assert_eq!(sessions.len(), MAX_ANALYSIS_SESSIONS);
        assert!(!sessions.iter().any(|session| session.updated_at_ms == 0));
        assert!(sessions.iter().any(|session| session.updated_at_ms == 20));
    }

    #[test]
    fn persisted_session_fits_storage_budget() {
        let session = AnalysisSession::with_revision(AnalysisRevision::draft(
            1,
            "x".repeat(MAX_SESSION_BYTES),
            scope(),
        ));

        assert!(serde_json::to_vec(&session).unwrap().len() > MAX_SESSION_BYTES);
        assert!(session.persisted_bytes().unwrap().len() <= MAX_SESSION_BYTES);
    }

    #[test]
    fn serialized_bundle_retains_twenty_sessions_when_each_fits_its_own_budget() {
        let sessions = (0..MAX_ANALYSIS_SESSIONS)
            .map(|id| {
                let mut revision = AnalysisRevision::draft(id as u64, "question", scope());
                revision.plan = Some(large_plan());
                revision.state = RevisionState::PlanReady;
                AnalysisSession::with_revision(revision)
            })
            .collect::<Vec<_>>();

        let encoded = serialized_sessions(&sessions).unwrap();
        let restored: Vec<AnalysisSession> = serde_json::from_str(&encoded).unwrap();

        assert_eq!(restored.len(), MAX_ANALYSIS_SESSIONS);
        assert!(restored
            .iter()
            .all(|session| serde_json::to_vec(session).unwrap().len() <= MAX_SESSION_BYTES));
    }

    #[test]
    fn oversized_session_discards_oldest_profiles_and_interpretations_before_revisions() {
        let mut session =
            AnalysisSession::with_revision(AnalysisRevision::draft(1, "old", scope()));
        for id in 1..=5 {
            let mut revision = AnalysisRevision::draft(id, format!("question {id}"), scope());
            revision.execution = Some(huge_execution());
            revision.interpretation = Some(huge_interpretation());
            session.revisions.push(revision);
        }
        session.active_revision_id = 5;

        let restored: AnalysisSession =
            serde_json::from_slice(&session.persisted_bytes().unwrap()).unwrap();

        assert_eq!(restored.revisions.len(), 6);
        assert!(restored.revisions[1]
            .execution
            .as_ref()
            .unwrap()
            .profiles
            .is_empty());
        assert!(restored.revisions[1].interpretation.is_none());
        assert!(!restored.revisions[2]
            .execution
            .as_ref()
            .unwrap()
            .profiles
            .is_empty());
        assert!(restored.revisions[2].interpretation.is_some());
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
            question: "compare failures".into(),
            intent_summary: "Compare failures by status".into(),
            scope_summary: "default dataset".into(),
            filters: Vec::new(),
            groupings: vec!["status".into()],
            measures: vec!["run count".into()],
            expected_columns: vec!["status".into(), "run_count".into()],
            suggested_view: SuggestedView::Distribution,
            sql: "SELECT status, COUNT(*) FROM default.runs GROUP BY status".into(),
            warnings: Vec::new(),
        }
    }

    fn large_plan() -> AnalysisPlan {
        AnalysisPlan {
            id: 1,
            question: "q".repeat(16 * 1024),
            intent_summary: "i".repeat(16 * 1024),
            scope_summary: "s".repeat(16 * 1024),
            filters: Vec::new(),
            groupings: Vec::new(),
            measures: Vec::new(),
            expected_columns: Vec::new(),
            suggested_view: SuggestedView::Table,
            sql: "x".repeat(16 * 1024),
            warnings: Vec::new(),
        }
    }

    fn empty_evidence() -> QueryEvidence {
        QueryEvidence {
            rows: Vec::new(),
            returned_rows: 0,
            truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        }
    }

    fn evidence_with_rows() -> QueryEvidence {
        QueryEvidence {
            rows: vec![serde_json::json!({"status": "failed"})],
            returned_rows: 1,
            truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        }
    }

    fn run(id: &str) -> RunSummary {
        RunSummary {
            dataset: "default".into(),
            file: "source.json".into(),
            run_id: Some(id.into()),
            agent_id: "agent".into(),
            model_name: None,
            session_id: format!("session-{id}"),
            root_session_id: Some("root-a".into()),
            path: format!("agent/root-a/{id}"),
            row_count: 1,
            duplicate_event_ids: 0,
            status: "ok".into(),
        }
    }

    fn huge_execution() -> ExecutionSummary {
        ExecutionSummary {
            returned_rows: 1,
            truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
            executed_at_ms: 1,
            profiles: (0..1)
                .map(|index| ColumnProfile {
                    name: format!("column-{index}"),
                    kind: crate::result_profile::ColumnKind::Text,
                    row_count: 1,
                    non_null_count: 1,
                    missing_count: 0,
                    unique_count: 1,
                    min: None,
                    max: None,
                    mean: None,
                    histogram: Vec::new(),
                    top_values: (0..10)
                        .map(|value| crate::result_profile::ValueCount {
                            label: format!("{index}-{value}-{}", "x".repeat(1024)),
                            count: 1,
                        })
                        .collect(),
                    other_count: 0,
                    type_counts: Default::default(),
                })
                .collect(),
        }
    }

    fn huge_interpretation() -> AnalysisInterpretation {
        AnalysisInterpretation {
            observations: (0..12).map(|_| "o".repeat(4 * 1024)).collect(),
            ..AnalysisInterpretation::default()
        }
    }

    fn take_execute_operation(revision: &mut AnalysisRevision) -> u64 {
        match revision.take_pending_effect() {
            Some(AnalysisEffect::ExecuteSql { operation_id, .. }) => operation_id,
            effect => panic!("expected an execute effect, got {effect:?}"),
        }
    }

    fn executing_revision() -> (AnalysisRevision, u64) {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let plan_operation = revision.begin_plan_generation().unwrap();
        revision.finish_plan(1, plan_operation, plan()).unwrap();
        revision.confirm_execution().unwrap();
        let query_operation = take_execute_operation(&mut revision);
        (revision, query_operation)
    }
}
