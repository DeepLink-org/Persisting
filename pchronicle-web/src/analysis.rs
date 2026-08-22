use dioxus::prelude::*;

use crate::analysis_agent::{self, EvidenceDigest, InterpretationRequest, PlanRequest};
use crate::analysis_session::{
    self, AnalysisEffect, AnalysisInterpretation, AnalysisOperationId, AnalysisRevision,
    AnalysisScope, AnalysisScopeItem, AnalysisSession, EvidenceReference, RevisionState,
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
        Self {
            primary_action,
            run_enabled: review_is_runnable && question_matches,
            question_out_of_date: review_is_runnable && !question_matches,
            query_in_flight: revision.state == RevisionState::Executing,
            manually_edited: revision.manually_edited,
            sql_disclosure_label: "Advanced · view or edit SQL",
        }
    }
}

fn apply_manual_sql(revision: &mut AnalysisRevision, sql: String) -> Result<(), String> {
    if !matches!(
        revision.state,
        RevisionState::PlanReady | RevisionState::QueryError
    ) {
        return Err("SQL can only be edited after a plan is ready.".into());
    }
    let Some(plan) = revision.plan.as_mut() else {
        return Err("A plan is required before editing SQL.".into());
    };
    plan.sql = sql;
    revision.manually_edited = true;
    revision.state = RevisionState::PlanReady;
    revision.error = None;
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

fn launch_interpretation(
    config: llm::LlmConfig,
    expected_session_id: String,
    prepared: PreparedInterpretation,
    mut session: Signal<Option<AnalysisSession>>,
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
        let Some(revision) = current.active_revision_mut() else {
            return;
        };
        if revision.id != prepared.revision_id {
            return;
        }
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
        persist_session(&current, &mut storage_notice);
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
    let scope = initial_scope.or(default_scope);
    let mut question = use_signal(String::new);
    let mut session = use_signal(|| None::<AnalysisSession>);
    let mut config = use_signal(llm::load_config);
    let mut settings_open = use_signal(|| false);
    let mut storage_notice = use_signal(|| None::<String>);
    let mut restored = use_signal(|| false);

    let restore_catalog = catalog.clone();
    let restore_requested = requested_session_id.clone();
    use_effect(move || {
        if restored() {
            return;
        }
        let Some(catalog) = restore_catalog.as_ref() else {
            return;
        };
        restored.set(true);
        let Some(requested_id) = restore_requested.as_deref() else {
            return;
        };
        match analysis_session::load_sessions(&catalog.storage_path) {
            Ok(sessions) => {
                if let Some(saved) = sessions
                    .into_iter()
                    .find(|candidate| candidate.id == requested_id)
                {
                    if let Some(revision) = saved
                        .revisions
                        .iter()
                        .find(|revision| revision.id == saved.active_revision_id)
                    {
                        question.set(revision.question.clone());
                    }
                    session.set(Some(saved));
                }
            }
            Err(message) => storage_notice.set(Some(message)),
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
        && scope.is_some()
        && config().is_configured()
        && !question().trim().is_empty()
        && !generating;

    let scope_for_generate = scope.clone();
    let catalog_for_generate = catalog.clone();
    let generate_plan = move |_| {
        let Some(catalog) = catalog_for_generate.clone() else {
            return;
        };
        let Some(scope) = scope_for_generate.clone() else {
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
        let Some(revision) = next_session.active_revision_mut() else {
            return;
        };
        revision.question = prompt.clone();
        let revision_id = revision.id;
        let Ok(operation_id) = revision.begin_plan_generation() else {
            return;
        };
        let expected_session_id = next_session.id.clone();
        on_session_change.call(expected_session_id.clone());
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
            let Some(revision) = current.active_revision_mut() else {
                return;
            };
            if revision.id != revision_id {
                return;
            }
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut storage_notice);
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
        session.set(Some(current));
        spawn(async move {
            let result = api::query_evidence_interactive(&sql).await;
            let Some(mut current) = session() else {
                return;
            };
            if current.id != expected_session_id {
                return;
            }
            let Some(revision) = current.active_revision_mut() else {
                return;
            };
            if revision.id != revision_id {
                return;
            }
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
            persist_session(&current, &mut storage_notice);
            session.set(Some(current));
            if let Some(prepared) = prepared {
                launch_interpretation(
                    interpretation_config,
                    expected_session_id,
                    prepared,
                    session,
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
        let session_id = current.id.clone();
        on_session_change.call(session_id);
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
            let Some(revision) = current.active_revision_mut() else {
                return;
            };
            if revision.id != revision_id {
                return;
            }
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut storage_notice);
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
        persist_session(&current, &mut storage_notice);
        session.set(Some(current));
        launch_interpretation(
            interpretation_config,
            expected_session_id,
            prepared,
            session,
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
        on_session_change.call(expected_session_id.clone());
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
            let Some(revision) = current.active_revision_mut() else {
                return;
            };
            if revision.id != revision_id {
                return;
            }
            match result {
                Ok(plan) => {
                    let _ = revision.finish_plan(revision_id, operation_id, plan);
                }
                Err(error) => {
                    let _ = revision.fail_plan(revision_id, operation_id, error.message);
                }
            }
            persist_session(&current, &mut storage_notice);
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

    rsx! {
        section { class: "analyze-workspace", aria_label: "Question-driven analysis workspace",
            header { class: "analyze-header",
                div {
                    p { class: "analyze-eyebrow", "pChronicle / Analyze" }
                    h1 { "Ask a question. Review the plan. Run when ready." }
                    p { "Turn trajectory evidence into a bounded, read-only query without writing SQL first." }
                }
                button { class: "button analyze-settings-button", r#type: "button", onclick: move |_| settings_open.set(true),
                    span { aria_hidden: "true", "⚙" }
                    "Model settings"
                }
            }

            div { class: "analyze-layout",
                div { class: "analyze-main",
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
                            if let Some(scope) = &scope {
                                for item in &scope.items {
                                    span { class: "analyze-chip", "{scope_item_label(item)}" }
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
                            p { "Planning proposes SQL only. Nothing is queried until you confirm." }
                            button { class: "button primary", r#type: "button", disabled: !can_generate, onclick: generate_plan,
                                if generating { span { class: "analyze-spinner", aria_hidden: "true" } "Generating plan…" } else { "Generate plan" }
                            }
                        }
                    }

                    if let Some(revision) = active_revision.as_ref() {
                        if let Some(plan) = revision.plan.as_ref() {
                            section { class: "analyze-plan-card", aria_label: "Proposed analysis plan",
                                div { class: "analyze-section-heading",
                                    div { span { "02" } div { h2 { "Review the analysis plan" } p { "Confirm the intent and constraints before any evidence is queried." } } }
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
                                details { class: "analyze-sql-details",
                                    summary { "Advanced · view or edit SQL" }
                                    label { r#for: "analysis-sql", "Read-only SQL" }
                                    textarea {
                                        id: "analysis-sql",
                                        class: "analyze-sql-editor",
                                        rows: "8",
                                        value: "{plan.sql}",
                                        disabled: revision.state == RevisionState::Executing,
                                        oninput: move |event| {
                                            let Some(mut current) = session() else { return; };
                                            let Some(active) = current.active_revision_mut() else { return; };
                                            let _ = apply_manual_sql(active, event.value());
                                            session.set(Some(current));
                                        },
                                    }
                                }
                                if revision.state == RevisionState::QueryError {
                                    if let Some(error) = revision.error.as_ref() {
                                        div { class: "analyze-error", role: "alert", strong { "Analysis could not run" } p { "{error}" } p { "The reviewed plan and SQL are unchanged. Retry when ready." } }
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
                                    button { class: "button", r#type: "button", disabled: revision.state == RevisionState::Executing, onclick: regenerate_plan, "Regenerate" }
                                    button { class: "button primary", r#type: "button",
                                        disabled: !view_model.as_ref().is_some_and(|model| model.run_enabled),
                                        onclick: run_analysis,
                                        if revision.state == RevisionState::Executing { span { class: "analyze-spinner", aria_hidden: "true" } "Running analysis…" }
                                        else if view_model.as_ref().is_some_and(|model| model.primary_action == PrimaryAction::RetryAnalysis) { "Retry analysis" }
                                        else { "Run analysis" }
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
                                    p { "This saved interpretation remains available, but inspect or rerun the reviewed SQL to restore Result Explorer evidence." }
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

                aside { class: "analyze-aside", aria_label: "Analysis safeguards",
                    section {
                        p { class: "analyze-eyebrow", "Execution boundary" }
                        h2 { "You stay in control" }
                        ol {
                            li { span { "1" } div { strong { "Ask" } p { "Start with a question in plain language." } } }
                            li { span { "2" } div { strong { "Review" } p { "Inspect scope, measures, warnings, and SQL." } } }
                            li { span { "3" } div { strong { "Run" } p { "Only the Run analysis button queries evidence." } } }
                        }
                    }
                    section { class: "analyze-privacy",
                        p { class: "analyze-eyebrow", "Browser BYOK" }
                        h2 { "Direct model connection" }
                        p { "Schema, your question, and a bounded evidence digest go directly from this browser to the configured model endpoint." }
                        button { class: "analyze-link-button", r#type: "button", onclick: move |_| settings_open.set(true), "Review model settings" }
                    }
                    if let Some(catalog) = &catalog {
                        section { class: "analyze-catalog-card",
                            p { class: "analyze-eyebrow", "Active catalog" }
                            h2 { "{catalog.database}" }
                            dl {
                                div { dt { "Tables" } dd { "{catalog.tables.len()}" } }
                                div { dt { "Datasets" } dd { "{catalog.datasets.len()}" } }
                                div { dt { "Mode" } dd { if catalog.read_only { "Read-only" } else { "Bounded query" } } }
                            }
                            code { "{catalog.storage_path}" }
                        }
                    }
                    if let Some(message) = storage_notice() {
                        p { class: "analyze-storage-notice", role: "status", "{message}" }
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

fn persist_session(session: &AnalysisSession, storage_notice: &mut Signal<Option<String>>) {
    let mut sessions = match analysis_session::load_sessions(&session.storage_fingerprint) {
        Ok(sessions) => sessions,
        Err(message) => {
            storage_notice.set(Some(message));
            return;
        }
    };
    sessions.retain(|saved| saved.id != session.id);
    sessions.push(session.clone());
    if let Err(message) = analysis_session::save_sessions(&session.storage_fingerprint, &sessions) {
        storage_notice.set(Some(message));
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
        AnalysisScopeItem::Run { run } => format!("Run · {}", run.session_id),
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
    use crate::model::QueryEvidence;

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
        assert_eq!(model.sql_disclosure_label, "Advanced · view or edit SQL");
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
