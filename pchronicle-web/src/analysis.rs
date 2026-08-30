use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::analysis_agent::{self, EvidenceDigest, InterpretationRequest, PlanRequest};
use crate::analysis_session::{
    self, AnalysisEffect, AnalysisInterpretation, AnalysisOperationId, AnalysisPlan,
    AnalysisRevision, AnalysisScope, AnalysisScopeItem, AnalysisSession, AnalyzeTraceKind,
    AnalyzeTraceStatus, AnalyzeTraceStep, CompileFailure, EvidenceReference, RevisionState,
    SuggestedView,
};
use crate::api;
use crate::api::ApiFailure;
use crate::llm;
use crate::llm_settings::LlmSettings;
use crate::model::{QueryCatalog, QueryEvidence};
use crate::notice::{ErrorNotice, WorkspaceNotice, workspace_notice};
use crate::result_explorer::{ResultExplorer, ResultIdentity, identity_href};
use crate::result_profile::{AnalysisRefinement, ColumnProfile, profile_rows};

const QUESTION_STARTERS: [&str; 3] = [
    "Compare step counts per run by agent model",
    "Show the distribution of step latency and the slowest 20 steps",
    "Count tool calls by function name and drill into the busiest runs",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryAction {
    Analyze,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposerTab {
    Ask,
    Sql,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ComposerModel {
    primary_label: &'static str,
    primary_enabled: bool,
    submits_sql: bool,
    show_spec_summary: bool,
}

impl ComposerModel {
    fn from_context(
        tab: ComposerTab,
        revision: Option<&AnalysisRevision>,
        draft_question: &str,
        generating: bool,
        catalog_ready: bool,
        scope_ready: bool,
        model_ready: bool,
    ) -> Self {
        let show_spec_summary =
            revision.is_some_and(|revision| revision.spec.is_some() && !revision.manually_edited);
        match tab {
            ComposerTab::Ask => Self {
                primary_label: "Analyze",
                primary_enabled: catalog_ready
                    && scope_ready
                    && model_ready
                    && !draft_question.trim().is_empty()
                    && !generating,
                submits_sql: false,
                show_spec_summary,
            },
            ComposerTab::Sql => Self {
                primary_label: "Run",
                primary_enabled: !generating
                    && revision.is_some_and(|revision| revision.executable_sql().is_some()),
                submits_sql: true,
                show_spec_summary,
            },
        }
    }
}

fn composer_tab_after_catalog_insert() -> ComposerTab {
    ComposerTab::Sql
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnalysisViewModel {
    primary_action: PrimaryAction,
    run_enabled: bool,
    question_out_of_date: bool,
    query_in_flight: bool,
    manually_edited: bool,
    sql_disclosure_label: &'static str,
    trace_open: bool,
}

impl AnalysisViewModel {
    fn from_revision(revision: &AnalysisRevision, draft_question: &str) -> Self {
        let primary_action = match revision.state {
            RevisionState::Draft
            | RevisionState::PlanError
            | RevisionState::Stale
            | RevisionState::PlanReady
            | RevisionState::QueryError
            | RevisionState::Complete
            | RevisionState::InterpretationError => PrimaryAction::Analyze,
            _ => PrimaryAction::None,
        };
        let sql_ready = revision.executable_sql().is_some();
        let question_matches = draft_question.trim() == revision.question.trim();
        let run_sql = revision.manually_edited || revision.needs_rerun;
        Self {
            primary_action,
            run_enabled: primary_action == PrimaryAction::Analyze
                && (question_matches || revision.manually_edited)
                && (!run_sql || sql_ready),
            question_out_of_date: primary_action == PrimaryAction::Analyze
                && !question_matches
                && !revision.manually_edited,
            query_in_flight: matches!(
                revision.state,
                RevisionState::GeneratingPlan
                    | RevisionState::Executing
                    | RevisionState::Interpreting
            ),
            manually_edited: revision.manually_edited,
            sql_disclosure_label: "Compiled SQL",
            trace_open: matches!(
                revision.state,
                RevisionState::GeneratingPlan
                    | RevisionState::Executing
                    | RevisionState::Interpreting
            ),
        }
    }
}

fn insert_sql_token(sql: &str, cursor: usize, token: &str) -> String {
    let mut index = cursor.min(sql.len());
    while index > 0 && !sql.is_char_boundary(index) {
        index -= 1;
    }
    format!("{}{}{}", &sql[..index], token, &sql[index..])
}

fn apply_inserted_token(
    revision: &mut AnalysisRevision,
    token: &str,
    cursor: usize,
) -> Result<usize, String> {
    if matches!(
        revision.state,
        RevisionState::GeneratingPlan | RevisionState::Executing | RevisionState::Interpreting
    ) {
        return Err("SQL cannot be edited while an operation is running.".into());
    }
    let sql = revision
        .plan
        .as_ref()
        .map(|plan| plan.sql.clone())
        .unwrap_or_default();
    let mut index = cursor.min(sql.len());
    while index > 0 && !sql.is_char_boundary(index) {
        index -= 1;
    }
    apply_manual_sql(revision, insert_sql_token(&sql, cursor, token))?;
    Ok(index + token.len())
}

fn apply_manual_sql(revision: &mut AnalysisRevision, sql: String) -> Result<(), String> {
    if matches!(
        revision.state,
        RevisionState::GeneratingPlan | RevisionState::Executing | RevisionState::Interpreting
    ) {
        return Err("SQL cannot be edited while an operation is running.".into());
    }
    if !matches!(
        revision.state,
        RevisionState::Draft
            | RevisionState::PlanError
            | RevisionState::Stale
            | RevisionState::PlanReady
            | RevisionState::QueryError
            | RevisionState::Complete
            | RevisionState::InterpretationError
    ) {
        return Err("SQL can only be edited in a draft or reviewed version.".into());
    }
    if let Some(plan) = revision.plan.as_mut() {
        plan.sql = sql;
    } else {
        revision.plan = Some(AnalysisPlan {
            id: revision.id,
            question: revision.question.clone(),
            intent_summary: "Manual SQL".into(),
            scope_summary: revision
                .scope
                .items
                .first()
                .map(|item| scope_item_label(item))
                .unwrap_or_default(),
            filters: Vec::new(),
            groupings: Vec::new(),
            measures: Vec::new(),
            expected_columns: Vec::new(),
            suggested_view: SuggestedView::Table,
            sql,
            warnings: Vec::new(),
        });
    }
    revision.manually_edited = true;
    revision.state = RevisionState::PlanReady;
    revision.clear_error();
    revision.execution = None;
    revision.evidence = None;
    revision.interpretation = None;
    revision.needs_rerun = false;
    revision.pending_effect = None;
    revision.active_operation_id = None;
    Ok(())
}

fn compile_error_text(failure: &CompileFailure) -> String {
    match failure.field.as_deref() {
        Some(field) => format!("{}: {}", field, failure.message),
        None => failure.message.clone(),
    }
}

fn record_api_failure(revision: &mut AnalysisRevision, failure: &ApiFailure) {
    revision.set_api_error_meta(
        Some(failure.code.clone()),
        failure.request_id.clone(),
        failure.engine_detail.clone(),
    );
}

fn record_compile_failure(revision: &mut AnalysisRevision, failure: &CompileFailure) {
    revision.set_api_error_meta(
        Some(failure.code.clone()),
        failure.request_id.clone(),
        failure.engine_detail.clone(),
    );
}

fn notice_from_revision(revision: &AnalysisRevision) -> Option<WorkspaceNotice> {
    if revision.error_code.is_none()
        && revision.error_request_id.is_none()
        && revision.error_engine_detail.is_none()
    {
        return None;
    }
    Some(workspace_notice(&ApiFailure {
        status: 0,
        code: revision.error_code.clone().unwrap_or_default(),
        message: revision.error.clone().unwrap_or_default(),
        request_id: revision.error_request_id.clone(),
        field: None,
        engine_detail: revision.error_engine_detail.clone(),
        raw: revision.error.clone().unwrap_or_default(),
    }))
}

#[derive(Clone)]
struct PreparedInterpretation {
    revision_id: u64,
    operation_id: AnalysisOperationId,
    digest: EvidenceDigest,
}

fn finish_query_for_interpretation(
    revision: &mut AnalysisRevision,
    revision_id: u64,
    operation_id: AnalysisOperationId,
    evidence: QueryEvidence,
    profiles: Vec<ColumnProfile>,
) -> Result<Option<PreparedInterpretation>, String> {
    let effect = revision.finish_query(revision_id, operation_id, evidence, profiles)?;
    let Some(AnalysisEffect::Interpret {
        revision_id,
        operation_id,
    }) = effect
    else {
        return Ok(None);
    };
    let plan = revision.plan.as_ref().ok_or_else(|| {
        "An analysis plan is required before summarizing query results.".to_string()
    })?;
    let evidence = revision
        .evidence
        .as_ref()
        .ok_or_else(|| "Query results are required before creating a summary.".to_string())?;
    let profiles = revision
        .execution
        .as_ref()
        .map(|execution| execution.profiles.as_slice())
        .unwrap_or_default();
    let digest = analysis_agent::build_evidence_digest(plan, &revision.scope, evidence, profiles);
    let _ = revision.take_pending_effect();
    Ok(Some(PreparedInterpretation {
        revision_id,
        operation_id,
        digest,
    }))
}

fn retry_interpretation_from_evidence(
    revision: &mut AnalysisRevision,
) -> Result<PreparedInterpretation, String> {
    let effect = revision.retry_interpretation()?;
    let AnalysisEffect::Interpret {
        revision_id,
        operation_id,
    } = effect
    else {
        return Err("Retry did not prepare an interpretation operation.".into());
    };
    let plan = revision
        .plan
        .as_ref()
        .ok_or_else(|| "The reviewed plan is unavailable for interpretation.".to_string())?;
    let evidence = revision
        .evidence
        .as_ref()
        .ok_or_else(|| "Query results are unavailable; rerun the analysis first.".to_string())?;
    let profiles = revision
        .execution
        .as_ref()
        .map(|execution| execution.profiles.as_slice())
        .unwrap_or_default();
    let digest = analysis_agent::build_evidence_digest(plan, &revision.scope, evidence, profiles);
    let _ = revision.take_pending_effect();
    Ok(PreparedInterpretation {
        revision_id,
        operation_id,
        digest,
    })
}

fn follow_up_plan_allowed(
    source_revision_id: u64,
    active_revision_id: u64,
    draft_question: &str,
    source_question: &str,
) -> bool {
    source_revision_id == active_revision_id && draft_question.trim() == source_question.trim()
}

fn interpretation_reference_identity(reference: &EvidenceReference) -> Option<ResultIdentity> {
    identity_href(&serde_json::json!({
        "dataset": reference.dataset,
        "_file_": reference.file,
        "run_id": reference.run_id,
        "agent_id": reference.agent_id,
        "session_id": reference.session_id,
        "root_session_id": reference.root_session_id,
        "turn_id": reference.turn_id,
    }))
}

fn revision_for_callback<'a>(
    session: &'a mut AnalysisSession,
    expected_session_id: &str,
    revision_id: u64,
) -> Option<&'a mut AnalysisRevision> {
    if session.id != expected_session_id {
        return None;
    }
    session
        .revisions
        .iter_mut()
        .find(|revision| revision.id == revision_id)
}

fn scope_without_item(
    scope: &AnalysisScope,
    index: usize,
    catalog: Option<&QueryCatalog>,
) -> Option<AnalysisScope> {
    if index >= scope.items.len() {
        return None;
    }
    if scope.items.len() == 1 {
        return matches!(
            scope.items.first(),
            Some(AnalysisScopeItem::Root { .. } | AnalysisScopeItem::Run { .. })
        )
        .then(|| catalog.map(AnalysisScope::from_catalog))
        .flatten();
    }
    let mut next = scope.clone();
    next.items.remove(index);
    Some(next)
}

fn scope_item_removal_enabled(
    scope: &AnalysisScope,
    catalog: Option<&QueryCatalog>,
    state: Option<&RevisionState>,
) -> bool {
    scope_without_item(scope, 0, catalog).is_some()
        && !matches!(
            state,
            Some(RevisionState::GeneratingPlan | RevisionState::Executing)
        )
}

fn launch_interpretation(
    config: llm::LlmConfig,
    expected_session_id: String,
    prepared: PreparedInterpretation,
    mut session: Signal<Option<AnalysisSession>>,
    mut recent_sessions: Signal<Vec<AnalysisSession>>,
    mut storage_notice: Signal<Option<String>>,
) {
    spawn(async move {
        let result = analysis_agent::interpret(InterpretationRequest {
            config,
            revision_id: prepared.revision_id,
            digest: prepared.digest.clone(),
        })
        .await;
        let Some(mut current) = session() else {
            return;
        };
        if current.id != expected_session_id {
            return;
        }
        let Some(revision) =
            revision_for_callback(&mut current, &expected_session_id, prepared.revision_id)
        else {
            return;
        };
        match result {
            Ok(mut interpretation) => {
                analysis_agent::ensure_truncation_limitation(&mut interpretation, &prepared.digest);
                let _ = revision.finish_interpretation(
                    prepared.revision_id,
                    prepared.operation_id,
                    interpretation,
                );
            }
            Err(error) => {
                let _ = revision.fail_interpretation(
                    prepared.revision_id,
                    prepared.operation_id,
                    error.message,
                );
            }
        }
        persist_session(&current, &mut recent_sessions, &mut storage_notice);
        session.set(Some(current));
    });
}

#[component]
pub fn AnalysisWorkspace(
    catalog: Option<QueryCatalog>,
    initial_scope: Option<AnalysisScope>,
    requested_session_id: Option<String>,
    on_session_change: EventHandler<String>,
) -> Element {
    let default_scope = catalog.as_ref().map(AnalysisScope::from_catalog);
    let initial_workspace_scope = initial_scope.or(default_scope);
    let mut scope = use_signal(move || initial_workspace_scope);
    let mut question = use_signal(String::new);
    let mut session = use_signal(|| None::<AnalysisSession>);
    let mut recent_sessions = use_signal(Vec::<AnalysisSession>::new);
    let mut config = use_signal(llm::load_config);
    let mut settings_open = use_signal(|| false);
    let mut storage_notice = use_signal(|| None::<String>);
    let mut clear_confirmation = use_signal(|| false);
    let mut restored = use_signal(|| false);
    let mut selected_table = use_signal(String::new);
    let mut sql_caret = use_signal(|| None::<usize>);
    let mut analyze_trace_open = use_signal(|| false);
    let mut composer_tab = use_signal(|| ComposerTab::Ask);

    use_effect(use_reactive(
        (&catalog, &requested_session_id),
        move |(restore_catalog, restore_requested)| {
            if restored() {
                return;
            }
            let Some(catalog) = restore_catalog.as_ref() else {
                return;
            };
            restored.set(true);
            let fingerprint =
                analysis_session::storage_fingerprint(&catalog.database, &catalog.storage_path);
            let mut sessions = match analysis_session::load_sessions(&fingerprint) {
                Ok(sessions) => sessions,
                Err(message) => {
                    storage_notice.set(Some(message));
                    Vec::new()
                }
            };
            let requested_id = restore_requested.as_deref().filter(|id| !id.is_empty());
            let mut restored_session = requested_id
                .and_then(|id| {
                    sessions
                        .iter()
                        .find(|candidate| candidate.id == id)
                        .cloned()
                })
                .unwrap_or_else(|| {
                    let initial_scope =
                        scope().unwrap_or_else(|| AnalysisScope::from_catalog(catalog));
                    AnalysisSession::with_revision(AnalysisRevision::draft(1, "", initial_scope))
                });
            restored_session.storage_fingerprint = fingerprint;
            restored_session.reconcile_catalog(&catalog.snapshot_id);
            if let Some(revision) = restored_session.active_revision() {
                question.set(revision.question.clone());
                scope.set(Some(scope_for_catalog(&revision.scope, catalog)));
            }
            let session_id = restored_session.id.clone();
            sessions.retain(|saved| saved.id != session_id);
            sessions.push(restored_session.clone());
            analysis_session::trim_sessions(&mut sessions);
            recent_sessions.set(sessions);
            let persisted =
                persist_session(&restored_session, &mut recent_sessions, &mut storage_notice);
            if let Some(session_id) = persisted_session_id(&session_id, persisted) {
                on_session_change.call(session_id);
            }
            session.set(Some(restored_session));
        },
    ));

    use_effect(move || {
        if let Some(index) = sql_caret() {
            set_sql_textarea_cursor(index);
            sql_caret.set(None);
        }
    });

    let active_revision = session().and_then(|value| {
        value
            .revisions
            .into_iter()
            .find(|revision| revision.id == value.active_revision_id)
    });
    let draft_question = question();
    let view_model = active_revision
        .as_ref()
        .map(|revision| AnalysisViewModel::from_revision(revision, &draft_question));
    let generating = active_revision.as_ref().is_some_and(|revision| {
        matches!(
            revision.state,
            RevisionState::GeneratingPlan | RevisionState::Executing | RevisionState::Interpreting
        )
    });
    let composer = ComposerModel::from_context(
        composer_tab(),
        active_revision.as_ref(),
        &draft_question,
        generating,
        catalog.is_some(),
        scope().is_some(),
        config().is_configured(),
    );

    let scope_for_generate = scope;
    let catalog_for_generate = catalog.clone();
    let generate_plan = move |_| {
        let Some(catalog) = catalog_for_generate.clone() else {
            return;
        };
        let Some(scope) = scope_for_generate() else {
            return;
        };
        let prompt = question().trim().to_string();
        if prompt.is_empty() || !config().is_configured() {
            return;
        }

        let mut next_session = session().unwrap_or_else(|| {
            AnalysisSession::with_revision(AnalysisRevision::draft(
                1,
                prompt.clone(),
                scope.clone(),
            ))
        });
        let previous_plan = next_session.active_revision_mut().and_then(|revision| {
            revision
                .plan
                .clone()
                .or_else(|| revision.prior_plan_context.clone())
        });
        let previous_spec = next_session.active_revision().and_then(|revision| {
            revision
                .spec
                .clone()
                .or_else(|| revision.prior_spec_context.clone())
        });
        let needs_new_revision = next_session.active_revision_mut().is_some_and(|revision| {
            !matches!(
                revision.state,
                RevisionState::Draft
                    | RevisionState::PlanError
                    | RevisionState::Stale
                    | RevisionState::QueryError
            ) || revision.question != prompt
                || revision.scope != scope
        });
        if needs_new_revision {
            let revision = next_session.new_revision(prompt.clone(), scope.clone());
            revision.prior_plan_context = previous_plan.clone();
            revision.prior_spec_context = previous_spec.clone();
        }
        if next_session.title.trim().is_empty() {
            next_session.title = prompt.clone();
        }
        let Some(revision) = next_session.active_revision_mut() else {
            return;
        };
        revision.question = prompt.clone();
        let revision_id = revision.id;
        let Ok(operation_id) = revision.begin_plan_generation() else {
            return;
        };
        let expected_session_id = next_session.id.clone();
        let persisted = persist_session(&next_session, &mut recent_sessions, &mut storage_notice);
        if let Some(session_id) = persisted_session_id(&expected_session_id, persisted) {
            on_session_change.call(session_id);
        }
        session.set(Some(next_session));

        let request = PlanRequest {
            config: config(),
            catalog,
            scope,
            question: prompt,
            plan_id: revision_id,
            previous_plan,
            previous_spec,
            compile_error: None,
            refinement: None,
        };
        spawn(async move {
            run_spec_compile_execute(
                request,
                expected_session_id,
                revision_id,
                operation_id,
                session,
                recent_sessions,
                storage_notice,
            )
            .await;
        });
    };

    let run_sql = move |_| {
        let Some(mut current) = session() else {
            return;
        };
        let Some(revision) = current.active_revision_mut() else {
            return;
        };
        if revision.executable_sql().is_none() {
            return;
        }
        if revision.confirm_sql_run().is_err() {
            return;
        }
        let Some(AnalysisEffect::ExecuteSql {
            revision_id,
            operation_id,
            sql,
        }) = revision.take_pending_effect()
        else {
            return;
        };
        let expected_session_id = current.id.clone();
        let interpretation_config = config();
        persist_session(&current, &mut recent_sessions, &mut storage_notice);
        session.set(Some(current));
        spawn(async move {
            let result = api::query_evidence_interactive(&sql).await;
            let Some(mut current) = session() else {
                return;
            };
            if current.id != expected_session_id {
                return;
            }
            let Some(revision) =
                revision_for_callback(&mut current, &expected_session_id, revision_id)
            else {
                return;
            };
            let prepared = match result {
                Ok(evidence) => {
                    let profiles = profile_rows(&evidence.rows);
                    finish_query_for_interpretation(
                        revision,
                        revision_id,
                        operation_id,
                        evidence,
                        profiles,
                    )
                    .ok()
                    .flatten()
                }
                Err(failure) => {
                    let _ = revision.fail_query(revision_id, operation_id, failure.to_string());
                    record_api_failure(revision, &failure);
                    None
                }
            };
            persist_session(&current, &mut recent_sessions, &mut storage_notice);
            session.set(Some(current));
            if let Some(prepared) = prepared {
                launch_interpretation(
                    interpretation_config,
                    expected_session_id,
                    prepared,
                    session,
                    recent_sessions,
                    storage_notice,
                );
            }
        });
    };

    let catalog_for_refinement = catalog.clone();
    let prepare_refinement = move |refinement: AnalysisRefinement| {
        let Some(catalog) = catalog_for_refinement.clone() else {
            return;
        };
        if !config().is_configured() {
            return;
        }
        let Some(mut current) = session() else {
            return;
        };
        let Some(source) = current
            .revisions
            .iter()
            .find(|revision| revision.id == current.active_revision_id)
        else {
            return;
        };
        let draft_question = question();
        if !refinement_plan_allowed(
            source.id,
            refinement_source_revision_id(&refinement),
            &draft_question,
            &source.question,
        ) {
            return;
        }
        let prompt = source.question.clone();
        let scope = source.scope.clone();
        let previous_plan = source.plan.clone();
        let previous_spec = source.spec.clone();
        let revision = current.new_revision(prompt.clone(), scope.clone());
        revision.prior_plan_context = previous_plan.clone();
        revision.prior_spec_context = previous_spec.clone();
        let revision_id = revision.id;
        let Ok(operation_id) = revision.begin_plan_generation() else {
            return;
        };
        let expected_session_id = current.id.clone();
        let persisted = persist_session(&current, &mut recent_sessions, &mut storage_notice);
        if let Some(session_id) = persisted_session_id(&expected_session_id, persisted) {
            on_session_change.call(session_id);
        }
        session.set(Some(current));

        let request = PlanRequest {
            config: config(),
            catalog,
            scope,
            question: prompt,
            plan_id: revision_id,
            previous_plan,
            previous_spec,
            compile_error: None,
            refinement: Some(refinement),
        };
        spawn(async move {
            run_spec_compile_execute(
                request,
                expected_session_id,
                revision_id,
                operation_id,
                session,
                recent_sessions,
                storage_notice,
            )
            .await;
        });
    };

    let retry_interpretation = move |_| {
        if !config().is_configured() {
            return;
        }
        let Some(mut current) = session() else {
            return;
        };
        let expected_session_id = current.id.clone();
        let Some(revision) = current.active_revision_mut() else {
            return;
        };
        let Ok(prepared) = retry_interpretation_from_evidence(revision) else {
            return;
        };
        let interpretation_config = config();
        persist_session(&current, &mut recent_sessions, &mut storage_notice);
        session.set(Some(current));
        launch_interpretation(
            interpretation_config,
            expected_session_id,
            prepared,
            session,
            recent_sessions,
            storage_notice,
        );
    };

    let catalog_for_follow_up = catalog.clone();
    let generate_follow_up = move |suggested_question: String| {
        let Some(catalog) = catalog_for_follow_up.clone() else {
            return;
        };
        if !config().is_configured() {
            return;
        }
        let Some(mut current) = session() else {
            return;
        };
        let Some(source) = current
            .revisions
            .iter()
            .find(|revision| revision.id == current.active_revision_id)
        else {
            return;
        };
        if !follow_up_plan_allowed(
            source.id,
            current.active_revision_id,
            &question(),
            &source.question,
        ) {
            return;
        }
        let previous_plan = source.plan.clone();
        let previous_spec = source.spec.clone();
        let scope = source.scope.clone();
        let Ok(revision) = current.new_follow_up(suggested_question.clone()) else {
            return;
        };
        let revision_id = revision.id;
        let Ok(operation_id) = revision.begin_plan_generation() else {
            return;
        };
        let expected_session_id = current.id.clone();
        question.set(suggested_question.clone());
        let persisted = persist_session(&current, &mut recent_sessions, &mut storage_notice);
        if let Some(session_id) = persisted_session_id(&expected_session_id, persisted) {
            on_session_change.call(session_id);
        }
        session.set(Some(current));

        let request = PlanRequest {
            config: config(),
            catalog,
            scope,
            question: suggested_question,
            plan_id: revision_id,
            previous_plan,
            previous_spec,
            compile_error: None,
            refinement: None,
        };
        spawn(async move {
            run_spec_compile_execute(
                request,
                expected_session_id,
                revision_id,
                operation_id,
                session,
                recent_sessions,
                storage_notice,
            )
            .await;
        });
    };

    let edit_follow_up = move |suggested_question: String| {
        let Some(current) = session() else {
            return;
        };
        let Some(source) = current
            .revisions
            .iter()
            .find(|revision| revision.id == current.active_revision_id)
        else {
            return;
        };
        if follow_up_plan_allowed(
            source.id,
            current.active_revision_id,
            &question(),
            &source.question,
        ) {
            question.set(suggested_question);
        }
    };

    let rewrite_problem = move |_| {
        if let Some(current) = session() {
            if let Some(revision) = current
                .revisions
                .iter()
                .find(|revision| revision.id == current.active_revision_id)
            {
                question.set(revision.question.clone());
            }
        }
        session.set(None);
    };
    let _regenerate_plan = generate_plan.clone();

    let catalog_for_scope_removal = catalog.clone();
    let remove_scope_item = EventHandler::new(move |index: usize| {
        let Some(current_scope) = scope() else {
            return;
        };
        let Some(next_scope) =
            scope_without_item(&current_scope, index, catalog_for_scope_removal.as_ref())
        else {
            return;
        };
        if let Some(mut current) = session() {
            if current
                .apply_working_scope_change(question(), next_scope.clone())
                .is_err()
            {
                return;
            }
            persist_session(&current, &mut recent_sessions, &mut storage_notice);
            session.set(Some(current));
        }
        scope.set(Some(next_scope));
    });

    let catalog_for_recent = catalog.clone();
    let select_recent_session = EventHandler::new(move |session_id: String| {
        if let Some(mut departing) = session() {
            if departing.id != session_id {
                departing.normalize_inflight_for_navigation();
                persist_session(&departing, &mut recent_sessions, &mut storage_notice);
            }
        }
        let Some(mut selected) = recent_sessions()
            .into_iter()
            .find(|candidate| candidate.id == session_id)
        else {
            return;
        };
        if let Some(catalog) = catalog_for_recent.as_ref() {
            selected.reconcile_catalog(&catalog.snapshot_id);
            if let Some(revision) = selected.active_revision() {
                scope.set(Some(scope_for_catalog(&revision.scope, catalog)));
            }
        }
        if let Some(revision) = selected.active_revision() {
            question.set(revision.question.clone());
        }
        let persisted = persist_session(&selected, &mut recent_sessions, &mut storage_notice);
        if let Some(session_id) = persisted_session_id(&selected.id, persisted) {
            on_session_change.call(session_id);
        }
        session.set(Some(selected));
    });

    let catalog_for_revision = catalog.clone();
    let select_revision = EventHandler::new(move |revision_id: u64| {
        let Some(mut current) = session() else {
            return;
        };
        if current.select_revision(revision_id).is_err() {
            return;
        }
        if let Some(revision) = current.active_revision() {
            question.set(revision.question.clone());
            if let Some(catalog) = catalog_for_revision.as_ref() {
                scope.set(Some(scope_for_catalog(&revision.scope, catalog)));
            } else {
                scope.set(Some(revision.scope.clone()));
            }
        }
        persist_session(&current, &mut recent_sessions, &mut storage_notice);
        session.set(Some(current));
    });

    let catalog_for_clear = catalog.clone();
    let clear_history = move |_| {
        let fingerprint = catalog_for_clear
            .as_ref()
            .map(|catalog| {
                analysis_session::storage_fingerprint(&catalog.database, &catalog.storage_path)
            })
            .or_else(|| session().map(|current| current.storage_fingerprint));
        let Some(fingerprint) = fingerprint else {
            return;
        };
        match analysis_session::clear_sessions(&fingerprint) {
            Ok(()) => {
                recent_sessions.set(Vec::new());
                session.set(None);
                question.set(String::new());
                clear_confirmation.set(false);
                storage_notice.set(Some("Analysis history cleared for these datasets.".into()));
                on_session_change.call(String::new());
            }
            Err(message) => storage_notice.set(Some(message)),
        }
    };

    let current_session_id = session().map(|current| current.id).unwrap_or_default();
    let current_revision_id = session()
        .map(|current| current.active_revision_id)
        .unwrap_or_default();
    let revision_history = session()
        .map(|current| current.revisions)
        .unwrap_or_default();
    let timeline_now_ms = current_time_millis();
    let scope_revision_state = active_revision
        .as_ref()
        .map(|revision| revision.state.clone());
    let sql_text = active_revision
        .as_ref()
        .and_then(|revision| revision.plan.as_ref())
        .map(|plan| plan.sql.clone())
        .unwrap_or_default();
    let sql_locked = active_revision.as_ref().is_some_and(|revision| {
        matches!(
            revision.state,
            RevisionState::GeneratingPlan | RevisionState::Executing
        )
    });
    let _run_enabled = view_model.as_ref().is_some_and(|model| model.run_enabled);
    let schema_tables = catalog
        .as_ref()
        .map(crate::model::queryable_tables)
        .unwrap_or_default();
    let selected_table_name = {
        let current = selected_table();
        if schema_tables.iter().any(|table| table.name == current) {
            current
        } else {
            schema_tables
                .first()
                .map(|table| table.name.clone())
                .unwrap_or_default()
        }
    };
    let selected_schema_table = schema_tables
        .iter()
        .find(|table| table.name == selected_table_name)
        .cloned();
    let page_title = session()
        .as_ref()
        .map(session_label)
        .unwrap_or_else(|| "New analysis".into());
    let table_count = schema_tables.len();

    rsx! {
        section { class: "analyze-workspace", aria_label: "Question-driven analysis workspace",
            nav { class: "analyze-schema", aria_label: "Dataset fields",
                header {
                    strong { "SQL tables" }
                    span { "{table_count}" }
                }
                if schema_tables.is_empty() {
                    p { class: "analyze-schema-empty", "Datasets are still loading." }
                } else {
                    div { class: "analyze-schema-list",
                        ul { class: "analyze-schema-tables",
                            for table in schema_tables.iter() {
                                {
                                    let table_name = table.name.clone();
                                    let selected = table_name == selected_table_name;
                                    rsx! {
                                        li {
                                            button {
                                                class: if selected { "analyze-schema-table active" } else { "analyze-schema-table" },
                                                r#type: "button",
                                                aria_pressed: selected,
                                                onclick: move |_| selected_table.set(table_name.clone()),
                                                strong { "{table.name}" }
                                                small {
                                                    if table.kind == "view" {
                                                        span {
                                                            class: "analyze-schema-kind view",
                                                            "view"
                                                        }
                                                        " · "
                                                    }
                                                    "{table.grain}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(table) = selected_schema_table.as_ref() {
                        div { class: "analyze-schema-list nested",
                            p { class: "analyze-schema-fields-label", "{table.name} fields" }
                            if !table.description.is_empty() {
                                p { class: "analyze-schema-table-copy", "{table.description}" }
                            }
                            ul {
                                for field in table.fields.iter() {
                                    {
                                        let token = field_sql_token(&table.name, &field.name);
                                        let locked = sql_locked;
                                        let data_type = field.data_type.clone();
                                        let description = field.description.clone();
                                        rsx! {
                                            li {
                                                button {
                                                    class: "analyze-schema-field",
                                                    r#type: "button",
                                                    disabled: locked,
                                                    title: if description.is_empty() { data_type.clone() } else { format!("{data_type} · {description}") },
                                                    onclick: move |_| {
                                                        let Some(scope) = scope() else { return; };
                                                        let mut current = session().unwrap_or_else(|| {
                                                            AnalysisSession::with_revision(AnalysisRevision::draft(
                                                                1,
                                                                question(),
                                                                scope.clone(),
                                                            ))
                                                        });
                                                        let Some(active) = current.active_revision_mut() else { return; };
                                                        let Ok(caret) = apply_inserted_token(active, &token, sql_textarea_cursor()) else { return; };
                                                        persist_session(&current, &mut recent_sessions, &mut storage_notice);
                                                        session.set(Some(current));
                                                        composer_tab.set(composer_tab_after_catalog_insert());
                                                        sql_caret.set(Some(caret));
                                                    },
                                                    code { "{field.name}" }
                                                    small { "{field.data_type}" }
                                                    if !field.description.is_empty() {
                                                        span { "{field.description}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "analyze-detail",
                header { class: "analyze-detail-head",
                    div {
                        p { class: "eyebrow", "Analysis" }
                        h1 { "{page_title}" }
                        p {
                            if composer_tab() == ComposerTab::Ask {
                                "Ask in plain language, or write SQL. Analysis creates a plan, generates a read-only query, and returns limited results."
                            } else {
                                "Run executes this query. Manual SQL is not repaired automatically."
                            }
                        }
                    }
                    div { class: "analyze-header-actions",
                        if !recent_sessions().is_empty() {
                            label { class: "analyze-recent-select",
                                span { "Recent" }
                                select {
                                    value: "{current_session_id}",
                                    onchange: move |event| select_recent_session.call(event.value()),
                                    for saved in recent_sessions() {
                                        option { value: "{saved.id}", "{session_label(&saved)}" }
                                    }
                                }
                            }
                        }
                        if clear_confirmation() {
                            div { class: "analyze-clear-confirmation", role: "group", aria_label: "Confirm clearing analysis history",
                                span { "Clear analysis history for these datasets?" }
                                button { class: "button", r#type: "button", onclick: clear_history, "Clear" }
                                button { class: "analyze-link-button", r#type: "button", onclick: move |_| clear_confirmation.set(false), "Cancel" }
                            }
                        } else {
                            button { class: "analyze-link-button", r#type: "button", onclick: move |_| clear_confirmation.set(true), "Clear history" }
                        }
                        button { class: "button analyze-settings-button", r#type: "button", onclick: move |_| settings_open.set(true),
                            span { aria_hidden: "true", "⚙" }
                            "Model settings"
                        }
                    }
                }
                if let Some(message) = storage_notice() {
                    p { class: "analyze-storage-notice analyze-storage-notice-inline", role: "status", "{message}" }
                }
                section { class: "analyze-composer", aria_label: "Analysis composer",
                    div { class: "analyze-composer-toolbar",
                        div { class: "analyze-composer-tabs", role: "tablist", aria_label: "Analysis input mode",
                            button {
                                class: if composer_tab() == ComposerTab::Ask { "active" } else { "" },
                                r#type: "button",
                                role: "tab",
                                aria_selected: composer_tab() == ComposerTab::Ask,
                                onclick: move |_| composer_tab.set(ComposerTab::Ask),
                                "Ask"
                            }
                            button {
                                class: if composer_tab() == ComposerTab::Sql { "active" } else { "" },
                                r#type: "button",
                                role: "tab",
                                aria_selected: composer_tab() == ComposerTab::Sql,
                                onclick: move |_| composer_tab.set(ComposerTab::Sql),
                                "Write SQL"
                            }
                        }
                        span { class: "analyze-step-state",
                            if generating { { analyze_progress_label(active_revision.as_ref()) } }
                            else if !composer.show_spec_summary && active_revision.as_ref().is_some_and(|revision| revision.manually_edited) { "Manually edited" }
                            else { "Draft" }
                        }
                        div { class: "analyze-context-row", aria_label: "Analysis context",
                            span { class: if catalog.is_some() { "analyze-status ready" } else { "analyze-status" },
                                span { aria_hidden: "true" }
                                if catalog.is_some() { "Datasets ready" } else { "Loading datasets…" }
                            }
                            span { class: "analyze-chip lock", "Read-only" }
                            if let Some(scope) = scope() {
                                for (index, item) in scope.items.iter().enumerate() {
                                    {
                                        let label = scope_item_label(item);
                                        let only_item = scope.items.len() == 1;
                                        let removal_enabled = scope_item_removal_enabled(
                                            &scope,
                                            catalog.as_ref(),
                                            scope_revision_state.as_ref(),
                                        );
                                        let blocked_by_operation = matches!(
                                            scope_revision_state.as_ref(),
                                            Some(RevisionState::GeneratingPlan | RevisionState::Executing)
                                        );
                                        let single_dataset = only_item && matches!(
                                            scope.items.first(),
                                            Some(AnalysisScopeItem::Dataset { .. })
                                        );
                                        rsx! {
                                            span { class: "analyze-chip",
                                                "{label}"
                                                button {
                                                    class: "analyze-chip-remove",
                                                    r#type: "button",
                                                    disabled: !removal_enabled,
                                                    aria_label: if removal_enabled { "Remove {label} from analysis scope" } else if blocked_by_operation { "Analysis scope cannot change while an operation is running" } else if single_dataset { "The dataset analysis scope cannot be removed" } else { "The datasets must load before this scope can be removed" },
                                                    title: if removal_enabled { "Remove scope" } else if blocked_by_operation { "Wait for the current plan or query operation to finish" } else if single_dataset { "At least one explicit scope is required" } else { "Wait for the datasets to load" },
                                                    onclick: move |_| remove_scope_item.call(index),
                                                    "×"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if composer_tab() == ComposerTab::Ask {
                            label { class: "analyze-question-label", r#for: "analysis-question", "Question" }
                            textarea {
                                id: "analysis-question",
                                class: "analyze-question-input",
                                rows: "4",
                                value: "{question}",
                                placeholder: "Ask about runs, errors, latency, tool use, or model behavior…",
                                disabled: generating,
                                oninput: move |event| question.set(event.value()),
                            }
                            div { class: "analyze-starters", aria_label: "Question starters",
                                span { "Try a starting point" }
                                div {
                                    for starter in QUESTION_STARTERS {
                                        button { r#type: "button", disabled: generating, onclick: move |_| question.set(starter.into()), "{starter}" }
                                    }
                                }
                            }
                            if !config().is_configured() {
                                div { class: "analyze-config-callout", role: "status",
                                    div { strong { "Connect a model for Analysis" } p { "Your draft stays here while you configure the endpoint." } }
                                    button { class: "button", r#type: "button", onclick: move |_| settings_open.set(true), "Open model settings" }
                                }
                            }
                            if let Some(revision) = active_revision.as_ref() {
                                if revision.state == RevisionState::PlanError {
                                    if let Some(notice) = notice_from_revision(revision) {
                                        div { class: "analyze-error-host",
                                            ErrorNotice { notice, on_dismiss: None }
                                            p { "Your question is unchanged. Adjust it or Analyze again." }
                                        }
                                    } else {
                                        div { class: "analyze-error", role: "alert",
                                            strong { "The analysis plan could not be created" }
                                            if let Some(message) = revision.error.as_ref() { p { "{message}" } }
                                            else { p { "The model did not return a valid analysis plan." } }
                                            p { "Your question is unchanged. Adjust it or Analyze again." }
                                        }
                                    }
                                }
                            }
                            if view_model.as_ref().is_some_and(|model| model.question_out_of_date) {
                                div { class: "analyze-config-callout", role: "status",
                                    div {
                                        strong { "This plan is for the previous question" }
                                        p { "Analyze again for the current question, or restore the reviewed question." }
                                    }
                                }
                            }
                            div { class: "analyze-question-actions",
                                p { "Analysis creates a plan, generates a read-only query, and returns limited results." }
                                button { class: "button primary", r#type: "button", disabled: !composer.primary_enabled, onclick: generate_plan,
                                    if generating {
                                        span { class: "analyze-spinner", aria_hidden: "true" }
                                        { analyze_progress_label(active_revision.as_ref()) }
                                    } else { "{composer.primary_label}" }
                                }
                            }
                        } else {
                            label { class: "analyze-question-label", r#for: "analysis-sql", "SQL" }
                            textarea {
                                id: "analysis-sql",
                                class: "analyze-sql-editor",
                                rows: "6",
                                value: "{sql_text}",
                                placeholder: "SELECT …",
                                disabled: sql_locked,
                                oninput: move |event| {
                                    let Some(scope) = scope() else { return; };
                                    let mut current = session().unwrap_or_else(|| {
                                        AnalysisSession::with_revision(AnalysisRevision::draft(
                                            1,
                                            question(),
                                            scope.clone(),
                                        ))
                                    });
                                    let Some(active) = current.active_revision_mut() else { return; };
                                    if apply_manual_sql(active, event.value()).is_err() {
                                        return;
                                    }
                                    persist_session(&current, &mut recent_sessions, &mut storage_notice);
                                    session.set(Some(current));
                                },
                            }
                            if let Some(revision) = active_revision.as_ref() {
                                if revision.state == RevisionState::QueryError {
                                    if let Some(notice) = notice_from_revision(revision) {
                                        div { class: "analyze-error-host",
                                            ErrorNotice { notice, on_dismiss: None }
                                            p { "Fix the SQL and Run. Analyze will not repair a handwritten query." }
                                        }
                                    } else if let Some(error) = revision.error.as_ref() {
                                        div { class: "analyze-error", role: "alert", strong { "Analysis could not run" } p { "{error}" } p { "Fix the SQL and Run. Analyze will not repair a handwritten query." } }
                                    }
                                }
                                if revision.needs_rerun {
                                    div { class: "analyze-config-callout", role: "status",
                                        div { strong { "Rerun to restore rows" } p { "Saved summaries remain visible, but result rows are never stored in browser history." } }
                                    }
                                }
                            }
                            div { class: "analyze-question-actions",
                                p { "Run executes this query. Manual SQL is not repaired automatically." }
                                button { class: "button primary", r#type: "button", disabled: !composer.primary_enabled, onclick: run_sql,
                                    if generating {
                                        span { class: "analyze-spinner", aria_hidden: "true" }
                                        { analyze_progress_label(active_revision.as_ref()) }
                                    } else { "{composer.primary_label}" }
                                }
                            }
                        }
                    }

                    div { class: "analyze-stage",
                    details {
                        class: "analyze-sql-card",
                        aria_label: "Analysis process",
                        open: generating || analyze_trace_open(),
                        summary {
                            onclick: move |event| {
                                event.prevent_default();
                                if !generating {
                                    analyze_trace_open.set(!analyze_trace_open());
                                }
                            },
                            div { class: "analyze-section-heading",
                                div { h2 { "Analysis process" } p { "Plan, generated SQL, query execution, and summary." } }
                            }
                        }
                        if let Some(revision) = active_revision.as_ref() {
                            if revision.trace.is_empty() {
                                p { class: "analyze-trace-empty", "Analyze or Run to view each processing step." }
                            } else {
                                AnalyzeTraceView { steps: revision.trace.clone() }
                            }
                        } else {
                            p { class: "analyze-trace-empty", "Analyze or Run to view each processing step." }
                        }
                    }

                    if !revision_history.is_empty() {
                        nav { class: "analyze-revision-timeline", aria_label: "Analysis version history",
                            for revision in revision_history {
                                button {
                                    class: if revision.id == current_revision_id { "analyze-revision active" } else { "analyze-revision" },
                                    r#type: "button",
                                    onclick: move |_| select_revision.call(revision.id),
                                    span { class: "analyze-revision-marker", aria_hidden: "true" }
                                    span { class: "analyze-revision-copy",
                                        strong { "{revision_heading(&revision)}" }
                                        small { "{revision_state_label(&revision.state)} · {relative_time_label(revision.updated_at_ms, timeline_now_ms)} · {revision_row_label(&revision)}" }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(revision) = active_revision.as_ref() {
                        if composer.show_spec_summary {
                            if let Some(spec) = revision.spec.as_ref() {
                            section { class: "analyze-plan-card", aria_label: "Analysis plan",
                                div { class: "analyze-section-heading",
                                    div { h2 { "Analysis plan" } p { "The generated SQL comes from this plan. Analyze repairs the plan when needed." } }
                                }
                                dl { class: "analyze-plan-summary",
                                    div { dt { "Intent" } dd { "{spec.intent}" } }
                                    div { dt { "One row per" } dd { "{spec.grain}" } }
                                    div { dt { "Measure" } dd { "{spec.measure}" } }
                                    if let Some(dimension) = spec.dimension.as_ref() {
                                        div { dt { "Group by" } dd { "{dimension}" } }
                                    }
                                    div { dt { "Output" } dd { "{spec.output}" } }
                                }
                                if !spec.assumptions.is_empty() {
                                    div { class: "analyze-warnings", role: "note", strong { "Assumptions" } ul { for assumption in &spec.assumptions { li { "{assumption}" } } } }
                                }
                            }
                            }
                        } else if let Some(plan) = revision.plan.as_ref() {
                            if !revision.manually_edited && shows_plan_summary(plan) {
                            section { class: "analyze-plan-card", aria_label: "Proposed analysis plan",
                                div { class: "analyze-section-heading",
                                    div { h2 { "Review the analysis plan" } p { "This saved SQL is out of date. Analyze again to create a new plan." } }
                                }
                                dl { class: "analyze-plan-summary",
                                    div { dt { "Intent" } dd { "{plan.intent_summary}" } }
                                    div { dt { "Scope" } dd { "{plan.scope_summary}" } }
                                    PlanListRow { label: "Filters", values: plan.filters.clone() }
                                    PlanListRow { label: "Grouping", values: plan.groupings.clone() }
                                    PlanListRow { label: "Measures", values: plan.measures.clone() }
                                }
                            }
                            }
                        }

                        if let Some(evidence) = revision.evidence.clone() {
                            section { class: "analyze-result-card", aria_label: "Analysis result",
                                div { class: "analyze-section-heading", div { h2 { "Analysis results" } p { "Limited results returned by the confirmed query." } } }
                                if evidence.rows.is_empty() {
                                    div { class: "analyze-empty-result",
                                        div {
                                            strong { "No rows matched this plan" }
                                            p { "Rewrite the question or broaden the plan before trying again." }
                                        }
                                        button { class: "button", r#type: "button", onclick: rewrite_problem, "Rewrite question" }
                                    }
                                } else {
                                    ResultExplorer {
                                        evidence: evidence.clone(),
                                        profiles: revision.execution.as_ref().map(|execution| execution.profiles.clone()).unwrap_or_default(),
                                        revision_id: revision.id,
                                        refinement_enabled: refinement_plan_allowed(
                                            revision.id,
                                            revision.id,
                                            &draft_question,
                                            &revision.question,
                                        ),
                                        on_stage_filter: move |_| {},
                                        on_prepare_refinement: prepare_refinement,
                                    }
                                    if revision.state == RevisionState::Interpreting {
                                        div { class: "analyze-interpretation-status", role: "status",
                                            span { class: "analyze-spinner", aria_hidden: "true" }
                                            div { strong { "Summarizing the returned results…" } p { "Results remain available while the model prepares a summary tied to the returned rows." } }
                                        }
                                    }
                                    if revision.state == RevisionState::InterpretationError {
                                        div { class: "analyze-interpretation-error", role: "alert",
                                            div {
                                                strong { "The results could not be summarized" }
                                                if let Some(message) = revision.error.as_ref() { p { "{message}" } }
                                                p { "Returned rows and profiles are preserved. Retrying does not rerun SQL." }
                                            }
                                            button { class: "button", r#type: "button", onclick: retry_interpretation, "Retry interpretation" }
                                        }
                                    }
                                    if let Some(interpretation) = revision.interpretation.clone() {
                                        InterpretationPanel {
                                            interpretation,
                                            follow_up_enabled: config().is_configured() && follow_up_plan_allowed(
                                                revision.id,
                                                revision.id,
                                                &draft_question,
                                                &revision.question,
                                            ),
                                            on_follow_up: generate_follow_up,
                                            on_edit_follow_up: edit_follow_up,
                                        }
                                    }
                                }
                            }
                        } else if let Some(interpretation) = revision.interpretation.clone() {
                            section { class: "analyze-result-card", aria_label: "Saved analysis interpretation",
                                div { class: "analyze-section-heading", div { h2 { "Saved interpretation" } p { "The summary was restored from this analysis session." } } }
                                div { class: "analyze-saved-interpretation-note", role: "note",
                                    strong { "Returned rows are not stored in the browser" }
                                    p { "This saved interpretation remains available. Rerun to restore rows in Result Explorer." }
                                }
                                InterpretationPanel {
                                    interpretation,
                                    follow_up_enabled: config().is_configured() && follow_up_plan_allowed(
                                        revision.id,
                                        revision.id,
                                        &draft_question,
                                        &revision.question,
                                    ),
                                    on_follow_up: generate_follow_up,
                                    on_edit_follow_up: edit_follow_up,
                                }
                            }
                        }
                    }
                    }
                }
            }
        }
        if settings_open() {
            LlmSettings {
                config: config(),
                on_close: move |_| settings_open.set(false),
                on_save: move |value| {
                    llm::save_config(&value);
                    config.set(value);
                    settings_open.set(false);
                },
            }
        }
    }
}

fn persist_session(
    session: &AnalysisSession,
    recent_sessions: &mut Signal<Vec<AnalysisSession>>,
    storage_notice: &mut Signal<Option<String>>,
) -> bool {
    let mut persisted_session = session.clone();
    persisted_session.mark_updated();
    let mut sessions = match analysis_session::load_sessions(&persisted_session.storage_fingerprint)
    {
        Ok(sessions) => sessions,
        Err(message) => {
            recent_sessions.set(vec![persisted_session]);
            storage_notice.set(Some(message));
            return false;
        }
    };
    sessions.retain(|saved| saved.id != persisted_session.id);
    sessions.push(persisted_session.clone());
    analysis_session::trim_sessions(&mut sessions);
    let persisted = if let Err(message) =
        analysis_session::save_sessions(&persisted_session.storage_fingerprint, &sessions)
    {
        storage_notice.set(Some(message));
        false
    } else {
        true
    };
    recent_sessions.set(sessions);
    persisted
}

async fn run_spec_compile_execute(
    mut request: PlanRequest,
    expected_session_id: String,
    revision_id: u64,
    operation_id: AnalysisOperationId,
    mut session: Signal<Option<AnalysisSession>>,
    mut recent_sessions: Signal<Vec<AnalysisSession>>,
    mut storage_notice: Signal<Option<String>>,
) {
    let interpretation_config = request.config.clone();
    loop {
        let result = analysis_agent::generate_spec(request.clone()).await;
        let Some(mut current) = session() else {
            return;
        };
        if current.id != expected_session_id {
            return;
        }
        let Some(revision) = revision_for_callback(&mut current, &expected_session_id, revision_id)
        else {
            return;
        };
        let spec = match result {
            Ok(spec) => spec,
            Err(error) => {
                let _ = revision.fail_plan(revision_id, operation_id, error.message);
                persist_session(&current, &mut recent_sessions, &mut storage_notice);
                session.set(Some(current));
                return;
            }
        };
        revision.note_spec_ready(&spec);
        request.previous_spec = Some(spec.clone());
        let snapshot_id = revision.scope.snapshot_id.clone();
        let scope = revision.scope.clone();
        persist_session(&current, &mut recent_sessions, &mut storage_notice);
        session.set(Some(current));
        let compiled = api::compile_analysis(&spec, &snapshot_id, &scope).await;
        let Some(mut current) = session() else {
            return;
        };
        if current.id != expected_session_id {
            return;
        }
        let Some(revision) = revision_for_callback(&mut current, &expected_session_id, revision_id)
        else {
            return;
        };
        match compiled {
            Ok(compiled) => {
                let effect = revision.finish_compiled_spec(
                    revision_id,
                    operation_id,
                    compiled.spec,
                    compiled.sql,
                );
                let pending = match effect {
                    Ok(effect) => effect.or_else(|| revision.take_pending_effect()),
                    Err(message) => {
                        let _ = revision.fail_plan(revision_id, operation_id, message);
                        persist_session(&current, &mut recent_sessions, &mut storage_notice);
                        session.set(Some(current));
                        return;
                    }
                };
                persist_session(&current, &mut recent_sessions, &mut storage_notice);
                session.set(Some(current));
                let Some(AnalysisEffect::ExecuteSql {
                    revision_id,
                    operation_id,
                    sql,
                }) = pending
                else {
                    return;
                };
                let query_result = api::query_evidence_interactive(&sql).await;
                let Some(mut current) = session() else {
                    return;
                };
                if current.id != expected_session_id {
                    return;
                }
                let Some(revision) =
                    revision_for_callback(&mut current, &expected_session_id, revision_id)
                else {
                    return;
                };
                let prepared = match query_result {
                    Ok(evidence) => {
                        let profiles = profile_rows(&evidence.rows);
                        finish_query_for_interpretation(
                            revision,
                            revision_id,
                            operation_id,
                            evidence,
                            profiles,
                        )
                        .ok()
                        .flatten()
                    }
                    Err(failure) => {
                        let _ = revision.fail_query(revision_id, operation_id, failure.to_string());
                        record_api_failure(revision, &failure);
                        None
                    }
                };
                persist_session(&current, &mut recent_sessions, &mut storage_notice);
                session.set(Some(current));
                if let Some(prepared) = prepared {
                    launch_interpretation(
                        interpretation_config,
                        expected_session_id,
                        prepared,
                        session,
                        recent_sessions,
                        storage_notice,
                    );
                }
                return;
            }
            Err(failure) => {
                let summary = failure.summary();
                let _ = revision.fail_compile(
                    revision_id,
                    operation_id,
                    compile_error_text(&failure),
                );
                record_compile_failure(revision, &failure);
                let stopped = revision.state == RevisionState::PlanError;
                persist_session(&current, &mut recent_sessions, &mut storage_notice);
                session.set(Some(current));
                if stopped {
                    return;
                }
                request.compile_error = Some(summary);
            }
        }
    }
}

fn persisted_session_id(session_id: &str, persisted: bool) -> Option<String> {
    persisted.then(|| session_id.to_string())
}

fn scope_for_catalog(scope: &AnalysisScope, catalog: &QueryCatalog) -> AnalysisScope {
    AnalysisScope {
        database: catalog.database.clone(),
        storage_path: catalog.storage_path.clone(),
        snapshot_id: catalog.snapshot_id.clone(),
        items: scope.items.clone(),
    }
}

fn refinement_source_revision_id(refinement: &AnalysisRefinement) -> u64 {
    match refinement {
        AnalysisRefinement::Filter { intent } => intent.source_revision_id,
        AnalysisRefinement::FullProfile {
            source_revision_id, ..
        } => *source_revision_id,
    }
}

fn refinement_plan_allowed(
    source_revision_id: u64,
    refinement_source_revision_id: u64,
    draft_question: &str,
    source_question: &str,
) -> bool {
    refinement_source_revision_id == source_revision_id
        && draft_question.trim() == source_question.trim()
}

fn scope_item_label(item: &AnalysisScopeItem) -> String {
    match item {
        AnalysisScopeItem::Dataset { name } => format!("Dataset · {name}"),
        AnalysisScopeItem::Root {
            dataset,
            root_session_id,
            ..
        } => format!("Root · {dataset} / {root_session_id}"),
        AnalysisScopeItem::Run { run } => {
            let mut coordinates = vec![
                run.dataset.as_str(),
                run.file.as_str(),
                run.agent_id.as_str(),
            ];
            if let Some(root) = run.root_session_id.as_deref() {
                coordinates.push(root);
            }
            coordinates.push(run.session_id.as_str());
            if let Some(run_id) = run.run_id.as_deref() {
                coordinates.push(run_id);
            }
            format!("Run · {}", coordinates.join(" / "))
        }
    }
}

fn first_sql_line(sql: &str) -> Option<&str> {
    sql.lines().map(str::trim).find(|line| !line.is_empty())
}

fn revision_heading(revision: &AnalysisRevision) -> String {
    let question = revision.question.trim();
    if !question.is_empty() {
        return question.into();
    }
    revision
        .plan
        .as_ref()
        .and_then(|plan| first_sql_line(&plan.sql))
        .unwrap_or("Draft")
        .into()
}

fn session_label(session: &AnalysisSession) -> String {
    let title = session.title.trim();
    if !title.is_empty() {
        return title.into();
    }
    let Some(revision) = session.active_revision() else {
        return "New analysis".into();
    };
    let question = revision.question.trim();
    if !question.is_empty() {
        return question.into();
    }
    revision
        .plan
        .as_ref()
        .and_then(|plan| first_sql_line(&plan.sql).map(str::to_string))
        .unwrap_or_else(|| "New analysis".into())
}

fn shows_plan_summary(plan: &AnalysisPlan) -> bool {
    plan.intent_summary != "Manual SQL"
        || !plan.filters.is_empty()
        || !plan.groupings.is_empty()
        || !plan.measures.is_empty()
        || !plan.warnings.is_empty()
}

fn quote_sql_ident(name: &str) -> String {
    let is_plain = name
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if is_plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

fn field_sql_token(table: &str, field: &str) -> String {
    let table = table
        .split('.')
        .map(quote_sql_ident)
        .collect::<Vec<_>>()
        .join(".");
    format!("{table}.{}", quote_sql_ident(field))
}

fn sql_textarea_cursor() -> usize {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("analysis-sql"))
        .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .and_then(|textarea| textarea.selection_start().ok().flatten())
        .unwrap_or(0) as usize
}

fn set_sql_textarea_cursor(index: usize) {
    let Some(textarea) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("analysis-sql"))
        .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    else {
        return;
    };
    let index = index as u32;
    let _ = textarea.set_selection_start(Some(index));
    let _ = textarea.set_selection_end(Some(index));
}

fn revision_state_label(state: &RevisionState) -> &'static str {
    match state {
        RevisionState::Draft => "Draft",
        RevisionState::GeneratingPlan => "Creating plan",
        RevisionState::PlanReady => "Plan ready",
        RevisionState::Executing => "Executing",
        RevisionState::Interpreting => "Interpreting",
        RevisionState::Complete => "Complete",
        RevisionState::PlanError => "Plan error",
        RevisionState::QueryError => "Rerun required",
        RevisionState::InterpretationError => "Interpretation error",
        RevisionState::Stale => "Stale",
    }
}

fn analyze_progress_label(revision: Option<&AnalysisRevision>) -> &'static str {
    if let Some(step) = revision.and_then(|revision| {
        revision
            .trace
            .iter()
            .rev()
            .find(|step| step.status == AnalyzeTraceStatus::Running)
    }) {
        return match step.kind {
            AnalyzeTraceKind::GenerateSpec => "Creating plan…",
            AnalyzeTraceKind::RepairSpec => "Repairing plan…",
            AnalyzeTraceKind::Compile => "Compiling SQL…",
            AnalyzeTraceKind::Execute => "Executing…",
            AnalyzeTraceKind::Interpret => "Interpreting…",
        };
    }
    match revision.map(|revision| &revision.state) {
        Some(RevisionState::GeneratingPlan)
            if revision.is_some_and(|revision| revision.repair_count > 0) =>
        {
            "Repairing plan…"
        }
        Some(RevisionState::GeneratingPlan) => "Creating plan…",
        Some(RevisionState::Executing) => "Executing…",
        Some(RevisionState::Interpreting) => "Interpreting…",
        _ => "Analyzing…",
    }
}

fn trace_step_preview(step: &AnalyzeTraceStep) -> String {
    if let Some(error) = &step.error {
        return error.clone();
    }
    if let Some(output) = &step.output {
        return output
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(output)
            .chars()
            .take(140)
            .collect();
    }
    step.prompt
        .as_deref()
        .unwrap_or("In progress")
        .lines()
        .next()
        .unwrap_or("In progress")
        .chars()
        .take(140)
        .collect()
}

fn trace_status_label(status: AnalyzeTraceStatus) -> &'static str {
    match status {
        AnalyzeTraceStatus::Pending => "pending",
        AnalyzeTraceStatus::Running => "running",
        AnalyzeTraceStatus::Ok => "ok",
        AnalyzeTraceStatus::Error => "error",
    }
}

#[component]
fn AnalyzeTraceView(steps: Vec<AnalyzeTraceStep>) -> Element {
    let mut open_id = use_signal(|| None::<u64>);
    let axis_len = steps.len().max(1);
    let bar_width = 100.0 / axis_len as f64;
    rsx! {
        div { class: "analyze-trace-surface",
            div { class: "span-table",
                div { class: "span-table-head",
                    div { "Structure" }
                    div { "Overview" }
                    div { class: "span-axis-head", span { "Sequence" } }
                    div { "Status" }
                }
                for (index, step) in steps.iter().enumerate() {
                    {
                        let step = step.clone();
                        let row_open = open_id() == Some(step.id) || step.status == AnalyzeTraceStatus::Running;
                        let phase = step.kind.phase();
                        let title = step.kind.title();
                        let preview = trace_step_preview(&step);
                        let status = trace_status_label(step.status);
                        let left = index as f64 * bar_width;
                        let step_id = step.id;
                        rsx! {
                            details {
                                key: "trace-{step_id}",
                                class: if row_open { "span-row is-open" } else { "span-row" },
                                open: row_open,
                                summary {
                                    class: "span-row-summary",
                                    onclick: move |event| {
                                        event.prevent_default();
                                        open_id.set(if open_id() == Some(step_id) { None } else { Some(step_id) });
                                    },
                                    div { class: "span-structure",
                                        span { class: "disclosure" }
                                        div {
                                            div { class: "span-structure-title",
                                                strong { "{title}" }
                                                span { class: "phase-badge {phase}", "{phase}" }
                                                if step.status == AnalyzeTraceStatus::Error {
                                                    span { class: "pc2-error-chip", "error" }
                                                }
                                            }
                                            span { "step {index + 1} of {axis_len}" }
                                        }
                                    }
                                    div { class: "span-row-copy",
                                        strong { class: "overview-line", title: "{preview}", "{preview}" }
                                    }
                                    div { class: "span-seq-cell",
                                        div { class: "span-track", title: "{title}",
                                            div { class: "span-grid-lines" }
                                            div {
                                                class: "span-bar {phase} analyze-trace-bar {status}",
                                                style: format!("left: {left:.2}%; width: {bar_width:.2}%"),
                                            }
                                        }
                                    }
                                    div { class: "span-evidence-count",
                                        strong { "{status}" }
                                        span { "{phase}" }
                                    }
                                }
                                if row_open {
                                    div { class: "span-detail analyze-trace-detail",
                                        if let Some(prompt) = step.prompt.as_ref() {
                                            div {
                                                strong { "Prompt" }
                                                pre { "{prompt}" }
                                            }
                                        }
                                        if let Some(output) = step.output.as_ref() {
                                            div {
                                                strong { "Result" }
                                                pre { "{output}" }
                                            }
                                        }
                                        if let Some(error) = step.error.as_ref() {
                                            div {
                                                strong { "Error" }
                                                pre { "{error}" }
                                            }
                                        }
                                        if step.prompt.is_none() && step.output.is_none() && step.error.is_none() {
                                            p { "This step is still running." }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn revision_row_label(revision: &AnalysisRevision) -> String {
    revision
        .execution
        .as_ref()
        .map(|execution| format!("{} rows", execution.returned_rows))
        .unwrap_or_else(|| "Not run".into())
}

fn current_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or_default()
}

fn relative_time_label(updated_at_ms: u64, now_ms: u64) -> String {
    let elapsed_seconds = now_ms.saturating_sub(updated_at_ms) / 1_000;
    if elapsed_seconds < 60 {
        "Just now".into()
    } else if elapsed_seconds < 60 * 60 {
        format!("{} min ago", elapsed_seconds / 60)
    } else if elapsed_seconds < 24 * 60 * 60 {
        format!("{} hr ago", elapsed_seconds / (60 * 60))
    } else {
        format!("{} days ago", elapsed_seconds / (24 * 60 * 60))
    }
}

#[component]
fn PlanListRow(label: &'static str, values: Vec<String>) -> Element {
    rsx! {
        div {
            dt { "{label}" }
            dd {
                if values.is_empty() {
                    span { class: "analyze-none", "None" }
                } else {
                    ul { for value in values { li { "{value}" } } }
                }
            }
        }
    }
}

#[component]
fn InterpretationPanel(
    interpretation: AnalysisInterpretation,
    follow_up_enabled: bool,
    on_follow_up: EventHandler<String>,
    on_edit_follow_up: EventHandler<String>,
) -> Element {
    rsx! {
        section { class: "analyze-interpretation", aria_label: "Result summary",
            div { class: "analyze-interpretation-grid",
                section { class: "analyze-interpretation-block observed",
                    h3 { "Observed in this result" }
                    if interpretation.observations.is_empty() {
                        p { class: "analyze-none", "No direct observations were returned." }
                    } else {
                        ul { for observation in &interpretation.observations { li { "{observation}" } } }
                    }
                    if !interpretation.references.is_empty() {
                        div { class: "analyze-interpretation-references", aria_label: "Supporting result rows",
                            for reference in &interpretation.references {
                                if let Some(identity) = interpretation_reference_identity(reference) {
                                    span { class: "analyze-interpretation-reference linked",
                                        span { "{reference.label}" }
                                        a { href: "{identity.run_href}", "Run" }
                                        if let Some(turn_href) = identity.turn_href { a { href: "{turn_href}", "Step" } }
                                    }
                                } else {
                                    span { class: "analyze-interpretation-reference", "{reference.label}" }
                                }
                            }
                        }
                    }
                }
                section { class: "analyze-interpretation-block inferred",
                    h3 { "Possible explanation" }
                    if interpretation.inferences.is_empty() {
                        p { class: "analyze-none", "No inference was offered from these results." }
                    } else {
                        ul { for inference in &interpretation.inferences { li { "{inference}" } } }
                    }
                }
                section { class: "analyze-interpretation-block limitations",
                    h3 { "Coverage and limitations" }
                    if interpretation.limitations.is_empty() {
                        p { class: "analyze-none", "No additional limitations were reported." }
                    } else {
                        ul { for limitation in &interpretation.limitations { li { "{limitation}" } } }
                    }
                }
                section { class: "analyze-interpretation-block follow-ups",
                    h3 { "Continue investigating" }
                    if !follow_up_enabled && !interpretation.follow_ups.is_empty() {
                        p { class: "analyze-follow-up-stale", role: "status", "Follow-up planning is paused because the draft question changed. Restore the reviewed question or generate the edited draft." }
                    }
                    if interpretation.follow_ups.is_empty() {
                        p { class: "analyze-none", "No follow-up questions were suggested." }
                    } else {
                        div { class: "analyze-follow-up-list",
                            for (index, follow_up) in interpretation.follow_ups.iter().enumerate() {
                                div { class: "analyze-follow-up", key: "follow-up-{index}",
                                    p { "{follow_up}" }
                                    div {
                                        button {
                                            class: "button primary",
                                            r#type: "button",
                                            disabled: !follow_up_enabled,
                                            onclick: {
                                                let follow_up = follow_up.clone();
                                                move |_| on_follow_up.call(follow_up.clone())
                                            },
                                            "Analyze"
                                        }
                                        button {
                                            class: "analyze-link-button",
                                            r#type: "button",
                                            disabled: !follow_up_enabled,
                                            onclick: {
                                                let follow_up = follow_up.clone();
                                                move |_| on_edit_follow_up.call(follow_up.clone())
                                            },
                                            "Edit question"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis_session::{
        AnalysisEffect, AnalysisPlan, AnalysisRevision, AnalysisScope, AnalysisScopeItem,
        AnalysisSpec, EvidenceReference, RevisionState, SuggestedView,
    };
    use crate::model::{QueryEvidence, RunSummary};

    fn plan_ready_revision() -> AnalysisRevision {
        let scope = AnalysisScope {
            database: "default".into(),
            storage_path: "/tmp/evidence".into(),
            snapshot_id: "snapshot-1".into(),
            items: vec![AnalysisScopeItem::Dataset {
                name: "default".into(),
            }],
        };
        let mut revision = AnalysisRevision::draft(7, "Compare run outcomes", scope);
        let operation_id = revision.begin_plan_generation().unwrap();
        let effect = revision
            .finish_plan(
                7,
                operation_id,
                AnalysisPlan {
                    id: 7,
                    question: "Compare run outcomes".into(),
                    intent_summary: "Compare successful and failed runs".into(),
                    scope_summary: "The selected dataset".into(),
                    filters: vec!["Current snapshot".into()],
                    groupings: vec!["status".into()],
                    measures: vec!["run count".into()],
                    expected_columns: vec!["status".into(), "run_count".into()],
                    suggested_view: SuggestedView::Table,
                    sql: "SELECT status, COUNT(*) AS run_count FROM default.runs GROUP BY status"
                        .into(),
                    warnings: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(revision.state, RevisionState::PlanReady);
        assert!(effect.is_none());
        revision
    }

    #[test]
    fn plan_ready_exposes_run_but_never_auto_runs() {
        let revision = plan_ready_revision();
        let model = AnalysisViewModel::from_revision(&revision, &revision.question);

        assert_eq!(model.primary_action, PrimaryAction::Analyze);
        assert!(!model.query_in_flight);
        assert_eq!(model.sql_disclosure_label, "Compiled SQL");
        assert!(!model.trace_open);
        assert!(revision.pending_effect.is_none());
    }

    fn compiled_spec() -> AnalysisSpec {
        AnalysisSpec {
            intent: "composition".into(),
            grain: "run".into(),
            measure: "step_count".into(),
            dimension: Some("agent_model_name".into()),
            filters: Vec::new(),
            ranking: None,
            output: "table".into(),
            assumptions: Vec::new(),
            identity_columns: Vec::new(),
            uncomputable_reason: None,
        }
    }

    #[test]
    fn ask_tab_analyzes_even_when_sql_is_already_runnable() {
        let mut revision = plan_ready_revision();
        revision.spec = Some(compiled_spec());
        let composer = ComposerModel::from_context(
            ComposerTab::Ask,
            Some(&revision),
            &revision.question,
            false,
            true,
            true,
            true,
        );
        assert_eq!(composer.primary_label, "Analyze");
        assert!(composer.primary_enabled);
        assert!(!composer.submits_sql);
        assert!(composer.show_spec_summary);
    }

    #[test]
    fn sql_tab_runs_the_shared_query_and_hides_spec_after_edits() {
        let mut revision = plan_ready_revision();
        revision.spec = Some(compiled_spec());
        let compiled = ComposerModel::from_context(
            ComposerTab::Sql,
            Some(&revision),
            &revision.question,
            false,
            true,
            true,
            true,
        );
        assert_eq!(compiled.primary_label, "Run");
        assert!(compiled.primary_enabled);
        assert!(compiled.submits_sql);
        assert!(compiled.show_spec_summary);

        apply_manual_sql(&mut revision, "SELECT 1".into()).unwrap();
        let edited = ComposerModel::from_context(
            ComposerTab::Sql,
            Some(&revision),
            &revision.question,
            false,
            true,
            true,
            true,
        );
        assert!(edited.primary_enabled);
        assert!(!edited.show_spec_summary);
    }

    #[test]
    fn composer_disables_empty_ask_or_empty_sql() {
        let draft = empty_draft();
        let ask = ComposerModel::from_context(
            ComposerTab::Ask,
            Some(&draft),
            "",
            false,
            true,
            true,
            true,
        );
        assert!(!ask.primary_enabled);
        let sql = ComposerModel::from_context(
            ComposerTab::Sql,
            Some(&draft),
            "Count tool calls",
            false,
            true,
            true,
            true,
        );
        assert!(!sql.primary_enabled);
    }

    #[test]
    fn catalog_insert_switches_to_the_sql_tab() {
        assert_eq!(composer_tab_after_catalog_insert(), ComposerTab::Sql);
    }

    #[test]
    fn generating_revision_opens_analyze_trace() {
        let mut revision = empty_draft();
        revision.begin_plan_generation().unwrap();
        let model = AnalysisViewModel::from_revision(&revision, &revision.question);
        assert!(model.trace_open);
        assert!(model.query_in_flight);
        assert_eq!(analyze_progress_label(Some(&revision)), "Creating plan…");
        assert_eq!(revision.trace[0].kind.title(), "Create plan");
    }

    #[test]
    fn manual_sql_is_marked_and_still_waits_for_run() {
        let mut revision = plan_ready_revision();

        apply_manual_sql(&mut revision, "SELECT 1".into()).unwrap();

        let model = AnalysisViewModel::from_revision(&revision, &revision.question);
        assert!(model.manually_edited);
        assert_eq!(revision.state, RevisionState::PlanReady);
        assert!(revision.pending_effect.is_none());
    }

    fn empty_draft() -> AnalysisRevision {
        AnalysisRevision::draft(
            1,
            "",
            AnalysisScope {
                database: "default".into(),
                storage_path: "/tmp/evidence".into(),
                snapshot_id: "snapshot-1".into(),
                items: vec![AnalysisScopeItem::Dataset {
                    name: "default".into(),
                }],
            },
        )
    }

    #[test]
    fn draft_sql_enables_run_without_a_copilot_plan() {
        let mut revision = empty_draft();
        apply_manual_sql(&mut revision, "SELECT status, COUNT(*) FROM runs".into()).unwrap();

        let model = AnalysisViewModel::from_revision(&revision, "");
        assert!(revision.manually_edited);
        assert_eq!(revision.state, RevisionState::PlanReady);
        assert_eq!(
            revision.plan.as_ref().map(|plan| plan.sql.as_str()),
            Some("SELECT status, COUNT(*) FROM runs")
        );
        assert!(model.run_enabled);
        assert!(revision.pending_effect.is_none());
    }

    #[test]
    fn empty_sql_cannot_run() {
        let mut revision = empty_draft();
        apply_manual_sql(&mut revision, "   ".into()).unwrap();
        let model = AnalysisViewModel::from_revision(&revision, "");
        assert!(!model.run_enabled);
    }

    #[test]
    fn manual_sql_can_be_edited_after_a_finished_run() {
        for state in [RevisionState::Complete, RevisionState::InterpretationError] {
            let mut revision = plan_ready_revision();
            revision.state = state;
            apply_manual_sql(&mut revision, "SELECT 2".into()).unwrap();
            let model = AnalysisViewModel::from_revision(&revision, &revision.question);
            assert_eq!(revision.state, RevisionState::PlanReady);
            assert!(model.run_enabled);
            assert!(model.manually_edited);
        }
    }

    #[test]
    fn manual_sql_can_run_after_the_question_changes() {
        let mut revision = empty_draft();
        apply_manual_sql(&mut revision, "SELECT 1".into()).unwrap();
        let model = AnalysisViewModel::from_revision(&revision, "a later question");
        assert!(model.run_enabled);
        assert!(!model.question_out_of_date);
    }

    #[test]
    fn insert_sql_token_lands_at_the_cursor() {
        assert_eq!(insert_sql_token("SELECT ", 7, "status"), "SELECT status");
        assert_eq!(insert_sql_token("", 0, "runs.status"), "runs.status");
        assert_eq!(
            insert_sql_token("SELECT  FROM runs", 7, "status"),
            "SELECT status FROM runs"
        );
    }

    #[test]
    fn field_sql_token_uses_dataset_qualified_table_names() {
        assert_eq!(field_sql_token("atif.runs", "status"), "atif.runs.status");
        assert_eq!(field_sql_token("atif.runs", "_file_"), "atif.runs._file_");
    }

    #[test]
    fn session_label_uses_manual_sql_when_question_is_empty() {
        let mut revision = empty_draft();
        apply_manual_sql(&mut revision, "SELECT status FROM runs\nLIMIT 10".into()).unwrap();
        let mut session = AnalysisSession::with_revision(revision);
        session.title.clear();
        assert_eq!(session_label(&session), "SELECT status FROM runs");
    }

    #[test]
    fn history_labels_expose_state_and_row_count() {
        let revision = plan_ready_revision();
        let mut session = AnalysisSession::with_revision(revision);
        session.title.clear();
        let empty = AnalysisSession::with_revision(AnalysisRevision::draft(
            1,
            "",
            session.active_revision().unwrap().scope.clone(),
        ));

        assert_eq!(session_label(&session), "Compare run outcomes");
        assert_eq!(session_label(&empty), "New analysis");
        assert_eq!(
            revision_state_label(&session.active_revision().unwrap().state),
            "Plan ready"
        );
        assert_eq!(
            revision_row_label(session.active_revision().unwrap()),
            "Not run"
        );
        assert_eq!(relative_time_label(1_000, 31_000), "Just now");
        assert_eq!(relative_time_label(1_000, 301_000), "5 min ago");
        assert_eq!(relative_time_label(1_000, 7_201_000), "2 hr ago");
    }

    #[test]
    fn failed_persistence_does_not_promote_bootstrap_session() {
        assert_eq!(persisted_session_id("analysis-123", false), None);
        assert_eq!(
            persisted_session_id("analysis-123", true),
            Some("analysis-123".into())
        );
    }

    #[test]
    fn run_scope_chip_exposes_run_and_root_coordinates() {
        let item = AnalysisScopeItem::Run {
            run: RunSummary {
                dataset: "default".into(),
                file: "source.json".into(),
                run_id: Some("run-a".into()),
                agent_id: "agent".into(),
                model_name: None,
                session_id: "session-a".into(),
                root_session_id: Some("root-a".into()),
                path: "agent/root-a/session-a".into(),
                row_count: 1,
                duplicate_event_ids: 0,
                status: "ok".into(),
            },
        };

        assert_eq!(
            scope_item_label(&item),
            "Run · default / source.json / agent / root-a / session-a / run-a"
        );
    }

    #[test]
    fn async_session_fence_rejects_another_session_with_the_same_revision_id() {
        let mut session = AnalysisSession::with_revision(plan_ready_revision());
        let expected_session_id = session.id.clone();

        assert!(revision_for_callback(&mut session, &expected_session_id, 7).is_some());
        assert!(revision_for_callback(&mut session, "another-session", 7).is_none());
    }

    #[test]
    fn callback_can_finish_an_inactive_revision_but_operation_token_still_decides() {
        let mut planning =
            AnalysisRevision::draft(7, "Compare run outcomes", plan_ready_revision().scope);
        let operation_id = planning.begin_plan_generation().unwrap();
        let mut session = AnalysisSession::with_revision(planning);
        let expected_session_id = session.id.clone();
        let active_id = session
            .new_revision("Another question", plan_ready_revision().scope)
            .id;

        let revision = revision_for_callback(&mut session, &expected_session_id, 7).unwrap();
        assert_eq!(
            revision
                .finish_plan(7, operation_id + 1, plan_ready_revision().plan.unwrap())
                .unwrap(),
            None
        );
        assert_eq!(revision.state, RevisionState::GeneratingPlan);
        revision
            .finish_plan(7, operation_id, plan_ready_revision().plan.unwrap())
            .unwrap();

        assert_eq!(revision.state, RevisionState::PlanReady);
        assert_eq!(session.active_revision_id, active_id);
    }

    #[test]
    fn query_and_interpretation_callbacks_can_finish_inactive_revisions() {
        let mut executing = plan_ready_revision();
        executing.confirm_execution().unwrap();
        let (revision_id, query_operation) = match executing.take_pending_effect().unwrap() {
            AnalysisEffect::ExecuteSql {
                revision_id,
                operation_id,
                ..
            } => (revision_id, operation_id),
            effect => panic!("expected execute effect, got {effect:?}"),
        };
        let mut query_session = AnalysisSession::with_revision(executing);
        let query_session_id = query_session.id.clone();
        let query_active_id = query_session
            .new_revision("Another question", plan_ready_revision().scope)
            .id;

        revision_for_callback(&mut query_session, &query_session_id, revision_id)
            .unwrap()
            .finish_query(
                revision_id,
                query_operation,
                QueryEvidence {
                    rows: Vec::new(),
                    returned_rows: 0,
                    truncated: false,
                    max_rows: 100,
                    max_bytes: 4 * 1024 * 1024,
                },
                Vec::new(),
            )
            .unwrap();

        assert_eq!(
            query_session
                .revisions
                .iter()
                .find(|revision| revision.id == revision_id)
                .unwrap()
                .state,
            RevisionState::Complete
        );
        assert_eq!(query_session.active_revision_id, query_active_id);

        let mut interpreting = plan_ready_revision();
        interpreting.confirm_execution().unwrap();
        let (revision_id, query_operation) = match interpreting.take_pending_effect().unwrap() {
            AnalysisEffect::ExecuteSql {
                revision_id,
                operation_id,
                ..
            } => (revision_id, operation_id),
            effect => panic!("expected execute effect, got {effect:?}"),
        };
        let interpretation_operation = match interpreting
            .finish_query(
                revision_id,
                query_operation,
                QueryEvidence {
                    rows: vec![serde_json::json!({"status": "failed"})],
                    returned_rows: 1,
                    truncated: false,
                    max_rows: 100,
                    max_bytes: 4 * 1024 * 1024,
                },
                Vec::new(),
            )
            .unwrap()
            .unwrap()
        {
            AnalysisEffect::Interpret { operation_id, .. } => operation_id,
            effect => panic!("expected interpretation effect, got {effect:?}"),
        };
        let mut interpretation_session = AnalysisSession::with_revision(interpreting);
        let interpretation_session_id = interpretation_session.id.clone();
        let interpretation_active_id = interpretation_session
            .new_revision("Another question", plan_ready_revision().scope)
            .id;

        revision_for_callback(
            &mut interpretation_session,
            &interpretation_session_id,
            revision_id,
        )
        .unwrap()
        .finish_interpretation(
            revision_id,
            interpretation_operation,
            AnalysisInterpretation::default(),
        )
        .unwrap();

        assert_eq!(
            interpretation_session
                .revisions
                .iter()
                .find(|revision| revision.id == revision_id)
                .unwrap()
                .state,
            RevisionState::Complete
        );
        assert_eq!(
            interpretation_session.active_revision_id,
            interpretation_active_id
        );
    }

    #[test]
    fn scope_removal_policy_blocks_the_last_chip_and_plan_or_query_generation() {
        let catalog = QueryCatalog {
            snapshot_id: "snapshot-1".into(),
            read_only: true,
            database: "default".into(),
            storage_path: "/tmp/evidence".into(),
            path_column: "_file_".into(),
            datasets: Vec::new(),
            tables: Vec::new(),
        };
        let dataset_scope = AnalysisScope::from_catalog(&catalog);
        let root_scope = AnalysisScope::from_root(&catalog, "default", "source.json", "root-a");
        assert!(!scope_item_removal_enabled(
            &dataset_scope,
            Some(&catalog),
            None
        ));
        assert!(scope_item_removal_enabled(
            &root_scope,
            Some(&catalog),
            None
        ));
        assert!(!scope_item_removal_enabled(&root_scope, None, None));
        assert!(!scope_item_removal_enabled(
            &root_scope,
            Some(&catalog),
            Some(&RevisionState::GeneratingPlan)
        ));
        assert!(!scope_item_removal_enabled(
            &root_scope,
            Some(&catalog),
            Some(&RevisionState::Executing)
        ));
        assert!(scope_item_removal_enabled(
            &root_scope,
            Some(&catalog),
            Some(&RevisionState::Interpreting)
        ));
    }

    #[test]
    fn run_requires_draft_question_to_match_reviewed_question() {
        for state in [RevisionState::PlanReady, RevisionState::QueryError] {
            let mut revision = plan_ready_revision();
            revision.state = state;

            let changed = AnalysisViewModel::from_revision(&revision, "Compare model latency");
            assert!(!changed.run_enabled);
            assert!(changed.question_out_of_date);

            let reviewed = AnalysisViewModel::from_revision(&revision, "  Compare run outcomes  ");
            assert!(reviewed.run_enabled);
            assert!(!reviewed.question_out_of_date);
        }
    }

    #[test]
    fn refinement_plan_requires_source_revision_and_current_reviewed_question() {
        assert!(refinement_plan_allowed(
            7,
            7,
            "  Compare run outcomes  ",
            "Compare run outcomes",
        ));
        assert!(!refinement_plan_allowed(
            7,
            7,
            "Compare model latency",
            "Compare run outcomes",
        ));
        assert!(!refinement_plan_allowed(
            8,
            7,
            "Compare run outcomes",
            "Compare run outcomes",
        ));
    }

    #[test]
    fn query_success_prepares_a_digest_after_publishing_evidence() {
        let mut revision = plan_ready_revision();
        revision.confirm_execution().unwrap();
        let (revision_id, operation_id) = match revision.take_pending_effect().unwrap() {
            AnalysisEffect::ExecuteSql {
                revision_id,
                operation_id,
                ..
            } => (revision_id, operation_id),
            effect => panic!("expected execute effect, got {effect:?}"),
        };
        let evidence = QueryEvidence {
            rows: vec![serde_json::json!({"status": "failed"})],
            returned_rows: 1,
            truncated: false,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        };

        let prepared = finish_query_for_interpretation(
            &mut revision,
            revision_id,
            operation_id,
            evidence.clone(),
            profile_rows(&evidence.rows),
        )
        .unwrap()
        .unwrap();

        assert_eq!(revision.evidence, Some(evidence));
        assert_eq!(revision.state, RevisionState::Interpreting);
        assert_eq!(prepared.revision_id, revision_id);
        assert_eq!(prepared.digest.rows.len(), 1);
    }

    #[test]
    fn manual_sql_on_a_restored_revision_discards_all_derived_result_state() {
        let mut revision = plan_ready_revision();
        revision.confirm_execution().unwrap();
        let (revision_id, query_operation) = match revision.take_pending_effect().unwrap() {
            AnalysisEffect::ExecuteSql {
                revision_id,
                operation_id,
                ..
            } => (revision_id, operation_id),
            effect => panic!("expected execute effect, got {effect:?}"),
        };
        let interpretation_effect = revision
            .finish_query(
                revision_id,
                query_operation,
                QueryEvidence {
                    rows: vec![serde_json::json!({"status": "failed"})],
                    returned_rows: 1,
                    truncated: false,
                    max_rows: 100,
                    max_bytes: 4 * 1024 * 1024,
                },
                Vec::new(),
            )
            .unwrap()
            .unwrap();
        let AnalysisEffect::Interpret {
            operation_id: interpretation_operation,
            ..
        } = interpretation_effect
        else {
            panic!("expected interpretation effect");
        };
        revision
            .finish_interpretation(
                revision_id,
                interpretation_operation,
                AnalysisInterpretation {
                    observations: vec!["old conclusion".into()],
                    ..AnalysisInterpretation::default()
                },
            )
            .unwrap();
        revision.evidence = None;
        revision.state = RevisionState::QueryError;
        revision.needs_rerun = true;

        apply_manual_sql(&mut revision, "SELECT 1".into()).unwrap();

        assert_eq!(revision.state, RevisionState::PlanReady);
        assert!(revision.execution.is_none());
        assert!(revision.evidence.is_none());
        assert!(revision.interpretation.is_none());
        assert!(!revision.needs_rerun);
    }

    #[test]
    fn removing_scope_items_changes_only_the_working_scope_and_never_removes_the_last() {
        let mut working_scope = plan_ready_revision().scope;
        working_scope.items.push(AnalysisScopeItem::Dataset {
            name: "secondary".into(),
        });
        let reviewed_scope = working_scope.clone();

        let next_scope = scope_without_item(&working_scope, 0, None).unwrap();

        assert_eq!(next_scope.items.len(), 1);
        assert_eq!(
            next_scope.items[0],
            AnalysisScopeItem::Dataset {
                name: "secondary".into()
            }
        );
        assert_eq!(working_scope, reviewed_scope);
        assert!(scope_without_item(&next_scope, 0, None).is_none());
        assert!(scope_without_item(&working_scope, 9, None).is_none());
    }

    #[test]
    fn removing_a_single_run_or_root_falls_back_to_catalog_scope() {
        let catalog = QueryCatalog {
            snapshot_id: "snapshot-1".into(),
            read_only: true,
            database: "default".into(),
            storage_path: "/tmp/evidence".into(),
            path_column: "_file_".into(),
            datasets: Vec::new(),
            tables: Vec::new(),
        };
        let run_scope = AnalysisScope::from_run(
            &catalog,
            RunSummary {
                dataset: "default".into(),
                file: "source.json".into(),
                run_id: Some("run-a".into()),
                agent_id: "agent".into(),
                model_name: None,
                session_id: "session-a".into(),
                root_session_id: Some("root-a".into()),
                path: "agent/root-a/session-a".into(),
                row_count: 1,
                duplicate_event_ids: 0,
                status: "ok".into(),
            },
        );
        let root_scope = AnalysisScope::from_root(&catalog, "default", "source.json", "root-a");
        let dataset_scope = AnalysisScope::from_catalog(&catalog);
        let expected = AnalysisScope::from_catalog(&catalog);

        assert_eq!(
            scope_without_item(&run_scope, 0, Some(&catalog)),
            Some(expected.clone())
        );
        assert_eq!(
            scope_without_item(&root_scope, 0, Some(&catalog)),
            Some(expected)
        );
        assert!(scope_without_item(&dataset_scope, 0, Some(&catalog)).is_none());
        assert!(scope_without_item(&run_scope, 0, None).is_none());
    }

    #[test]
    fn follow_up_planning_requires_the_current_revision_and_unchanged_draft() {
        assert!(follow_up_plan_allowed(
            7,
            7,
            "  Compare run outcomes  ",
            "Compare run outcomes",
        ));
        assert!(!follow_up_plan_allowed(
            7,
            8,
            "Compare run outcomes",
            "Compare run outcomes",
        ));
        assert!(!follow_up_plan_allowed(
            7,
            7,
            "Compare model latency",
            "Compare run outcomes",
        ));
    }

    #[test]
    fn interpretation_reference_reuses_result_identity_coordinates() {
        let reference = EvidenceReference {
            label: "failed turn".into(),
            row_index: Some(0),
            dataset: Some("default".into()),
            file: Some("source.json".into()),
            run_id: Some("run-1".into()),
            agent_id: Some("agent-1".into()),
            session_id: Some("session-1".into()),
            root_session_id: Some("root-1".into()),
            turn_id: Some(4),
        };

        let identity = interpretation_reference_identity(&reference).unwrap();

        assert_eq!(
            identity.run_href,
            "?page=detail&dataset=default&file=source.json&run_id=run-1&agent_id=agent-1&session_id=session-1&root_session_id=root-1"
        );
        assert_eq!(
            identity.turn_href,
            Some(format!("{}&turn=4", identity.run_href))
        );
    }
}
