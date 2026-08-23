use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::analysis_agent::{self, EvidenceDigest, InterpretationRequest, PlanRequest};
use crate::analysis_session::{
    self, AnalysisEffect, AnalysisInterpretation, AnalysisOperationId, AnalysisPlan,
    AnalysisRevision, AnalysisScope, AnalysisScopeItem, AnalysisSession, EvidenceReference,
    RevisionState, SuggestedView,
};
use crate::api;
use crate::llm;
use crate::llm_settings::LlmSettings;
use crate::model::{QueryCatalog, QueryEvidence};
use crate::result_explorer::{identity_href, ResultExplorer, ResultIdentity};
use crate::result_profile::{profile_rows, AnalysisRefinement, ColumnProfile};

const QUESTION_STARTERS: [&str; 3] = [
    "Compare successful and failed runs in this scope",
    "Find latency outliers and the tools associated with them",
    "Summarize explicit errors by tool and model",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryAction {
    GeneratePlan,
    RunAnalysis,
    RetryAnalysis,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AnalysisViewModel {
    primary_action: PrimaryAction,
    run_enabled: bool,
    question_out_of_date: bool,
    query_in_flight: bool,
    manually_edited: bool,
    sql_disclosure_label: &'static str,
}

impl AnalysisViewModel {
    fn from_revision(revision: &AnalysisRevision, draft_question: &str) -> Self {
        let primary_action = match revision.state {
            RevisionState::Draft | RevisionState::PlanError | RevisionState::Stale => {
                PrimaryAction::GeneratePlan
            }
            RevisionState::PlanReady => PrimaryAction::RunAnalysis,
            RevisionState::QueryError => PrimaryAction::RetryAnalysis,
            _ => PrimaryAction::None,
        };
        let review_is_runnable = matches!(
            primary_action,
            PrimaryAction::RunAnalysis | PrimaryAction::RetryAnalysis
        );
        let question_matches = draft_question.trim() == revision.question.trim();
        let sql_ready = revision
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.sql.trim().is_empty());
        Self {
            primary_action,
            run_enabled: review_is_runnable
                && sql_ready
                && (question_matches || revision.manually_edited),
            question_out_of_date: review_is_runnable
                && !question_matches
                && !revision.manually_edited,
            query_in_flight: revision.state == RevisionState::Executing,
            manually_edited: revision.manually_edited,
            sql_disclosure_label: "SQL",
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
        return Err("SQL can only be edited on a draft or reviewed revision.".into());
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
    revision.error = None;
    revision.execution = None;
    revision.evidence = None;
    revision.interpretation = None;
    revision.needs_rerun = false;
    revision.pending_effect = None;
    revision.active_operation_id = None;
    Ok(())
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
    let plan = revision
        .plan
        .as_ref()
        .ok_or_else(|| "A plan is required before interpreting query evidence.".to_string())?;
    let evidence = revision
        .evidence
        .as_ref()
        .ok_or_else(|| "Query evidence is required before interpretation.".to_string())?;
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
    let evidence = revision.evidence.as_ref().ok_or_else(|| {
        "The query evidence is unavailable; rerun the analysis first.".to_string()
    })?;
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
    let generating = active_revision
        .as_ref()
        .is_some_and(|revision| revision.state == RevisionState::GeneratingPlan);
    let can_generate = catalog.is_some()
        && scope().is_some()
        && config().is_configured()
        && !question().trim().is_empty()
        && !generating;

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
        let needs_new_revision = next_session.active_revision_mut().is_some_and(|revision| {
            !matches!(
                revision.state,
                RevisionState::Draft | RevisionState::PlanError | RevisionState::Stale
            ) || revision.question != prompt
                || revision.scope != scope
        });
        if needs_new_revision {
            let revision = next_session.new_revision(prompt.clone(), scope.clone());
            revision.prior_plan_context = previous_plan.clone();
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
            refinement: None,
        };
        spawn(async move {
            let result = analysis_agent::generate_plan(request).await;
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
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut recent_sessions, &mut storage_notice);
            session.set(Some(current));
        });
    };

    let run_analysis = move |_| {
        let Some(mut current) = session() else {
            return;
        };
        let Some(revision) = current.active_revision_mut() else {
            return;
        };
        if !AnalysisViewModel::from_revision(revision, &question()).run_enabled {
            return;
        }
        if revision.confirm_execution().is_err() {
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
                Err(message) => {
                    let _ = revision.fail_query(revision_id, operation_id, message);
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
        let revision = current.new_revision(prompt.clone(), scope.clone());
        revision.prior_plan_context = previous_plan.clone();
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
            refinement: Some(refinement),
        };
        spawn(async move {
            let result = analysis_agent::generate_plan(request).await;
            let Some(mut current) = session() else {
                return;
            };
            let Some(revision) =
                revision_for_callback(&mut current, &expected_session_id, revision_id)
            else {
                return;
            };
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut recent_sessions, &mut storage_notice);
            session.set(Some(current));
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
            refinement: None,
        };
        spawn(async move {
            let result = analysis_agent::generate_plan(request).await;
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
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut recent_sessions, &mut storage_notice);
            session.set(Some(current));
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
    let regenerate_plan = generate_plan.clone();

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
                storage_notice.set(Some("Analysis history cleared for this catalog.".into()));
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
    let run_enabled = view_model.as_ref().is_some_and(|model| model.run_enabled);
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

    rsx! {
        section { class: "analyze-workspace", aria_label: "Question-driven analysis workspace",
            header { class: "analyze-header",
                div { class: "analyze-header-inner",
                    div { class: "analyze-header-bar",
                        p { class: "analyze-eyebrow", "pChronicle / Analyze" }
                        div { class: "analyze-header-actions",
                            if !recent_sessions().is_empty() {
                                label { class: "analyze-recent-select",
                                    span { "Recent analysis" }
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
                                    span { "Clear this catalog's analysis history?" }
                                    button { class: "button", r#type: "button", onclick: clear_history, "Clear" }
                                    button { class: "analyze-link-button", r#type: "button", onclick: move |_| clear_confirmation.set(false), "Cancel" }
                                }
                            } else {
                                button { class: "analyze-link-button", r#type: "button", onclick: move |_| clear_confirmation.set(true), "Clear analysis history" }
                            }
                            button { class: "button analyze-settings-button", r#type: "button", onclick: move |_| settings_open.set(true),
                                span { aria_hidden: "true", "⚙" }
                                "Model settings"
                            }
                        }
                    }
                    h1 { "Ask a question. Or write SQL." }
                    p { class: "analyze-header-lede", "Describe what you want in plain language, or write a read-only query. Nothing is queried until you Run." }
                }
            }

            div { class: "analyze-layout",
                nav { class: "analyze-schema", aria_label: "Catalog schema",
                    div { class: "analyze-schema-heading",
                        p { class: "analyze-eyebrow", "Catalog" }
                        h2 { "SQL tables" }
                        p { "Names you can use in FROM. Click a table, then a field to insert it at the SQL cursor." }
                    }
                    if schema_tables.is_empty() {
                        p { class: "analyze-schema-empty", "Catalog is still loading." }
                    } else {
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
                                                small { "{table.grain}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(table) = selected_schema_table.as_ref() {
                            div { class: "analyze-schema-fields",
                                h3 { "{table.name} fields" }
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
                                                            let Some(mut current) = session() else { return; };
                                                            let Some(active) = current.active_revision_mut() else { return; };
                                                            let Ok(caret) = apply_inserted_token(active, &token, sql_textarea_cursor()) else { return; };
                                                            persist_session(&current, &mut recent_sessions, &mut storage_notice);
                                                            session.set(Some(current));
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
                div { class: "analyze-main",
                    if let Some(message) = storage_notice() {
                        p { class: "analyze-storage-notice analyze-storage-notice-inline", role: "status", "{message}" }
                    }
                    section { class: "analyze-question-card", aria_label: "Analysis question",
                        div { class: "analyze-section-heading",
                            div { span { "01" } div { h2 { "What do you want to understand?" } p { "Describe the comparison, pattern, or anomaly you want to investigate." } } }
                            span { class: "analyze-step-state", if generating { "Planning…" } else { "Draft" } }
                        }
                        label { class: "analyze-question-label", r#for: "analysis-question", "Question" }
                        textarea {
                            id: "analysis-question",
                            class: "analyze-question-input",
                            rows: "5",
                            value: "{question}",
                            placeholder: "Ask about runs, errors, latency, tool use, or model behavior…",
                            disabled: generating,
                            oninput: move |event| question.set(event.value()),
                        }
                        div { class: "analyze-context-row", aria_label: "Analysis context",
                            span { class: if catalog.is_some() { "analyze-status ready" } else { "analyze-status" },
                                span { aria_hidden: "true" }
                                if catalog.is_some() { "Catalog ready" } else { "Loading catalog…" }
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
                                                    aria_label: if removal_enabled { "Remove {label} from analysis scope" } else if blocked_by_operation { "Analysis scope cannot change while an operation is running" } else if single_dataset { "The dataset analysis scope cannot be removed" } else { "The catalog is required before this scope can be removed" },
                                                    title: if removal_enabled { "Remove scope" } else if blocked_by_operation { "Wait for the current plan or query operation to finish" } else if single_dataset { "At least one explicit scope is required" } else { "Wait for the catalog to load" },
                                                    onclick: move |_| remove_scope_item.call(index),
                                                    "×"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
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
                                div { strong { "Connect a model to generate a plan" } p { "Your draft stays here while you configure the endpoint." } }
                                button { class: "button", r#type: "button", onclick: move |_| settings_open.set(true), "Open model settings" }
                            }
                        }
                        if let Some(revision) = active_revision.as_ref() {
                            if revision.state == RevisionState::PlanError {
                                div { class: "analyze-error", role: "alert",
                                    strong { "The plan could not be generated" }
                                    if let Some(message) = revision.error.as_ref() { p { "{message}" } }
                                    else { p { "The model did not return a valid plan." } }
                                    p { "Your question is unchanged. Adjust it or generate the plan again." }
                                }
                            }
                        }
                        div { class: "analyze-question-actions",
                            p { "Generate plan fills the SQL editor. Run is the only query." }
                            button { class: "button primary", r#type: "button", disabled: !can_generate, onclick: generate_plan,
                                if generating { span { class: "analyze-spinner", aria_hidden: "true" } "Generating plan…" } else { "Generate plan" }
                            }
                        }
                    }

                    section { class: "analyze-sql-card", aria_label: "SQL editor",
                        div { class: "analyze-section-heading",
                            div { span { "02" } div { h2 { "SQL" } p { "Always visible. Generate plan writes this editor. Field clicks insert at the cursor." } } }
                            if active_revision.as_ref().is_some_and(|revision| revision.manually_edited) {
                                span { class: "analyze-edited-badge", "Manually edited" }
                            }
                        }
                        label { class: "analyze-question-label", r#for: "analysis-sql", "Read-only query" }
                        textarea {
                            id: "analysis-sql",
                            class: "analyze-sql-editor",
                            rows: "10",
                            value: "{sql_text}",
                            placeholder: "SELECT …",
                            disabled: sql_locked,
                            oninput: move |event| {
                                let Some(mut current) = session() else { return; };
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
                                if let Some(error) = revision.error.as_ref() {
                                    div { class: "analyze-error", role: "alert", strong { "Analysis could not run" } p { "{error}" } p { "The SQL is unchanged. Retry when ready." } }
                                }
                            }
                            if revision.needs_rerun {
                                div { class: "analyze-config-callout", role: "status",
                                    div { strong { "Rerun to restore rows" } p { "Saved summaries remain visible, but result rows are never stored in browser history." } }
                                }
                            }
                        }
                        if view_model.as_ref().is_some_and(|model| model.question_out_of_date) {
                            div { class: "analyze-config-callout", role: "status",
                                div {
                                    strong { "This plan is for the previous question" }
                                    p { "Regenerate to review a plan for the current question, or restore the reviewed question to run this SQL." }
                                }
                            }
                        }
                        div { class: "analyze-plan-actions",
                            p { "Nothing is queried until you Run." }
                            button { class: "button primary", r#type: "button",
                                disabled: !run_enabled,
                                onclick: run_analysis,
                                if active_revision.as_ref().is_some_and(|revision| revision.state == RevisionState::Executing) { span { class: "analyze-spinner", aria_hidden: "true" } "Running analysis…" }
                                else if active_revision.as_ref().is_some_and(|revision| revision.needs_rerun) { "Rerun to restore rows" }
                                else if view_model.as_ref().is_some_and(|model| model.primary_action == PrimaryAction::RetryAnalysis) { "Retry analysis" }
                                else { "Run analysis" }
                            }
                        }
                    }

                    if !revision_history.is_empty() {
                        nav { class: "analyze-revision-timeline", aria_label: "Analysis revision history",
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
                        if let Some(plan) = revision.plan.as_ref() {
                            if shows_plan_summary(plan) {
                            section { class: "analyze-plan-card", aria_label: "Proposed analysis plan",
                                div { class: "analyze-section-heading",
                                    div { span { "Plan" } div { h2 { "Review the analysis plan" } p { "Copilot proposed this intent. Edit SQL above or regenerate." } } }
                                    if revision.manually_edited { span { class: "analyze-edited-badge", "Manually edited" } }
                                }
                                dl { class: "analyze-plan-summary",
                                    div { dt { "Intent" } dd { "{plan.intent_summary}" } }
                                    div { dt { "Scope" } dd { "{plan.scope_summary}" } }
                                    PlanListRow { label: "Filters", values: plan.filters.clone() }
                                    PlanListRow { label: "Grouping", values: plan.groupings.clone() }
                                    PlanListRow { label: "Measures", values: plan.measures.clone() }
                                }
                                if !plan.warnings.is_empty() {
                                    div { class: "analyze-warnings", role: "note", strong { "Plan warnings" } ul { for warning in &plan.warnings { li { "{warning}" } } } }
                                }
                                div { class: "analyze-plan-actions",
                                    button { class: "button", r#type: "button", disabled: revision.state == RevisionState::Executing || generating, onclick: regenerate_plan, "Regenerate" }
                                }
                            }
                            }
                        }

                        if let Some(evidence) = revision.evidence.clone() {
                            section { class: "analyze-result-card", aria_label: "Analysis result",
                                div { class: "analyze-section-heading", div { span { "03" } div { h2 { "Analysis result" } p { "Bounded evidence returned by the confirmed query." } } } }
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
                                            div { strong { "Interpreting the returned evidence…" } p { "The Result Explorer remains available while the model prepares a grounded summary." } }
                                        }
                                    }
                                    if revision.state == RevisionState::InterpretationError {
                                        div { class: "analyze-interpretation-error", role: "alert",
                                            div {
                                                strong { "The evidence could not be interpreted" }
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
                                div { class: "analyze-section-heading", div { span { "03" } div { h2 { "Saved interpretation" } p { "The summary was restored from this analysis session." } } } }
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
        RevisionState::GeneratingPlan => "Planning",
        RevisionState::PlanReady => "Plan ready",
        RevisionState::Executing => "Running",
        RevisionState::Interpreting => "Interpreting",
        RevisionState::Complete => "Complete",
        RevisionState::PlanError => "Plan error",
        RevisionState::QueryError => "Rerun required",
        RevisionState::InterpretationError => "Interpretation error",
        RevisionState::Stale => "Stale",
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
        section { class: "analyze-interpretation", aria_label: "Evidence interpretation",
            div { class: "analyze-interpretation-grid",
                section { class: "analyze-interpretation-block observed",
                    h3 { "Observed in this result" }
                    if interpretation.observations.is_empty() {
                        p { class: "analyze-none", "No direct observations were returned." }
                    } else {
                        ul { for observation in &interpretation.observations { li { "{observation}" } } }
                    }
                    if !interpretation.references.is_empty() {
                        div { class: "analyze-interpretation-references", aria_label: "Grounded evidence references",
                            for reference in &interpretation.references {
                                if let Some(identity) = interpretation_reference_identity(reference) {
                                    span { class: "analyze-interpretation-reference linked",
                                        span { "{reference.label}" }
                                        a { href: "{identity.run_href}", "Run" }
                                        if let Some(turn_href) = identity.turn_href { a { href: "{turn_href}", "Turn" } }
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
                        p { class: "analyze-none", "No inference was offered from this evidence." }
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
                                            "Generate plan"
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
        EvidenceReference, RevisionState, SuggestedView,
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

        assert_eq!(model.primary_action, PrimaryAction::RunAnalysis);
        assert!(!model.query_in_flight);
        assert_eq!(model.sql_disclosure_label, "SQL");
        assert!(revision.pending_effect.is_none());
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
