use dioxus::prelude::*;

use crate::analysis_agent::{self, PlanRequest};
use crate::analysis_session::{
    self, AnalysisEffect, AnalysisRevision, AnalysisScope, AnalysisScopeItem, AnalysisSession,
    RevisionState,
};
use crate::api;
use crate::components::DataTable;
use crate::llm;
use crate::llm_settings::LlmSettings;
use crate::model::QueryCatalog;
use crate::result_profile::profile_rows;

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
        let previous_plan = next_session
            .active_revision_mut()
            .and_then(|revision| revision.plan.clone());
        let needs_new_revision = next_session.active_revision_mut().is_some_and(|revision| {
            !matches!(
                revision.state,
                RevisionState::Draft | RevisionState::PlanError | RevisionState::Stale
            ) || revision.question != prompt
                || revision.scope != scope
        });
        if needs_new_revision {
            next_session.new_revision(prompt.clone(), scope.clone());
        }
        let Some(revision) = next_session.active_revision_mut() else {
            return;
        };
        revision.question = prompt.clone();
        let revision_id = revision.id;
        let Ok(operation_id) = revision.begin_plan_generation() else {
            return;
        };
        let session_id = next_session.id.clone();
        on_session_change.call(session_id);
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
        session.set(Some(current));
        spawn(async move {
            let result = api::query_evidence_interactive(&sql).await;
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
                Ok(evidence) => {
                    let profiles = profile_rows(&evidence.rows);
                    // Task 6 consumes the returned Interpret effect. Until then, evidence
                    // is rendered immediately and no interpretation request is started.
                    let _ = revision.finish_query(revision_id, operation_id, evidence, profiles);
                }
                Err(message) => {
                    let _ = revision.fail_query(revision_id, operation_id, message);
                }
            }
            persist_session(&current, &mut storage_notice);
            session.set(Some(current));
        });
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
                                if let Some(error) = revision.error.as_ref() {
                                    div { class: "analyze-error", role: "alert", strong { "Analysis could not run" } p { "{error}" } p { "The reviewed plan and SQL are unchanged. Retry when ready." } }
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
                                DataTable { evidence: evidence.clone(), title: Some("Analysis result".into()) }
                                if evidence.rows.is_empty() {
                                    div { class: "analyze-empty-result",
                                        div {
                                            strong { "No rows matched this plan" }
                                            p { "Rewrite the question or broaden the plan before trying again." }
                                        }
                                        button { class: "button", r#type: "button", onclick: rewrite_problem, "Rewrite question" }
                                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis_session::{
        AnalysisPlan, AnalysisRevision, AnalysisScope, AnalysisScopeItem, RevisionState,
        SuggestedView,
    };

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
}
