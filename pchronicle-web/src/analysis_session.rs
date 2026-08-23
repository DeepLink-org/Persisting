#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::model::{QueryCatalog, QueryEvidence, RunSummary};
use crate::result_profile::ColumnProfile;

pub const MAX_ANALYSIS_SESSIONS: usize = 20;
pub const MAX_SESSION_BYTES: usize = 256 * 1024;
pub const STORAGE_PREFIX: &str = "pchronicle_analysis:";
pub const MAX_SPEC_REPAIRS: u32 = 2;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecFilter {
    pub field: String,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CompiledQuery {
    pub spec: AnalysisSpec,
    pub sql: String,
    pub assumptions: Vec<String>,
    pub identity_columns: Vec<String>,
    pub expected_columns: Vec<String>,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CompileFailure {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub engine_detail: Option<String>,
}

impl CompileFailure {
    pub fn summary(&self) -> String {
        match self.field.as_deref() {
            Some(field) => format!("{}: {}", field, self.message),
            None => self.message.clone(),
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzeTraceKind {
    GenerateSpec,
    Compile,
    RepairSpec,
    Execute,
    Interpret,
}

impl AnalyzeTraceKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::GenerateSpec => "Write spec",
            Self::Compile => "Compile SQL",
            Self::RepairSpec => "Repair spec",
            Self::Execute => "Run evidence",
            Self::Interpret => "Interpret results",
        }
    }

    pub fn phase(self) -> &'static str {
        match self {
            Self::GenerateSpec | Self::RepairSpec | Self::Interpret => "agent",
            Self::Compile | Self::Execute => "system",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzeTraceStatus {
    Pending,
    Running,
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalyzeTraceStep {
    pub id: u64,
    pub kind: AnalyzeTraceKind,
    pub status: AnalyzeTraceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const TRACE_TEXT_CHARS: usize = 4 * 1024;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<AnalysisSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_sql: Option<String>,
    #[serde(default)]
    pub repair_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_plan_context: Option<AnalysisPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_spec_context: Option<AnalysisSpec>,
    pub manually_edited: bool,
    pub execution: Option<ExecutionSummary>,
    pub interpretation: Option<AnalysisInterpretation>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub needs_rerun: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<AnalyzeTraceStep>,
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
            spec: None,
            compiled_sql: None,
            repair_count: 0,
            prior_plan_context: None,
            prior_spec_context: None,
            manually_edited: false,
            execution: None,
            interpretation: None,
            error: None,
            created_at_ms: now,
            updated_at_ms: now,
            needs_rerun: false,
            trace: Vec::new(),
            evidence: None,
            pending_effect: None,
            active_operation_id: None,
            next_operation_id: 0,
        }
    }

    pub fn begin_plan_generation(&mut self) -> Result<AnalysisOperationId, String> {
        match self.state {
            RevisionState::Draft
            | RevisionState::PlanError
            | RevisionState::Stale
            | RevisionState::QueryError => {
                self.state = RevisionState::GeneratingPlan;
                self.error = None;
                self.pending_effect = None;
                self.repair_count = 0;
                self.compiled_sql = None;
                self.trace.clear();
                let question = self.question.clone();
                self.start_trace_step(AnalyzeTraceKind::GenerateSpec, Some(&question));
                self.touch();
                Ok(self.begin_operation())
            }
            _ => Err("A spec can only be generated from a draft, error, or stale revision.".into()),
        }
    }

    pub fn finish_compiled_spec(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        spec: AnalysisSpec,
        sql: String,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != RevisionState::GeneratingPlan {
            return Err("This revision is not waiting for a compiled spec.".into());
        }
        self.spec = Some(spec.clone());
        self.compiled_sql = Some(sql.clone());
        self.repair_count = 0;
        self.manually_edited = false;
        self.plan = Some(plan_from_compiled(
            self.id,
            &self.question,
            &self.scope,
            &spec,
            &sql,
        ));
        self.state = RevisionState::PlanReady;
        self.error = None;
        self.complete_trace_step(Some(&sql));
        self.touch();
        self.confirm_execution()?;
        Ok(self.pending_effect.clone())
    }

    pub fn note_spec_ready(&mut self, spec: &AnalysisSpec) {
        let output = serde_json::to_string_pretty(spec).ok();
        self.complete_trace_step(output.as_deref());
        let prompt = format!("{} · {} · {}", spec.intent, spec.grain, spec.measure);
        self.start_trace_step(AnalyzeTraceKind::Compile, Some(&prompt));
        self.touch();
    }

    pub fn fail_compile(
        &mut self,
        revision_id: u64,
        operation_id: AnalysisOperationId,
        error: impl Into<String>,
    ) -> Result<Option<AnalysisEffect>, String> {
        if !self.accepts(revision_id, operation_id) {
            return Ok(None);
        }
        if self.state != RevisionState::GeneratingPlan {
            return Err("This revision is not waiting for a compiled spec.".into());
        }
        let error = error.into();
        self.fail_running_trace_step(&error);
        self.compiled_sql = None;
        self.pending_effect = None;
        if self.repair_count < MAX_SPEC_REPAIRS {
            self.repair_count = self.repair_count.saturating_add(1);
            self.error = Some(error.clone());
            self.start_trace_step(AnalyzeTraceKind::RepairSpec, Some(&error));
            self.touch();
            return Ok(None);
        }
        self.fail(
            revision_id,
            operation_id,
            RevisionState::GeneratingPlan,
            RevisionState::PlanError,
            error,
        )
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
        self.compiled_sql = self.plan.as_ref().map(|plan| plan.sql.clone());
        self.state = RevisionState::PlanReady;
        self.error = None;
        self.pending_effect = None;
        self.active_operation_id = None;
        let sql = self.compiled_sql.clone();
        self.complete_trace_step(sql.as_deref());
        self.touch();
        Ok(None)
    }

    pub fn confirm_execution(&mut self) -> Result<(), String> {
        if self.state == RevisionState::QueryError && !self.manually_edited && !self.needs_rerun {
            return Err("Revise the spec instead of replaying the failed SQL.".into());
        }
        if !matches!(
            self.state,
            RevisionState::PlanReady | RevisionState::QueryError
        ) {
            return Err("Review a ready spec before running this analysis.".into());
        }
        let sql = self
            .executable_sql()
            .ok_or_else(|| "Compiled SQL is required before running this analysis.".to_string())?;
        self.execution = None;
        self.evidence = None;
        self.interpretation = None;
        self.needs_rerun = false;
        let operation_id = self.begin_operation();
        self.state = RevisionState::Executing;
        self.error = None;
        self.pending_effect = Some(AnalysisEffect::ExecuteSql {
            revision_id: self.id,
            operation_id,
            sql: sql.clone(),
        });
        self.start_trace_step(AnalyzeTraceKind::Execute, Some(&sql));
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
        let summary = format!(
            "returned_rows={} truncated={}",
            evidence.returned_rows, evidence.truncated
        );
        self.execution = Some(ExecutionSummary {
            returned_rows: evidence.returned_rows,
            truncated: evidence.truncated,
            max_rows: evidence.max_rows,
            max_bytes: evidence.max_bytes,
            executed_at_ms: now_millis(),
            profiles,
        });
        self.evidence = Some(evidence);
        self.interpretation = None;
        self.error = None;
        self.needs_rerun = false;
        self.complete_trace_step(Some(&summary));
        let effect = has_rows.then(|| AnalysisEffect::Interpret {
            revision_id: self.id,
            operation_id: self.begin_operation(),
        });
        self.pending_effect = effect.clone();
        self.state = if effect.is_some() {
            self.start_trace_step(AnalyzeTraceKind::Interpret, Some(&summary));
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
        self.interpretation = Some(interpretation.clone());
        self.state = RevisionState::Complete;
        self.error = None;
        self.pending_effect = None;
        self.active_operation_id = None;
        let output = serde_json::to_string_pretty(&interpretation).ok();
        self.complete_trace_step(output.as_deref());
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

    pub fn executable_sql(&self) -> Option<String> {
        if self.manually_edited {
            return self
                .plan
                .as_ref()
                .map(|plan| plan.sql.clone())
                .filter(|sql| !sql.trim().is_empty());
        }
        self.compiled_sql
            .clone()
            .filter(|sql| !sql.trim().is_empty())
            .or_else(|| {
                self.plan
                    .as_ref()
                    .map(|plan| plan.sql.clone())
                    .filter(|sql| !sql.trim().is_empty())
            })
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
        self.start_trace_step(AnalyzeTraceKind::Interpret, Some("retry interpretation"));
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
        let message = self.error.clone().unwrap_or_default();
        self.fail_running_trace_step(&message);
        self.pending_effect = None;
        self.active_operation_id = None;
        self.touch();
        Ok(None)
    }

    fn start_trace_step(&mut self, kind: AnalyzeTraceKind, prompt: Option<&str>) -> u64 {
        let id = self
            .trace
            .last()
            .map(|step| step.id)
            .unwrap_or(0)
            .saturating_add(1);
        self.trace.push(AnalyzeTraceStep {
            id,
            kind,
            status: AnalyzeTraceStatus::Running,
            prompt: prompt
                .map(digest_trace_text)
                .filter(|value| !value.is_empty()),
            output: None,
            error: None,
        });
        id
    }

    fn complete_trace_step(&mut self, output: Option<&str>) {
        if let Some(step) = self.running_trace_step() {
            step.status = AnalyzeTraceStatus::Ok;
            step.output = output
                .map(digest_trace_text)
                .filter(|value| !value.is_empty());
            step.error = None;
        }
    }

    fn fail_running_trace_step(&mut self, error: &str) {
        if let Some(step) = self.running_trace_step() {
            step.status = AnalyzeTraceStatus::Error;
            if !error.is_empty() {
                step.error = Some(digest_trace_text(error));
            }
        }
    }

    fn running_trace_step(&mut self) -> Option<&mut AnalyzeTraceStep> {
        self.trace
            .iter_mut()
            .rev()
            .find(|step| step.status == AnalyzeTraceStatus::Running)
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
        let storage_fingerprint =
            storage_fingerprint(&revision.scope.database, &revision.scope.storage_path);
        Self {
            id: format!("analysis-{}", now_nanos()),
            title: revision.question.clone(),
            storage_fingerprint,
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

    pub fn new_follow_up(
        &mut self,
        question: impl Into<String>,
    ) -> Result<&mut AnalysisRevision, String> {
        let question = question.into();
        if question.trim().is_empty() {
            return Err("A follow-up question is required.".into());
        }
        let (scope, prior_plan_context) = self
            .revisions
            .iter()
            .find(|revision| revision.id == self.active_revision_id)
            .map(|revision| (revision.scope.clone(), revision.plan.clone()))
            .ok_or_else(|| "The active analysis revision is unavailable.".to_string())?;
        let revision = self.new_revision(question, scope);
        revision.prior_plan_context = prior_plan_context;
        Ok(revision)
    }

    pub fn active_revision_mut(&mut self) -> Option<&mut AnalysisRevision> {
        self.revisions
            .iter_mut()
            .find(|revision| revision.id == self.active_revision_id)
    }

    pub fn active_revision(&self) -> Option<&AnalysisRevision> {
        self.revisions
            .iter()
            .find(|revision| revision.id == self.active_revision_id)
    }

    pub fn reconcile_catalog(&mut self, snapshot_id: &str) {
        let mut changed = false;
        for revision in &mut self.revisions {
            if revision.scope.snapshot_id == snapshot_id {
                continue;
            }
            if revision.execution.is_none()
                && revision.plan.is_some()
                && matches!(
                    revision.state,
                    RevisionState::PlanReady | RevisionState::QueryError
                )
            {
                revision.state = RevisionState::Stale;
                revision.error = None;
                revision.pending_effect = None;
                revision.active_operation_id = None;
                revision.touch();
                changed = true;
            }
        }
        if changed {
            self.updated_at_ms = now_millis();
        }
    }

    pub fn apply_working_scope_change(
        &mut self,
        question: impl Into<String>,
        next_scope: AnalysisScope,
    ) -> Result<u64, String> {
        let (active_revision_id, state, was_executed, prior_plan_context) = self
            .active_revision()
            .map(|revision| {
                (
                    revision.id,
                    revision.state.clone(),
                    revision.execution.is_some(),
                    revision.plan.clone(),
                )
            })
            .ok_or_else(|| "The active analysis revision is unavailable.".to_string())?;

        if matches!(
            &state,
            RevisionState::GeneratingPlan | RevisionState::Executing
        ) {
            return Err("Analysis scope cannot change while an operation is running.".into());
        }

        if was_executed {
            let revision = self.new_revision(question, next_scope);
            revision.prior_plan_context = prior_plan_context;
            return Ok(revision.id);
        }

        let next_state = match state {
            RevisionState::Draft | RevisionState::PlanError => RevisionState::Draft,
            RevisionState::PlanReady | RevisionState::QueryError | RevisionState::Stale => {
                if prior_plan_context.is_some() {
                    RevisionState::Stale
                } else {
                    RevisionState::Draft
                }
            }
            _ => return Err("Analysis scope cannot change in this revision state.".into()),
        };

        let revision = self
            .active_revision_mut()
            .ok_or_else(|| "The active analysis revision is unavailable.".to_string())?;
        revision.scope = next_scope;
        revision.state = next_state;
        revision.error = None;
        revision.needs_rerun = false;
        revision.pending_effect = None;
        revision.active_operation_id = None;
        revision.touch();
        self.updated_at_ms = now_millis();
        Ok(active_revision_id)
    }

    pub fn normalize_inflight_for_navigation(&mut self) {
        let mut changed = false;
        for revision in &mut self.revisions {
            let normalized = match revision.state {
                RevisionState::GeneratingPlan => {
                    revision.state = RevisionState::Draft;
                    revision.error = None;
                    true
                }
                RevisionState::Executing => {
                    revision.state = RevisionState::PlanReady;
                    revision.error = None;
                    revision.needs_rerun = true;
                    true
                }
                RevisionState::Interpreting => {
                    if revision.evidence.is_some() {
                        revision.state = RevisionState::InterpretationError;
                        revision.error = Some(
                            "Interpretation was interrupted when this analysis was left.".into(),
                        );
                    } else {
                        revision.state = RevisionState::QueryError;
                        revision.error = None;
                        revision.needs_rerun = true;
                    }
                    true
                }
                _ => false,
            };
            if normalized {
                revision.pending_effect = None;
                revision.active_operation_id = None;
                revision.touch();
                changed = true;
            }
        }
        if changed {
            self.updated_at_ms = now_millis();
        }
    }

    pub fn select_revision(&mut self, revision_id: u64) -> Result<(), String> {
        if !self
            .revisions
            .iter()
            .any(|revision| revision.id == revision_id)
        {
            return Err("The selected analysis revision is unavailable.".into());
        }
        self.active_revision_id = revision_id;
        self.updated_at_ms = now_millis();
        Ok(())
    }

    pub fn mark_updated(&mut self) {
        self.updated_at_ms = now_millis();
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

pub fn storage_fingerprint(database: &str, storage_path: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in database
        .as_bytes()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(storage_path.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
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
            normalize_persisted_revision(revision);
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

pub fn restore_session(raw: &str) -> Result<AnalysisSession, String> {
    let mut session: AnalysisSession = serde_json::from_str(raw).map_err(|_| {
        "Could not restore the local analysis session because the saved data is invalid."
            .to_string()
    })?;
    prepare_for_storage(&mut session);
    Ok(session)
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
    if scope.database.trim().is_empty()
        || scope.storage_path.trim().is_empty()
        || scope.items.is_empty()
        || scope.items.iter().any(|item| match item {
            AnalysisScopeItem::Dataset { name } => name.trim().is_empty(),
            AnalysisScopeItem::Root {
                dataset,
                file,
                root_session_id,
            } => {
                dataset.trim().is_empty()
                    || file.trim().is_empty()
                    || root_session_id.trim().is_empty()
            }
            AnalysisScopeItem::Run { run } => {
                run.dataset.trim().is_empty()
                    || run.file.trim().is_empty()
                    || run.agent_id.trim().is_empty()
                    || run.session_id.trim().is_empty()
            }
        })
    {
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
        normalize_persisted_revision(revision);
    }
}

fn normalize_persisted_revision(revision: &mut AnalysisRevision) {
    revision.evidence = None;
    revision.pending_effect = None;
    revision.active_operation_id = None;
    revision.next_operation_id = 0;

    let was_in_flight = matches!(
        revision.state,
        RevisionState::GeneratingPlan | RevisionState::Executing | RevisionState::Interpreting
    );
    revision.state = match &revision.state {
        RevisionState::GeneratingPlan => RevisionState::Draft,
        RevisionState::Executing => RevisionState::PlanReady,
        RevisionState::Interpreting
        | RevisionState::InterpretationError
        | RevisionState::Complete
            if revision.execution.is_some() =>
        {
            RevisionState::QueryError
        }
        state => state.clone(),
    };
    if was_in_flight || revision.execution.is_some() {
        revision.needs_rerun = true;
    }
    for step in &mut revision.trace {
        if matches!(
            step.status,
            AnalyzeTraceStatus::Running | AnalyzeTraceStatus::Pending
        ) {
            step.status = AnalyzeTraceStatus::Error;
            if step.error.is_none() {
                step.error = Some("Session restored before this step finished.".into());
            }
        }
    }
    mark_legacy_sql_plan_stale(revision);
}

fn mark_legacy_sql_plan_stale(revision: &mut AnalysisRevision) {
    let has_legacy_sql = revision.spec.is_none()
        && revision
            .compiled_sql
            .as_ref()
            .is_none_or(|sql| sql.trim().is_empty())
        && !revision.manually_edited
        && revision
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.sql.trim().is_empty());
    if has_legacy_sql {
        revision.state = RevisionState::Stale;
        revision.error = Some(
            "This session stored a handwritten SQL plan. Analyze again to compile a spec.".into(),
        );
        revision.compiled_sql = None;
        revision.pending_effect = None;
        revision.active_operation_id = None;
    }
}

fn plan_from_compiled(
    id: u64,
    question: &str,
    scope: &AnalysisScope,
    spec: &AnalysisSpec,
    sql: &str,
) -> AnalysisPlan {
    AnalysisPlan {
        id,
        question: question.into(),
        intent_summary: spec.intent.clone(),
        scope_summary: scope
            .items
            .first()
            .map(|item| match item {
                AnalysisScopeItem::Dataset { name } => format!("dataset {name}"),
                AnalysisScopeItem::Root {
                    dataset,
                    root_session_id,
                    ..
                } => format!("{dataset} root {root_session_id}"),
                AnalysisScopeItem::Run { run } => format!("{} {}", run.dataset, run.session_id),
            })
            .unwrap_or_else(|| scope.database.clone()),
        filters: spec
            .filters
            .iter()
            .map(|filter| format!("{} {}", filter.field, filter.op))
            .collect(),
        groupings: spec.dimension.iter().cloned().collect(),
        measures: vec![spec.measure.clone()],
        expected_columns: spec.identity_columns.clone(),
        suggested_view: match spec.output.as_str() {
            "distribution" => SuggestedView::Distribution,
            _ => SuggestedView::Table,
        },
        sql: sql.into(),
        warnings: spec.assumptions.clone(),
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
        for step in &mut revision.trace {
            if step.prompt.take().is_some() || step.output.take().is_some() {
                return true;
            }
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
        if let Some(plan) = &mut revision.prior_plan_context {
            compact_plan(plan);
        }
        if let Some(error) = &mut revision.error {
            truncate_text(error, 4 * 1024);
        }
        revision.trace.truncate(32);
        for step in &mut revision.trace {
            if let Some(prompt) = &mut step.prompt {
                truncate_text(prompt, TRACE_TEXT_CHARS);
            }
            if let Some(output) = &mut step.output {
                truncate_text(output, TRACE_TEXT_CHARS);
            }
            if let Some(error) = &mut step.error {
                truncate_text(error, TRACE_TEXT_CHARS);
            }
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

fn digest_trace_text(value: &str) -> String {
    let mut text = value.to_string();
    truncate_text(&mut text, TRACE_TEXT_CHARS);
    text
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
    fn completed_interpretation_summary_persists_without_query_rows() {
        let session = complete_session();

        let restored: AnalysisSession =
            serde_json::from_slice(&session.persisted_bytes().unwrap()).unwrap();
        let revision = restored.revisions.first().unwrap();

        assert!(revision.evidence.is_none());
        assert_eq!(
            revision.interpretation.as_ref().unwrap().observations,
            vec!["One failed row was returned."]
        );
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
    fn query_rows_become_visible_before_interpretation_finishes() {
        let (mut revision, query_operation) = executing_revision();
        let evidence = evidence_with_rows();

        let effect = revision
            .finish_query(1, query_operation, evidence.clone(), Vec::new())
            .unwrap();

        assert_eq!(revision.evidence, Some(evidence));
        assert_eq!(revision.state, RevisionState::Interpreting);
        assert!(matches!(
            effect,
            Some(AnalysisEffect::Interpret { revision_id: 1, .. })
        ));
    }

    #[test]
    fn interpretation_failure_keeps_query_evidence() {
        let (mut revision, interpretation_operation) = interpreting_revision();
        let evidence = revision.evidence.clone();

        revision
            .fail_interpretation(1, interpretation_operation, "provider unavailable")
            .unwrap();

        assert_eq!(revision.state, RevisionState::InterpretationError);
        assert_eq!(revision.evidence, evidence);
        assert!(revision.execution.is_some());
    }

    #[test]
    fn follow_up_creates_a_new_unexecuted_revision() {
        let mut session = complete_session();

        let next = session.new_follow_up("only failed runs").unwrap();

        assert_eq!(next.question, "only failed runs");
        assert_eq!(next.state, RevisionState::Draft);
        assert!(next.evidence.is_none());
        assert!(next.execution.is_none());
        assert!(next.plan.is_none());
        assert!(next.interpretation.is_none());
        assert_eq!(next.scope, scope());
        assert_eq!(next.prior_plan_context, Some(plan()));
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
        assert!(revision.confirm_execution().is_err());
        assert!(revision.begin_plan_generation().is_ok());
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        assert!(revision.compiled_sql.is_none());
    }

    #[test]
    fn rerun_failure_cannot_reveal_a_previous_interpretation() {
        let raw = serde_json::to_string(&complete_session()).unwrap();
        let mut restored = restore_session(&raw).unwrap();
        let revision = restored.active_revision_mut().unwrap();
        assert!(revision.execution.is_some());
        assert!(revision.interpretation.is_some());
        assert!(revision.needs_rerun);

        revision.confirm_execution().unwrap();
        let (revision_id, operation_id) = match revision.take_pending_effect().unwrap() {
            AnalysisEffect::ExecuteSql {
                revision_id,
                operation_id,
                ..
            } => (revision_id, operation_id),
            effect => panic!("expected execute effect, got {effect:?}"),
        };
        revision
            .fail_query(revision_id, operation_id, "rerun failed")
            .unwrap();

        assert_eq!(revision.state, RevisionState::QueryError);
        assert!(revision.execution.is_none());
        assert!(revision.evidence.is_none());
        assert!(revision.interpretation.is_none());
        assert!(!revision.needs_rerun);
    }

    #[test]
    fn new_query_evidence_defensively_discards_an_old_interpretation() {
        let (mut revision, query_operation) = executing_revision();
        revision.interpretation = Some(AnalysisInterpretation {
            observations: vec!["old conclusion".into()],
            ..AnalysisInterpretation::default()
        });

        revision
            .finish_query(1, query_operation, evidence_with_rows(), Vec::new())
            .unwrap();

        assert!(revision.interpretation.is_none());
        assert_eq!(revision.state, RevisionState::Interpreting);
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
    fn multi_run_scope_round_trips_through_analyze_url() {
        let scope = AnalysisScope::from_runs(&catalog(), vec![run("left"), run("right")]);
        let href = analysis_href(&scope);
        let decoded = scope_from_query(href.split_once('?').unwrap().1).unwrap();

        assert_eq!(decoded.items, scope.items);
    }

    #[test]
    fn analyze_url_rejects_incomplete_scope_coordinates() {
        let incomplete = AnalysisScope {
            items: vec![AnalysisScopeItem::Root {
                dataset: "default".into(),
                file: String::new(),
                root_session_id: "root-a".into(),
            }],
            ..scope()
        };

        assert!(scope_from_query(&analysis_href(&incomplete)).is_err());
    }

    #[test]
    fn restored_session_has_summaries_but_requires_rows_to_be_rerun() {
        let mut restored =
            restore_session(&serde_json::to_string(&complete_session()).unwrap()).unwrap();
        let revision = restored.active_revision().unwrap();

        assert!(revision.evidence.is_none());
        assert!(revision.execution.is_some());
        assert!(revision.interpretation.is_some());
        assert!(revision.needs_rerun);
        assert_eq!(revision.state, RevisionState::QueryError);
        assert!(restored
            .active_revision_mut()
            .unwrap()
            .confirm_execution()
            .is_ok());
    }

    #[test]
    fn catalog_snapshot_change_marks_unexecuted_plan_stale() {
        let mut session = plan_ready_session("snapshot-a");

        session.reconcile_catalog("snapshot-b");

        assert_eq!(
            session.active_revision().unwrap().state,
            RevisionState::Stale
        );
    }

    #[test]
    fn scope_change_marks_only_unexecuted_review_stale() {
        for state in [RevisionState::PlanReady, RevisionState::QueryError] {
            let mut unexecuted = plan_ready_session("snapshot-a");
            unexecuted.active_revision_mut().unwrap().state = state;
            let active_revision_id = unexecuted.active_revision_id;
            let reviewed_scope = unexecuted.active_revision().unwrap().scope.clone();
            let next_scope = AnalysisScope {
                items: vec![AnalysisScopeItem::Dataset {
                    name: "secondary".into(),
                }],
                ..reviewed_scope.clone()
            };

            let returned_revision_id = unexecuted
                .apply_working_scope_change("compare failures", next_scope.clone())
                .unwrap();

            assert_eq!(returned_revision_id, active_revision_id);
            assert_eq!(
                unexecuted.active_revision().unwrap().state,
                RevisionState::Stale
            );
            assert_eq!(unexecuted.active_revision().unwrap().scope, next_scope);
        }
    }

    #[test]
    fn draft_and_plan_error_scope_changes_persist_as_editable_drafts() {
        let next_scope = AnalysisScope {
            items: vec![AnalysisScopeItem::Dataset {
                name: "secondary".into(),
            }],
            ..scope()
        };
        let draft = AnalysisRevision::draft(1, "draft question", scope());
        let mut failed = AnalysisRevision::draft(1, "failed question", scope());
        let operation_id = failed.begin_plan_generation().unwrap();
        failed
            .fail_plan(1, operation_id, "provider unavailable")
            .unwrap();

        for revision in [draft, failed] {
            let mut session = AnalysisSession::with_revision(revision);
            session
                .apply_working_scope_change("working question", next_scope.clone())
                .unwrap();

            let changed = session.active_revision().unwrap();
            assert_eq!(changed.scope, next_scope);
            assert_eq!(changed.state, RevisionState::Draft);
            assert!(changed.error.is_none());
            assert_eq!(session.revisions.len(), 1);
        }
    }

    #[test]
    fn reviewed_unexecuted_scope_changes_persist_and_become_stale() {
        for state in [
            RevisionState::PlanReady,
            RevisionState::QueryError,
            RevisionState::Stale,
        ] {
            let mut session = plan_ready_session("snapshot-a");
            let revision = session.active_revision_mut().unwrap();
            revision.state = state;
            revision.error = Some("old state error".into());
            let next_scope = AnalysisScope {
                items: vec![AnalysisScopeItem::Dataset {
                    name: "secondary".into(),
                }],
                ..scope()
            };

            session
                .apply_working_scope_change("compare failures", next_scope.clone())
                .unwrap();

            let changed = session.active_revision().unwrap();
            assert_eq!(changed.scope, next_scope);
            assert_eq!(changed.state, RevisionState::Stale);
            assert!(changed.error.is_none());
            assert_eq!(session.revisions.len(), 1);
        }
    }

    #[test]
    fn changed_draft_scope_survives_refresh_and_revision_selection() {
        let mut session =
            AnalysisSession::with_revision(AnalysisRevision::draft(1, "draft question", scope()));
        let next_scope = AnalysisScope {
            items: vec![AnalysisScopeItem::Dataset {
                name: "secondary".into(),
            }],
            ..scope()
        };
        session
            .apply_working_scope_change("draft question", next_scope.clone())
            .unwrap();
        let changed_revision_id = session.active_revision_id;
        session.new_revision("another question", scope());

        let mut restored: AnalysisSession =
            serde_json::from_slice(&session.persisted_bytes().unwrap()).unwrap();
        restored.select_revision(changed_revision_id).unwrap();

        assert_eq!(restored.active_revision().unwrap().scope, next_scope);
        assert_eq!(
            restored.active_revision().unwrap().state,
            RevisionState::Draft
        );
    }

    #[test]
    fn executed_scope_change_creates_a_draft_and_preserves_the_old_snapshot() {
        let mut session = complete_session();
        let old_revision_id = session.active_revision_id;
        let old_scope = session.active_revision().unwrap().scope.clone();
        let old_plan = session.active_revision().unwrap().plan.clone();
        let next_scope = AnalysisScope {
            items: vec![AnalysisScopeItem::Dataset {
                name: "secondary".into(),
            }],
            ..old_scope.clone()
        };

        let next_revision_id = session
            .apply_working_scope_change("Compare the new scope", next_scope.clone())
            .unwrap();

        assert_ne!(next_revision_id, old_revision_id);
        let next = session.active_revision().unwrap();
        assert_eq!(next.state, RevisionState::Draft);
        assert_eq!(next.question, "Compare the new scope");
        assert_eq!(next.scope, next_scope);
        assert_eq!(next.prior_plan_context, old_plan);
        let old = session
            .revisions
            .iter()
            .find(|revision| revision.id == old_revision_id)
            .unwrap();
        assert_eq!(old.state, RevisionState::Complete);
        assert_eq!(old.scope, old_scope);
        assert!(old.execution.is_some());
    }

    #[test]
    fn scope_change_is_rejected_while_plan_or_query_generation_is_in_flight() {
        let next_scope = AnalysisScope {
            items: vec![AnalysisScopeItem::Dataset {
                name: "secondary".into(),
            }],
            ..scope()
        };
        let mut planning =
            AnalysisSession::with_revision(AnalysisRevision::draft(1, "planning", scope()));
        planning
            .active_revision_mut()
            .unwrap()
            .begin_plan_generation()
            .unwrap();

        assert!(planning
            .apply_working_scope_change("planning", next_scope.clone())
            .is_err());
        assert_eq!(
            planning.active_revision().unwrap().state,
            RevisionState::GeneratingPlan
        );

        let (executing, _) = executing_revision();
        let mut querying = AnalysisSession::with_revision(executing);

        assert!(querying
            .apply_working_scope_change("executing", next_scope)
            .is_err());
        assert_eq!(
            querying.active_revision().unwrap().state,
            RevisionState::Executing
        );
    }

    #[test]
    fn restored_query_error_scope_change_also_preserves_the_executed_snapshot() {
        let raw = serde_json::to_string(&complete_session()).unwrap();
        let mut session = restore_session(&raw).unwrap();
        let old_revision_id = session.active_revision_id;
        assert_eq!(
            session.active_revision().unwrap().state,
            RevisionState::QueryError
        );
        assert!(session.active_revision().unwrap().execution.is_some());
        let next_scope = AnalysisScope {
            items: vec![AnalysisScopeItem::Dataset {
                name: "secondary".into(),
            }],
            ..scope()
        };

        session
            .apply_working_scope_change("retry with less scope", next_scope.clone())
            .unwrap();

        assert_eq!(
            session.active_revision().unwrap().state,
            RevisionState::Draft
        );
        assert_eq!(session.active_revision().unwrap().scope, next_scope);
        let old = session
            .revisions
            .iter()
            .find(|revision| revision.id == old_revision_id)
            .unwrap();
        assert_eq!(old.state, RevisionState::QueryError);
        assert!(old.execution.is_some());
        assert!(old.interpretation.is_some());
    }

    #[test]
    fn leaving_a_session_normalizes_every_inflight_revision_for_retry() {
        let mut session =
            AnalysisSession::with_revision(AnalysisRevision::draft(1, "planning", scope()));
        session
            .active_revision_mut()
            .unwrap()
            .begin_plan_generation()
            .unwrap();

        let mut executing = AnalysisRevision::draft(2, "executing", scope());
        let plan_operation = executing.begin_plan_generation().unwrap();
        executing.finish_plan(2, plan_operation, plan()).unwrap();
        executing.confirm_execution().unwrap();
        session.revisions.push(executing);

        let (mut interpreting, query_operation) = executing_revision();
        interpreting.id = 3;
        if let Some(AnalysisEffect::ExecuteSql { revision_id, .. }) =
            interpreting.pending_effect.as_mut()
        {
            *revision_id = 3;
        }
        interpreting
            .finish_query(3, query_operation, evidence_with_rows(), Vec::new())
            .unwrap();
        session.revisions.push(interpreting);

        session.normalize_inflight_for_navigation();

        assert_eq!(session.revisions[0].state, RevisionState::Draft);
        assert_eq!(session.revisions[1].state, RevisionState::PlanReady);
        assert!(session.revisions[1].needs_rerun);
        assert_eq!(
            session.revisions[2].state,
            RevisionState::InterpretationError
        );
        assert!(session.revisions[2].evidence.is_some());
        for revision in &session.revisions {
            assert!(revision.active_operation_id.is_none());
            assert!(revision.pending_effect.is_none());
        }
    }

    #[test]
    fn storage_fingerprint_partitions_database_and_storage_path() {
        let baseline = storage_fingerprint("default", "tmp/test/");

        assert_eq!(
            AnalysisSession::with_revision(AnalysisRevision::draft(1, "question", scope()))
                .storage_fingerprint,
            baseline
        );
        assert_ne!(baseline, storage_fingerprint("other", "tmp/test/"));
        assert_ne!(baseline, storage_fingerprint("default", "tmp/other/"));
    }

    #[test]
    fn catalog_reconciliation_preserves_executed_revision_snapshot() {
        let mut session = complete_session();

        session.reconcile_catalog("snapshot-b");

        let revision = session.active_revision().unwrap();
        assert_eq!(revision.state, RevisionState::Complete);
        assert_eq!(revision.scope.snapshot_id, "snapshot-a");
        assert!(revision.execution.is_some());
    }

    #[test]
    fn selecting_history_changes_only_the_active_revision() {
        let mut session = complete_session();
        let first_revision_id = session.active_revision_id;
        let second_revision_id = session
            .new_follow_up("compare only explicit failures")
            .unwrap()
            .id;

        session.select_revision(first_revision_id).unwrap();

        assert_eq!(session.active_revision_id, first_revision_id);
        assert_eq!(
            session.active_revision().unwrap().state,
            RevisionState::Complete
        );
        assert!(session.active_revision().unwrap().pending_effect.is_none());
        assert!(session
            .revisions
            .iter()
            .any(|revision| revision.id == second_revision_id));
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
        revision.needs_rerun = true;
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
    fn restored_generating_plan_becomes_a_rerunnable_draft_without_an_operation() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        revision.begin_plan_generation().unwrap();

        let mut restored = restored_revision(revision);

        assert_eq!(restored.state, RevisionState::Draft);
        assert!(restored.needs_rerun);
        assert!(restored.active_operation_id.is_none());
        assert!(restored.pending_effect.is_none());
        assert!(restored.begin_plan_generation().is_ok());
    }

    #[test]
    fn restored_executing_plan_becomes_confirmable_without_an_operation() {
        let (revision, _) = executing_revision();

        let mut restored = restored_revision(revision);

        assert_eq!(restored.state, RevisionState::PlanReady);
        assert!(restored.needs_rerun);
        assert!(restored.plan.is_some());
        assert!(restored.active_operation_id.is_none());
        assert!(restored.pending_effect.is_none());
        assert!(restored.confirm_execution().is_ok());
    }

    #[test]
    fn restored_interpretation_becomes_confirmable_without_an_operation() {
        let (mut revision, query_operation) = executing_revision();
        revision
            .finish_query(1, query_operation, evidence_with_rows(), Vec::new())
            .unwrap();

        let mut restored = restored_revision(revision);

        assert_eq!(restored.state, RevisionState::QueryError);
        assert!(restored.needs_rerun);
        assert!(restored.plan.is_some());
        assert!(restored.execution.is_some());
        assert!(restored.active_operation_id.is_none());
        assert!(restored.pending_effect.is_none());
        assert!(restored.confirm_execution().is_ok());
    }

    #[test]
    fn restored_interpretation_error_requires_a_query_rerun_without_fabricating_evidence() {
        let (mut revision, interpretation_operation) = interpreting_revision();
        revision
            .fail_interpretation(1, interpretation_operation, "provider unavailable")
            .unwrap();

        let mut restored = restored_revision(revision);

        assert_eq!(restored.state, RevisionState::QueryError);
        assert!(restored.evidence.is_none());
        assert!(restored.retry_interpretation().is_err());
        assert!(restored.confirm_execution().is_ok());
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
    fn persisted_mutation_keeps_an_old_session_at_the_twenty_session_boundary() {
        let mut sessions = (0..21)
            .map(|id| {
                let mut session = AnalysisSession::with_revision(AnalysisRevision::draft(
                    id,
                    format!("question {id}"),
                    scope(),
                ));
                session.id = format!("session-{id}");
                session.updated_at_ms = id;
                session
            })
            .collect::<Vec<_>>();
        sessions[0].mark_updated();

        trim_sessions(&mut sessions);

        assert!(sessions.iter().any(|session| session.id == "session-0"));
        assert!(!sessions.iter().any(|session| session.id == "session-1"));
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

    fn catalog() -> QueryCatalog {
        QueryCatalog {
            snapshot_id: "snapshot-a".into(),
            read_only: true,
            database: "default".into(),
            storage_path: "tmp/test/".into(),
            path_column: "_file_".into(),
            datasets: Vec::new(),
            tables: Vec::new(),
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
                    median: None,
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

    fn restored_revision(revision: AnalysisRevision) -> AnalysisRevision {
        let session = AnalysisSession::with_revision(revision);
        let restored: AnalysisSession =
            serde_json::from_slice(&session.persisted_bytes().unwrap()).unwrap();
        restored.revisions.into_iter().next().unwrap()
    }

    fn executing_revision() -> (AnalysisRevision, u64) {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let plan_operation = revision.begin_plan_generation().unwrap();
        revision.finish_plan(1, plan_operation, plan()).unwrap();
        revision.confirm_execution().unwrap();
        let query_operation = take_execute_operation(&mut revision);
        (revision, query_operation)
    }

    fn interpreting_revision() -> (AnalysisRevision, u64) {
        let (mut revision, query_operation) = executing_revision();
        let interpretation_operation = match revision
            .finish_query(1, query_operation, evidence_with_rows(), Vec::new())
            .unwrap()
        {
            Some(AnalysisEffect::Interpret { operation_id, .. }) => operation_id,
            effect => panic!("expected an interpretation effect, got {effect:?}"),
        };
        (revision, interpretation_operation)
    }

    fn complete_session() -> AnalysisSession {
        let (mut revision, interpretation_operation) = interpreting_revision();
        revision
            .finish_interpretation(
                1,
                interpretation_operation,
                AnalysisInterpretation {
                    observations: vec!["One failed row was returned.".into()],
                    inferences: vec!["Failures may warrant investigation.".into()],
                    limitations: Vec::new(),
                    follow_ups: vec!["only failed runs".into()],
                    references: Vec::new(),
                },
            )
            .unwrap();
        AnalysisSession::with_revision(revision)
    }

    fn plan_ready_session(snapshot_id: &str) -> AnalysisSession {
        let mut scope = scope();
        scope.snapshot_id = snapshot_id.into();
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope);
        let operation_id = revision.begin_plan_generation().unwrap();
        revision.finish_plan(1, operation_id, plan()).unwrap();
        AnalysisSession::with_revision(revision)
    }

    fn sample_spec() -> AnalysisSpec {
        AnalysisSpec {
            intent: "composition".into(),
            grain: "tool_call".into(),
            measure: "tool_call_count".into(),
            dimension: Some("function_name".into()),
            filters: Vec::new(),
            ranking: None,
            output: "table".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        }
    }

    #[test]
    fn compile_failures_repair_spec_twice_then_stop_without_executing_sql() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let operation_id = revision.begin_plan_generation().unwrap();
        assert!(revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap()
            .is_none());
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        assert_eq!(revision.repair_count, 1);
        assert!(revision.pending_effect.is_none());
        assert!(revision.compiled_sql.is_none());

        assert!(revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap()
            .is_none());
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        assert_eq!(revision.repair_count, 2);

        assert!(revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap()
            .is_none());
        assert_eq!(revision.state, RevisionState::PlanError);
        assert!(revision.pending_effect.is_none());
        assert!(revision.compiled_sql.is_none());
    }

    #[test]
    fn compiled_spec_queues_execution_of_the_new_sql() {
        let mut revision = AnalysisRevision::draft(1, "compare models", scope());
        let operation_id = revision.begin_plan_generation().unwrap();
        let effect = revision
            .finish_compiled_spec(
                1,
                operation_id,
                sample_spec(),
                "SELECT t.function_name, COUNT(*) AS tool_call_count FROM dataset.tool_calls AS t GROUP BY t.function_name".into(),
            )
            .unwrap();
        match effect {
            Some(AnalysisEffect::ExecuteSql { sql, .. }) => {
                assert!(sql.contains("tool_calls"));
            }
            other => panic!("expected ExecuteSql, got {other:?}"),
        }
        assert_eq!(revision.state, RevisionState::Executing);
    }

    #[test]
    fn analyze_trace_records_generate_compile_execute_and_interpret() {
        let mut revision = AnalysisRevision::draft(1, "compare models", scope());
        let operation_id = revision.begin_plan_generation().unwrap();
        assert_eq!(revision.trace[0].kind, AnalyzeTraceKind::GenerateSpec);
        assert_eq!(revision.trace[0].status, AnalyzeTraceStatus::Running);
        assert!(revision.trace[0]
            .prompt
            .as_deref()
            .unwrap()
            .contains("compare models"));

        revision.note_spec_ready(&sample_spec());
        assert_eq!(revision.trace[0].status, AnalyzeTraceStatus::Ok);
        assert_eq!(revision.trace[1].kind, AnalyzeTraceKind::Compile);
        assert_eq!(revision.trace[1].status, AnalyzeTraceStatus::Running);

        let query_operation = revision
            .finish_compiled_spec(1, operation_id, sample_spec(), "SELECT 1".into())
            .unwrap()
            .and_then(|effect| match effect {
                AnalysisEffect::ExecuteSql { operation_id, .. } => Some(operation_id),
                _ => None,
            })
            .expect("execute");
        assert_eq!(revision.trace[1].status, AnalyzeTraceStatus::Ok);
        assert_eq!(revision.trace[1].output.as_deref(), Some("SELECT 1"));
        assert_eq!(revision.trace[2].kind, AnalyzeTraceKind::Execute);
        assert_eq!(revision.trace[2].status, AnalyzeTraceStatus::Running);

        let interpret = revision
            .finish_query(1, query_operation, evidence_with_rows(), Vec::new())
            .unwrap()
            .expect("interpret");
        assert_eq!(revision.trace[2].status, AnalyzeTraceStatus::Ok);
        assert_eq!(revision.trace[3].kind, AnalyzeTraceKind::Interpret);
        assert_eq!(revision.trace[3].status, AnalyzeTraceStatus::Running);

        let AnalysisEffect::Interpret {
            operation_id: interpret_op,
            ..
        } = interpret
        else {
            panic!("expected interpret");
        };
        revision
            .finish_interpretation(
                1,
                interpret_op,
                AnalysisInterpretation {
                    observations: vec!["two rows".into()],
                    inferences: Vec::new(),
                    limitations: Vec::new(),
                    follow_ups: Vec::new(),
                    references: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(revision.trace[3].status, AnalyzeTraceStatus::Ok);
        assert_eq!(revision.state, RevisionState::Complete);
    }

    #[test]
    fn compile_failure_records_repair_then_stops_on_the_third_error() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        let operation_id = revision.begin_plan_generation().unwrap();
        revision.note_spec_ready(&sample_spec());
        revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap();
        assert_eq!(revision.trace[1].kind, AnalyzeTraceKind::Compile);
        assert_eq!(revision.trace[1].status, AnalyzeTraceStatus::Error);
        assert_eq!(revision.trace[2].kind, AnalyzeTraceKind::RepairSpec);
        assert_eq!(revision.trace[2].status, AnalyzeTraceStatus::Running);

        revision.note_spec_ready(&sample_spec());
        revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap();
        revision.note_spec_ready(&sample_spec());
        revision
            .fail_compile(1, operation_id, "runs has no unified status")
            .unwrap();
        assert_eq!(revision.state, RevisionState::PlanError);
        assert_eq!(
            revision.trace.last().unwrap().kind,
            AnalyzeTraceKind::Compile
        );
        assert_eq!(
            revision.trace.last().unwrap().status,
            AnalyzeTraceStatus::Error
        );
        assert!(!revision
            .trace
            .iter()
            .any(|step| step.status == AnalyzeTraceStatus::Running));
    }

    #[test]
    fn query_error_does_not_replay_failed_sql() {
        let (mut revision, query_operation) = executing_revision();
        revision
            .fail_query(1, query_operation, "column does not exist")
            .unwrap();
        assert_eq!(revision.state, RevisionState::QueryError);
        assert!(revision.confirm_execution().is_err());
        assert!(revision.begin_plan_generation().is_ok());
    }

    #[test]
    fn legacy_sql_plan_without_spec_is_stale() {
        let mut revision = AnalysisRevision::draft(1, "compare failures", scope());
        revision.plan = Some(plan());
        revision.state = RevisionState::PlanReady;
        mark_legacy_sql_plan_stale(&mut revision);
        assert_eq!(revision.state, RevisionState::Stale);
        assert!(revision.confirm_execution().is_err());
    }
}
