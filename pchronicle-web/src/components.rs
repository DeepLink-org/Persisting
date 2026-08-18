use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{QueryEvidence, TurnDetail, TurnSummary};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryEmbed {
    pub title: Option<String>,
    pub turn_ids: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableEmbed {
    pub title: Option<String>,
    pub evidence: QueryEvidence,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RichBlock {
    Text(String),
    Trajectory(TrajectoryEmbed),
    Table(TableEmbed),
}

pub fn trajectory_fence(title: &str, turn_ids: Vec<i64>) -> String {
    let payload = TrajectoryEmbed {
        title: Some(title.into()),
        turn_ids,
    };
    component_fence("trajectory", &payload)
}

pub fn table_fence(title: &str, evidence: QueryEvidence) -> String {
    let payload = TableEmbed {
        title: Some(title.into()),
        evidence,
    };
    component_fence("table", &payload)
}

fn component_fence<T: Serialize>(kind: &str, payload: &T) -> String {
    format!(
        "```pchronicle:{kind}\n{}\n```",
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".into())
    )
}

pub fn parse_rich_blocks(value: &str) -> Vec<RichBlock> {
    let lines = value.split_inclusive('\n').collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut index = 0;
    while index < lines.len() {
        let marker = lines[index].trim();
        let kind = marker.strip_prefix("```pchronicle:");
        if let Some(kind) = kind {
            let mut end = index + 1;
            while end < lines.len() && lines[end].trim() != "```" {
                end += 1;
            }
            if end < lines.len() {
                let payload = lines[index + 1..end].concat();
                let parsed = match kind {
                    "trajectory" => serde_json::from_str(&payload)
                        .ok()
                        .map(RichBlock::Trajectory),
                    "table" => serde_json::from_str(&payload).ok().map(RichBlock::Table),
                    _ => None,
                };
                if let Some(parsed) = parsed {
                    push_text_block(&mut blocks, &mut text);
                    blocks.push(parsed);
                    index = end + 1;
                    continue;
                }
            }
        }
        text.push_str(lines[index]);
        index += 1;
    }
    push_text_block(&mut blocks, &mut text);
    blocks
}

fn push_text_block(blocks: &mut Vec<RichBlock>, text: &mut String) {
    let value = text.trim().to_string();
    if !value.is_empty() {
        blocks.push(RichBlock::Text(value));
    }
    text.clear();
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

fn table_columns(rows: &[Value]) -> Vec<String> {
    let mut columns = Vec::new();
    for row in rows {
        if let Value::Object(object) = row {
            for key in object.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        } else if !columns.iter().any(|column| column == "value") {
            columns.push("value".into());
        }
    }
    columns
}

fn table_value<'a>(row: &'a Value, column: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    match row {
        Value::Object(object) => object.get(column).unwrap_or(&NULL),
        value if column == "value" => value,
        _ => &NULL,
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

#[component]
pub fn DataTable(
    evidence: QueryEvidence,
    title: Option<String>,
    #[props(default = false)] embedded: bool,
) -> Element {
    const MAX_COLUMNS: usize = 16;
    const MAX_CELL_CHARS: usize = 180;
    let columns = table_columns(&evidence.rows);
    let visible_columns = columns
        .iter()
        .take(MAX_COLUMNS)
        .cloned()
        .collect::<Vec<_>>();
    let hidden_columns = columns.len().saturating_sub(visible_columns.len());
    let byte_budget = format_bytes(evidence.max_bytes);
    let class = if embedded {
        "pc2-data-component embedded"
    } else {
        "pc2-data-component"
    };
    let title = title.unwrap_or_else(|| "Query result".into());
    rsx! { section { class,
        header { div { strong { "{title}" } span { "{evidence.returned_rows} rows · {columns.len()} columns" } } if evidence.truncated { span { class: "pc2-data-truncated", "truncated" } } }
        if evidence.rows.is_empty() {
            div { class: "pc2-data-empty", "The query returned no rows." }
        } else {
            div { class: "pc2-data-scroll",
                table { class: "pc2-data-table",
                    thead { tr { for column in &visible_columns { th { title: "{column}", "{column}" } } } }
                    tbody { for (row_index, row) in evidence.rows.iter().enumerate() { tr { key: "row-{row_index}", for column in &visible_columns { td { CellValue { value: table_value(row, column).clone(), limit: MAX_CELL_CHARS } } } } } }
                }
            }
        }
        footer {
            span { "Bounded at {evidence.max_rows} rows / {byte_budget}" }
            if hidden_columns > 0 { span { "+{hidden_columns} columns hidden" } }
            if evidence.truncated { span { "The server truncated this result before rendering." } }
        }
    } }
}

#[component]
fn CellValue(value: Value, limit: usize) -> Element {
    let (preview, truncated) = bounded_text(&value, limit);
    let full_value = value_text(&value, true);
    let kind = match &value {
        Value::Null => "null",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) | Value::Object(_) => "structured",
        Value::String(_) => "text",
    };
    let mut expanded = use_signal(|| false);
    rsx! {
        if truncated {
            button { class: "pc2-cell-value pc2-cell-expand {kind}", title: "Open full cell value", aria_label: "Open full cell value", onclick: move |_| expanded.set(true), span { "{preview}" } i { "↗" } }
        } else {
            span { class: "pc2-cell-value {kind}", "{preview}" }
        }
        if expanded() {
            div { class: "pc2-cell-modal-backdrop", role: "presentation", onclick: move |_| expanded.set(false),
                section { class: "pc2-cell-modal", role: "dialog", aria_modal: "true", aria_label: "Full cell value", tabindex: "-1", onclick: move |event| event.stop_propagation(), onkeydown: move |event| if event.key() == Key::Escape { expanded.set(false); },
                    header { div { strong { "Full cell value" } span { "{kind}" } } button { aria_label: "Close full cell value", onclick: move |_| expanded.set(false), "×" } }
                    pre { "{full_value}" }
                    footer { span { "{full_value.chars().count()} characters" } button { class: "button primary", onclick: move |_| expanded.set(false), "Close" } }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CompactSpanGroup {
    key: String,
    label: String,
    call_id: Option<String>,
    entries: Vec<TurnSummary>,
    first_seq: u64,
    last_seq: u64,
    tool_calls: usize,
}

fn compact_span_groups(turns: &[TurnSummary]) -> Vec<CompactSpanGroup> {
    let mut groups = Vec::<CompactSpanGroup>::new();
    for (index, turn) in turns.iter().cloned().enumerate() {
        let key = turn
            .call_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("turn-{}", turn.id));
        let first_seq = turn
            .event_seqs
            .iter()
            .copied()
            .min()
            .unwrap_or(index as u64);
        let last_seq = turn.event_seqs.iter().copied().max().unwrap_or(first_seq);
        let tool_calls = turn.tool_names.len();
        let step_number = (turn.id.max(1) + 1) / 2;
        let action_name = turn.tool_names.first().cloned();
        if let Some(group) = groups.last_mut().filter(|group| group.key == key) {
            group.first_seq = group.first_seq.min(first_seq);
            group.last_seq = group.last_seq.max(last_seq);
            group.tool_calls += tool_calls;
            if let Some(action_name) = action_name {
                group.label = format!("Step {step_number} · {action_name}");
            }
            group.entries.push(turn);
            continue;
        }
        let label = action_name.map_or_else(
            || format!("Step {step_number}"),
            |name| format!("Step {step_number} · {name}"),
        );
        groups.push(CompactSpanGroup {
            key,
            label,
            call_id: turn.call_id.clone(),
            entries: vec![turn],
            first_seq,
            last_seq,
            tool_calls,
        });
    }
    groups
}

fn compact_preview(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "No response content".into()
    } else if normalized.chars().count() <= limit {
        normalized
    } else {
        format!(
            "{}…",
            normalized
                .chars()
                .take(limit.saturating_sub(1))
                .collect::<String>()
        )
    }
}

#[component]
pub fn TrajectoryView(
    turns: Vec<TurnSummary>,
    expanded_turn_id: Option<i64>,
    detail: Option<TurnDetail>,
    loading: bool,
    #[props(default = false)] embedded: bool,
    on_turn: EventHandler<i64>,
) -> Element {
    let groups = compact_span_groups(&turns);
    let total_events = turns
        .iter()
        .flat_map(|turn| turn.event_seqs.iter().copied())
        .max()
        .map_or(turns.len().max(1), |seq| seq as usize + 1);
    let total_refs = turns
        .iter()
        .map(|turn| turn.event_seqs.len())
        .sum::<usize>();
    let total_tools = turns
        .iter()
        .map(|turn| turn.tool_names.len())
        .sum::<usize>();
    let class = if embedded {
        "pc2-trajectory-component embedded"
    } else {
        "pc2-trajectory-component"
    };
    rsx! { div { class,
        div { class: "span-summary", span { strong { "{groups.len()} spans" } " · {total_refs} event references" } span { "Sequence window 0 — {total_events.saturating_sub(1)}" } }
        div { class: "span-table", role: "tree", aria_label: "Trajectory span hierarchy",
            div { class: "span-table-head", div { "Structure" } div { "Overview" } div { class: "span-axis-head", span { "Timeline / occupancy" } div { class: "span-axis-ticks", span { "0" } span { "25%" } span { "50%" } span { "75%" } span { "{total_events.saturating_sub(1)}" } } } div { "Evidence" } }
            details { class: "trace-root", open: true,
                summary { class: "trace-root-summary", div { class: "span-structure root", span { class: "disclosure" } strong { "trajectory" } span { "{groups.len()} spans" } } div { class: "span-row-copy root-copy", "{total_refs} canonical references across the loaded run" } div { class: "span-track", div { class: "span-bar root-bar", style: "left:0%;width:100%" } } div { class: "span-evidence-count", strong { "{total_refs} ev" } span { "{total_tools} tools" } } }
                div { class: "span-children", for group in groups { CompactSpanRow { key: "{group.key}", group, total_events, expanded_turn_id, detail: detail.clone(), loading, embedded, on_turn } } }
            }
        }
    } }
}

#[component]
fn CompactSpanRow(
    group: CompactSpanGroup,
    total_events: usize,
    expanded_turn_id: Option<i64>,
    detail: Option<TurnDetail>,
    loading: bool,
    embedded: bool,
    on_turn: EventHandler<i64>,
) -> Element {
    let event_refs = group
        .entries
        .iter()
        .map(|turn| turn.event_seqs.len())
        .sum::<usize>();
    let roles = group
        .entries
        .iter()
        .map(|turn| turn.source.as_str())
        .collect::<Vec<_>>()
        .join(" → ");
    let preview = group
        .entries
        .iter()
        .rev()
        .find(|turn| !turn.preview.trim().is_empty())
        .map_or_else(
            || "No response content".into(),
            |turn| compact_preview(&turn.preview, 120),
        );
    let model = group
        .entries
        .iter()
        .rev()
        .find_map(|turn| turn.model_name.clone());
    let latency = group
        .entries
        .iter()
        .filter_map(|turn| turn.latency_ms)
        .sum::<f64>();
    let denominator = total_events.max(1) as f64;
    let left = group.first_seq as f64 / denominator * 100.0;
    let width = (group.last_seq.saturating_sub(group.first_seq) + 1) as f64 / denominator * 100.0;
    let phase = if group.tool_calls > 0 {
        "tool"
    } else {
        "model"
    };
    let has_error = group.entries.iter().any(|turn| turn.has_error);
    rsx! { details { class: "span-row",
        summary { class: "span-row-summary", div { class: "span-structure", span { class: "disclosure" } span { class: "span-status {phase}" } div { strong { title: "{group.label}", "{group.label}" } span { "{roles} · seq {group.first_seq}–{group.last_seq}" } } if has_error { span { class: "pc2-error-chip", "error" } } else { span { class: "phase-badge {phase}", "{phase}" } } } div { class: "span-row-copy", strong { title: "{preview}", "{preview}" } div { class: "span-copy-meta", if let Some(model) = model { span { "{model}" } } if latency > 0.0 { span { "{format_ms(latency)}" } } if group.tool_calls > 0 { span { "{group.tool_calls} tool calls" } } } } div { class: "span-track", title: "seq {group.first_seq} — {group.last_seq}", div { class: "span-grid-lines" } div { class: "span-bar {phase}", style: "left:{left:.4}%;width:max({width:.4}%,3px)" } } div { class: "span-evidence-count", strong { "{event_refs} ev" } span { "{group.tool_calls} tools" } } }
        div { class: "span-detail", div { class: "span-detail-meta", code { "seq {group.first_seq}..{group.last_seq}" } if let Some(call_id) = &group.call_id { code { "call {call_id}" } } } for turn in group.entries { CompactTurnRow { key: "turn-{turn.id}", turn: turn.clone(), expanded: expanded_turn_id == Some(turn.id), detail: detail.clone(), loading, embedded, on_turn } } }
    } }
}

#[component]
fn CompactTurnRow(
    turn: TurnSummary,
    expanded: bool,
    detail: Option<TurnDetail>,
    loading: bool,
    embedded: bool,
    on_turn: EventHandler<i64>,
) -> Element {
    let id = turn.id;
    let kind = turn.kind.clone().unwrap_or_else(|| "turn".into());
    let preview = compact_preview(&turn.preview, 180);
    let tool_count = turn.tool_names.len();
    let event_count = turn.event_seqs.len();
    if embedded {
        return rsx! { button { class: "compact-turn pc2-embedded-turn", onclick: move |_| on_turn.call(id), span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", "{preview}" } span { class: "compact-turn-stats", if tool_count > 0 { span { "{tool_count} tools" } } span { "{event_count} ev" } } } };
    }
    rsx! { details { class: if expanded { "compact-turn selected" } else { "compact-turn" }, open: expanded,
        summary { aria_label: "Expand {turn.source} turn {id}", onclick: move |event| { event.prevent_default(); on_turn.call(id); }, span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", "{preview}" } span { class: "compact-turn-stats", if tool_count > 0 { span { "{tool_count} tools" } } span { "{event_count} ev" } } }
        if expanded { div { class: "compact-turn-body pc2-inline-detail", if loading { div { class: "pc2-inline-loading", span { class: "spinner" } "Loading full turn…" } } else if let Some(value) = detail.filter(|value| value.summary.id == id) { InlineTurnDetail { value } } else { div { class: "pc2-inline-unavailable", "Full evidence is unavailable for this turn." } } } }
    } }
}

#[component]
fn InlineTurnDetail(value: TurnDetail) -> Element {
    rsx! { div { class: "pc2-inline-detail-head", strong { "Full turn evidence" } }
        div { class: "pc2-inspector-facts", Fact { label: "Turn", value: format!("#{}", value.summary.id) } Fact { label: "Source", value: value.summary.source.clone() } Fact { label: "Kind", value: value.summary.kind.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Model", value: value.summary.model_name.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Latency", value: value.summary.latency_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "TTFT", value: value.summary.ttft_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Tokens", value: value.summary.total_tokens.map(|tokens| tokens.to_string()).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Token split", value: format!("{} in · {} out", optional_u64(value.summary.prompt_tokens), optional_u64(value.summary.completion_tokens)) } Fact { label: "Events", value: value.events.len().to_string() } }
        EvidenceBlock { title: "Message", value: value.turn.text() }
        if let Some(reasoning) = &value.turn.reasoning_content { EvidenceBlock { title: "Reasoning", value: reasoning.clone() } }
        if !value.wire_tool_calls.is_empty() { EvidenceBlock { title: "Tool calls", value: serde_json::to_string_pretty(&value.wire_tool_calls).unwrap_or_default() } }
        if let Some(observation) = &value.turn.observation { EvidenceBlock { title: "Observation", value: serde_json::to_string_pretty(observation).unwrap_or_default() } }
        if !value.events.is_empty() { EvidenceBlock { title: "Raw linked events", value: serde_json::to_string_pretty(&value.events).unwrap_or_default() } }
    }
}

#[component]
fn Fact(label: &'static str, value: String) -> Element {
    rsx! { div { span { "{label}" } code { "{value}" } } }
}

#[component]
fn EvidenceBlock(title: &'static str, value: String) -> Element {
    rsx! { details { class: "pc2-evidence-block", open: title == "Message", summary { "{title}" } pre { "{value}" } } }
}

fn format_ms(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.2}s", value / 1000.0)
    } else {
        format!("{value:.1}ms")
    }
}
fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "—".into())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_fences_round_trip() {
        let input = format!(
            "Before\n\n{}\n\nAfter",
            trajectory_fence("Hot turns", vec![2, 4])
        );
        let blocks = parse_rich_blocks(&input);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[1], RichBlock::Trajectory(value) if value.turn_ids == vec![2, 4]));
    }

    #[test]
    fn table_columns_are_stable_and_cover_scalar_rows() {
        let rows = vec![
            serde_json::json!({"b": 2, "a": 1}),
            serde_json::json!("tail"),
        ];
        assert_eq!(table_columns(&rows), vec!["a", "b", "value"]);
    }

    #[test]
    fn bounded_cells_do_not_keep_unbounded_content() {
        let (value, truncated) = bounded_text(&Value::String("abcdefghij".into()), 6);
        assert_eq!(value, "abcde…");
        assert!(truncated);
    }
}
