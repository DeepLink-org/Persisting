use std::collections::BTreeSet;

use dioxus::prelude::*;
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::json_value::{is_structured_json, JsonValue};
use crate::model::QueryEvidence;
use crate::result_profile::{
    AnalysisRefinement, ColumnKind, ColumnProfile, RefinementIntent, RefinementPredicate,
};

const MAX_COLUMNS: usize = 16;
const MAX_CELL_CHARS: usize = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultIdentity {
    pub run_href: String,
    pub turn_href: Option<String>,
}

pub fn identity_href(row: &Value) -> Option<ResultIdentity> {
    let object = row.as_object()?;
    let coordinate = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let dataset = coordinate("dataset")?;
    let file = coordinate("_file_")?;
    let agent_id = coordinate("agent_id")?;
    let session_id = coordinate("session_id")?;
    let mut run_href = format!(
        "?page=detail&dataset={}&file={}",
        urlencoding::encode(dataset),
        urlencoding::encode(file),
    );
    if let Some(run_id) = coordinate("run_id") {
        run_href.push_str("&run_id=");
        run_href.push_str(&urlencoding::encode(run_id));
    }
    run_href.push_str("&agent_id=");
    run_href.push_str(&urlencoding::encode(agent_id));
    run_href.push_str("&session_id=");
    run_href.push_str(&urlencoding::encode(session_id));
    if let Some(root_session_id) = coordinate("root_session_id") {
        run_href.push_str("&root_session_id=");
        run_href.push_str(&urlencoding::encode(root_session_id));
    }
    let turn_id = object.get("turn_id").and_then(Value::as_i64);
    Some(ResultIdentity {
        turn_href: turn_id.map(|turn_id| format!("{run_href}&turn={turn_id}")),
        run_href,
    })
}

pub fn profile_scope_label(evidence: &QueryEvidence) -> String {
    if evidence.returned_rows == 0 {
        return "No distribution · 0 returned rows".into();
    }
    format!(
        "{} · {} returned {}{}",
        if evidence.truncated {
            "Preview distribution"
        } else {
            "Distribution of all returned rows"
        },
        evidence.returned_rows,
        if evidence.returned_rows == 1 {
            "row"
        } else {
            "rows"
        },
        if evidence.truncated {
            " · truncated"
        } else {
            ""
        },
    )
}

#[component]
pub fn ResultExplorer(
    evidence: QueryEvidence,
    profiles: Vec<ColumnProfile>,
    revision_id: u64,
    refinement_enabled: bool,
    on_stage_filter: EventHandler<RefinementIntent>,
    on_prepare_refinement: EventHandler<AnalysisRefinement>,
) -> Element {
    let columns = table_columns(&evidence.rows);
    let visible_columns = columns
        .iter()
        .take(MAX_COLUMNS)
        .cloned()
        .collect::<Vec<_>>();
    let hidden_columns = columns.len().saturating_sub(visible_columns.len());
    let initial_column = visible_columns.first().cloned();
    let mut selected_column = use_signal(move || initial_column);
    let mut staged_intent = use_signal(|| None::<RefinementIntent>);
    let selected_profile = selected_column().and_then(|selected| {
        profiles
            .iter()
            .find(|profile| profile.name == selected)
            .cloned()
    });
    let scope_label = profile_scope_label(&evidence);
    let byte_budget = format_bytes(evidence.max_bytes);

    rsx! {
        section { class: "result-explorer", aria_label: "Result Explorer",
            header { class: "result-explorer-header",
                div {
                    strong { "Result Explorer" }
                    span { "{scope_label}" }
                }
                span { class: "result-explorer-count", "{evidence.returned_rows} rows · {columns.len()} columns" }
            }
            if !refinement_enabled {
                div { class: "result-refinement-stale", role: "status",
                    strong { "Refinement planning is paused" }
                    span { "Regenerate for the edited question, or restore the reviewed question to prepare a refinement." }
                }
            }
            div { class: "result-explorer-layout",
                div { class: "result-explorer-table-region",
                    div { class: "result-explorer-scroll",
                        table { class: "result-explorer-table",
                            thead { tr {
                                for column in &visible_columns {
                                    if let Some(profile) = profiles.iter().find(|profile| &profile.name == column) {
                                        th { key: "profile-{column}",
                                            ProfileHeader {
                                                profile: profile.clone(),
                                                revision_id,
                                                selected: selected_column().as_deref() == Some(column.as_str()),
                                                on_select: move |name| selected_column.set(Some(name)),
                                                on_stage: {
                                                    let on_stage_filter = on_stage_filter;
                                                    move |intent: RefinementIntent| {
                                                        staged_intent.set(Some(intent.clone()));
                                                        on_stage_filter.call(intent);
                                                    }
                                                },
                                            }
                                        }
                                    } else {
                                        th { key: "column-{column}", button { class: "result-profile-title", onclick: { let column = column.clone(); move |_| selected_column.set(Some(column.clone())) }, "{column}" } }
                                    }
                                }
                            } }
                            tbody {
                                for (row_index, row) in evidence.rows.iter().enumerate() {
                                    tr { key: "row-{row_index}",
                                        for (column_index, column) in visible_columns.iter().enumerate() {
                                            td { key: "cell-{row_index}-{column_index}",
                                                if column_index == 0 {
                                                    if let Some(identity) = identity_href(row) {
                                                        div { class: "result-identity-links",
                                                            a { href: "{identity.run_href}", "Run" }
                                                            if let Some(turn_href) = identity.turn_href { a { href: "{turn_href}", "Step" } }
                                                        }
                                                    }
                                                }
                                                BoundedCell { value: table_value(row, column).clone() }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(intent) = staged_intent() {
                        div { class: "result-refinement-stage", role: "status",
                            div {
                                span { "Staged refinement" }
                                strong { "{intent.column} · {intent.label}" }
                                small { "No query has run and the current SQL is unchanged." }
                            }
                            div { class: "result-refinement-actions",
                                button { class: "analyze-link-button", r#type: "button", onclick: move |_| staged_intent.set(None), "Cancel" }
                                button { class: "button primary", r#type: "button", disabled: !refinement_enabled, onclick: move |_| on_prepare_refinement.call(AnalysisRefinement::Filter { intent: intent.clone() }), "Apply through Assistant" }
                            }
                        }
                    }
                }
                if let Some(profile) = selected_profile {
                    ProfilePanel {
                        profile,
                        scope_label: scope_label.clone(),
                        revision_id,
                        refinement_enabled,
                        on_stage: {
                            let on_stage_filter = on_stage_filter;
                            move |intent: RefinementIntent| {
                                staged_intent.set(Some(intent.clone()));
                                on_stage_filter.call(intent);
                            }
                        },
                        on_prepare_refinement,
                    }
                }
            }
            footer { class: "result-explorer-footer",
                span { "Result limit · {evidence.max_rows} rows / {byte_budget}" }
                if hidden_columns > 0 { span { "+{hidden_columns} columns hidden" } }
                if evidence.truncated { span { "Returned rows only; the server truncated this result." } }
            }
        }
    }
}

#[component]
fn ProfileHeader(
    profile: ColumnProfile,
    revision_id: u64,
    selected: bool,
    on_select: EventHandler<String>,
    on_stage: EventHandler<RefinementIntent>,
) -> Element {
    let summary = profile_summary(&profile);
    let missing = percent(profile.missing_count, profile.row_count);
    let max_count = profile_max_count(&profile);
    rsx! {
        div { class: if selected { "result-profile-header selected" } else { "result-profile-header" },
            button { class: "result-profile-title", r#type: "button", title: "Inspect {profile.name}", onclick: { let name = profile.name.clone(); move |_| on_select.call(name.clone()) },
                strong { "{profile.name}" }
                span { "{kind_label(&profile.kind)}" }
            }
            div { class: "result-mini-profile", aria_label: "Returned-row preview for {profile.name}",
                if supports_value_filter(&profile.kind) {
                    for (index, count) in profile_counts(&profile).into_iter().enumerate() {
                        button {
                            key: "bar-{index}",
                            class: "result-mini-bar",
                            r#type: "button",
                            title: "Stage {count.label}",
                            onclick: {
                                let intent = value_intent(revision_id, &profile, index);
                                move |event| { event.stop_propagation(); if let Some(intent) = intent.clone() { on_stage.call(intent); } }
                            },
                            i { style: format!("height:{}%", percent(count.count, max_count).max(8.0)) }
                        }
                    }
                } else {
                    for (index, count) in profile_counts(&profile).into_iter().enumerate() {
                        span { key: "bar-{index}", class: "result-mini-bar", i { style: format!("height:{}%", percent(count.count, max_count).max(8.0)) } }
                    }
                }
            }
            div { class: "result-profile-meta", span { "{summary}" }
                if profile.missing_count > 0 {
                    button { r#type: "button", onclick: { let intent = missing_intent(revision_id, &profile); move |event| { event.stop_propagation(); on_stage.call(intent.clone()); } }, "{missing:.0}% missing" }
                } else { span { "0% missing" } }
            }
        }
    }
}

#[component]
fn ProfilePanel(
    profile: ColumnProfile,
    scope_label: String,
    revision_id: u64,
    refinement_enabled: bool,
    on_stage: EventHandler<RefinementIntent>,
    on_prepare_refinement: EventHandler<AnalysisRefinement>,
) -> Element {
    let max_count = profile_max_count(&profile);
    let missing = percent(profile.missing_count, profile.row_count);
    let full_profile = AnalysisRefinement::FullProfile {
        source_revision_id: revision_id,
        column: profile.name.clone(),
        column_kind: profile.kind.clone(),
    };
    rsx! {
        aside { class: "result-profile-panel", aria_label: "Column profile for {profile.name}",
            p { class: "analyze-eyebrow", "Selected column" }
            h3 { "{profile.name}" }
            div { class: "result-profile-kind", "{kind_label(&profile.kind)}" }
            p { class: "result-profile-scope", "{scope_label}" }
            dl { class: "result-profile-stats",
                div { dt { "Present" } dd { "{profile.non_null_count}" } }
                div { dt { "Unique" } dd { "{profile.unique_count}" } }
                div { dt { "Missing" } dd { "{missing:.1}%" } }
                for (label, value) in profile_stat_rows(&profile) {
                    div { dt { "{label}" } dd { "{value}" } }
                }
            }
            if profile.row_count == 0 {
                p { class: "result-profile-none", "No returned rows; no distribution is available." }
            } else {
                div { class: "result-profile-bars",
                    for (index, count) in profile_counts(&profile).into_iter().enumerate() {
                        if supports_value_filter(&profile.kind) {
                            button { key: "detail-{index}", r#type: "button", onclick: { let intent = value_intent(revision_id, &profile, index); move |_| if let Some(intent) = intent.clone() { on_stage.call(intent) } },
                                span { "{count.label}" }
                                i { span { style: format!("width:{}%", percent(count.count, max_count)) } }
                                code { "{count.count}" }
                            }
                        } else {
                            div { key: "detail-{index}", span { "{count.label}" } i { span { style: format!("width:{}%", percent(count.count, max_count)) } } code { "{count.count}" } }
                        }
                    }
                }
            }
            if profile.missing_count > 0 {
                button { class: "result-profile-missing", r#type: "button", onclick: { let intent = missing_intent(revision_id, &profile); move |_| on_stage.call(intent.clone()) }, "Stage missing values · {profile.missing_count}" }
            }
            button { class: "button result-full-profile", r#type: "button", disabled: !refinement_enabled, onclick: move |_| on_prepare_refinement.call(full_profile.clone()), "Create full-distribution query" }
            if refinement_enabled {
                small { "Assistant will draft an aggregate plan for review. It will not run automatically." }
            } else {
                small { "Regenerate or restore the reviewed question before preparing this query." }
            }
        }
    }
}

#[derive(Clone)]
struct ProfileCount {
    label: String,
    count: usize,
}

fn profile_counts(profile: &ColumnProfile) -> Vec<ProfileCount> {
    if !profile.top_values.is_empty() {
        return profile
            .top_values
            .iter()
            .map(|value| ProfileCount {
                label: value.label.clone(),
                count: value.count,
            })
            .collect();
    }
    profile
        .histogram
        .iter()
        .enumerate()
        .map(|(index, bin)| ProfileCount {
            label: format!(
                "{} to {}{}",
                format_profile_value(&profile.kind, bin.lower),
                format_profile_value(&profile.kind, bin.upper),
                if index + 1 == profile.histogram.len() {
                    " (inclusive)"
                } else {
                    ""
                }
            ),
            count: bin.count,
        })
        .collect()
}

fn profile_max_count(profile: &ColumnProfile) -> usize {
    profile_counts(profile)
        .into_iter()
        .map(|value| value.count)
        .max()
        .unwrap_or(1)
}

fn supports_value_filter(kind: &ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Categorical | ColumnKind::Boolean | ColumnKind::Number
    )
}

fn value_intent(
    revision_id: u64,
    profile: &ColumnProfile,
    index: usize,
) -> Option<RefinementIntent> {
    match profile.kind {
        ColumnKind::Categorical | ColumnKind::Boolean => {
            let value = profile.top_values.get(index)?;
            let predicate_value = if profile.kind == ColumnKind::Boolean {
                Value::Bool(value.label == "true")
            } else {
                Value::String(value.label.clone())
            };
            Some(RefinementIntent {
                source_revision_id: revision_id,
                column: profile.name.clone(),
                label: format!("equals {}", value.label),
                predicate: RefinementPredicate::Equals {
                    value: predicate_value,
                },
            })
        }
        ColumnKind::Number => {
            let bin = profile.histogram.get(index)?;
            Some(RefinementIntent {
                source_revision_id: revision_id,
                column: profile.name.clone(),
                label: format!(
                    "{} to {}",
                    format_number(bin.lower),
                    format_number(bin.upper)
                ),
                predicate: RefinementPredicate::NumericRange {
                    lower: bin.lower,
                    upper: bin.upper,
                    include_upper: index + 1 == profile.histogram.len(),
                },
            })
        }
        _ => None,
    }
}

fn missing_intent(revision_id: u64, profile: &ColumnProfile) -> RefinementIntent {
    RefinementIntent {
        source_revision_id: revision_id,
        column: profile.name.clone(),
        label: "is missing".into(),
        predicate: RefinementPredicate::Missing,
    }
}

#[component]
fn BoundedCell(value: Value) -> Element {
    let structured = is_structured_json(&value);
    let (preview, truncated) = bounded_text(&value, MAX_CELL_CHARS);
    let full_value = value_text(&value, true);
    let kind = value_kind(&value);
    let mut expanded = use_signal(|| false);
    rsx! {
        if truncated || structured {
            button { class: "result-cell result-cell-expand {kind}", r#type: "button", title: "Open full cell value", aria_label: "Open full cell value", onclick: move |_| expanded.set(true),
                span { "{preview}" } i { "↗" }
            }
        } else {
            span { class: "result-cell {kind}", "{preview}" }
        }
        if expanded() {
            div { class: "result-cell-backdrop", role: "presentation", onclick: move |_| expanded.set(false),
                section { class: "result-cell-modal", role: "dialog", aria_modal: "true", aria_label: "Full cell value", tabindex: "-1", onclick: move |event| event.stop_propagation(), onkeydown: move |event| if event.key() == Key::Escape { expanded.set(false); },
                    header { div { strong { "Full cell value" } span { "{kind}" } } button { aria_label: "Close full cell value", onclick: move |_| expanded.set(false), "×" } }
                    if structured { div { class: "result-cell-json", JsonValue { value: value.clone() } } }
                    else { pre { "{full_value}" } }
                    footer { span { "{full_value.chars().count()} characters" } button { class: "button primary", onclick: move |_| expanded.set(false), "Close" } }
                }
            }
        }
    }
}

fn table_columns(rows: &[Value]) -> Vec<String> {
    let mut columns = BTreeSet::new();
    let mut has_scalar = false;
    for row in rows {
        if let Value::Object(object) = row {
            columns.extend(object.keys().cloned());
        } else {
            has_scalar = true;
        }
    }
    if has_scalar {
        columns.insert("value".into());
    }
    columns.into_iter().collect()
}

fn table_value<'a>(row: &'a Value, column: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    match row {
        Value::Object(object) => object.get(column).unwrap_or(&NULL),
        value if column == "value" => value,
        _ => &NULL,
    }
}

fn bounded_text(value: &Value, limit: usize) -> (String, bool) {
    let raw = value_text(value, false);
    if raw.chars().count() <= limit {
        (raw, false)
    } else {
        (
            format!(
                "{}…",
                raw.chars()
                    .take(limit.saturating_sub(1))
                    .collect::<String>()
            ),
            true,
        )
    }
}

fn value_text(value: &Value, pretty: bool) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        value if pretty => serde_json::to_string_pretty(value).unwrap_or_default(),
        value => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) | Value::Object(_) => "structured",
        Value::String(_) => "text",
    }
}

fn kind_label(kind: &ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Empty => "empty",
        ColumnKind::Number => "number",
        ColumnKind::Boolean => "boolean",
        ColumnKind::Categorical => "categorical",
        ColumnKind::Text => "text",
        ColumnKind::DateTime => "datetime",
        ColumnKind::Object => "object",
        ColumnKind::Array => "array",
        ColumnKind::Identifier => "identifier",
        ColumnKind::Mixed => "mixed",
    }
}

fn profile_summary(profile: &ColumnProfile) -> String {
    match (profile.min, profile.max) {
        (Some(min), Some(max)) => format!(
            "{}–{}",
            format_profile_value(&profile.kind, min),
            format_profile_value(&profile.kind, max)
        ),
        _ => format!("{} unique", profile.unique_count),
    }
}

fn profile_stat_rows(profile: &ColumnProfile) -> Vec<(&'static str, String)> {
    let labels = match profile.kind {
        ColumnKind::Number => Some(("Minimum", "Maximum", "Mean", "Median")),
        ColumnKind::Text | ColumnKind::Array => Some((
            "Minimum length",
            "Maximum length",
            "Mean length",
            "Median length",
        )),
        ColumnKind::DateTime => Some(("Earliest", "Latest", "", "")),
        _ => None,
    };
    let Some((min_label, max_label, mean_label, median_label)) = labels else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if let Some(value) = profile.min {
        rows.push((min_label, format_profile_value(&profile.kind, value)));
    }
    if let Some(value) = profile.max {
        rows.push((max_label, format_profile_value(&profile.kind, value)));
    }
    if !mean_label.is_empty() {
        if let Some(value) = profile.mean {
            rows.push((mean_label, format_profile_value(&profile.kind, value)));
        }
    }
    if !median_label.is_empty() {
        if let Some(value) = profile.median {
            rows.push((median_label, format_profile_value(&profile.kind, value)));
        }
    }
    rows
}

fn format_profile_value(kind: &ColumnKind, value: f64) -> String {
    if kind != &ColumnKind::DateTime || !value.is_finite() {
        return format_number(value);
    }
    let nanos = (value * 1_000_000_000.0).round();
    if nanos < i128::MIN as f64 || nanos > i128::MAX as f64 {
        return format_number(value);
    }
    OffsetDateTime::from_unix_timestamp_nanos(nanos as i128)
        .ok()
        .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
        .unwrap_or_else(|| format_number(value))
}

fn format_number(value: f64) -> String {
    if value.abs() >= 10_000.0 || (value != 0.0 && value.abs() < 0.01) {
        format!("{value:.2e}")
    } else if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn percent(value: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 * 100.0 / total as f64
    }
}

fn format_bytes(value: usize) -> String {
    if value >= 1024 * 1024 && value.is_multiple_of(1024 * 1024) {
        format!("{} MiB", value / (1024 * 1024))
    } else if value >= 1024 && value.is_multiple_of(1024) {
        format!("{} KiB", value / 1024)
    } else {
        format!("{value} bytes")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::model::QueryEvidence;

    use crate::result_profile::profile_rows;

    use super::{identity_href, profile_counts, profile_scope_label};

    fn evidence(rows: Vec<Value>, returned_rows: usize, truncated: bool) -> QueryEvidence {
        QueryEvidence {
            rows,
            returned_rows,
            truncated,
            max_rows: 100,
            max_bytes: 4 * 1024 * 1024,
        }
    }

    #[test]
    fn complete_coordinates_create_run_and_turn_links() {
        let row = json!({
            "dataset":"captures",
            "_file_":"nested/run.json",
            "run_id":"run-1",
            "agent_id":"agent-a",
            "session_id":"session-a",
            "root_session_id":"root-a",
            "turn_id":12
        });

        let identity = identity_href(&row).unwrap();

        assert!(identity.run_href.contains("page=detail"));
        assert!(identity.run_href.contains("session_id=session-a"));
        assert!(identity.turn_href.unwrap().contains("turn=12"));
    }

    #[test]
    fn incomplete_coordinates_do_not_guess_a_link() {
        assert_eq!(identity_href(&json!({"session_id":"only"})), None);
    }

    #[test]
    fn nullable_run_and_root_coordinates_still_create_links() {
        let row = json!({
            "dataset":"captures",
            "_file_":"gateway/events.lance",
            "agent_id":"gateway",
            "session_id":"session-a",
            "run_id": null,
            "root_session_id": null,
            "turn_id": 12
        });

        let identity = identity_href(&row).expect("detail supports nullable run and root ids");

        assert!(!identity.run_href.contains("run_id="));
        assert!(!identity.run_href.contains("root_session_id="));
        assert!(identity.run_href.contains("agent_id=gateway"));
        assert!(identity.turn_href.unwrap().contains("turn=12"));
    }

    #[test]
    fn truncated_results_are_labeled_as_preview() {
        assert_eq!(
            profile_scope_label(&evidence(Vec::new(), 100, true)),
            "Preview distribution · 100 returned rows · truncated"
        );
    }

    #[test]
    fn complete_results_are_labeled_as_all_returned_rows() {
        assert_eq!(
            profile_scope_label(&evidence(Vec::new(), 3, false)),
            "Distribution of all returned rows · 3 returned rows"
        );
    }

    #[test]
    fn datetime_profile_bins_use_datetime_labels() {
        let profiles = profile_rows(&[
            json!({"occurred_at":"2026-08-22T01:02:03Z"}),
            json!({"occurred_at":"2026-08-23T02:03:04Z"}),
        ]);
        let counts = profile_counts(&profiles[0]);

        assert!(counts[0].label.contains("2026-08-22"));
        assert!(!counts[0].label.contains("1.77e"));
    }
}
