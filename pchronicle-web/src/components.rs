use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agenticmd_view::{AgenticMdRenderer, compact_metric_value, metrics_are_renderable};
use crate::chat_view::{
    TraceCard, chat_row_visible, group_chats, prompt_turn, source_class, step_row_visible,
};
use crate::json_value::{JsonValue, is_structured_json};
use crate::model::{
    EventProvenance, QueryEvidence, StorylineTurn, TurnDetail, TurnSummary, WireToolCall,
    extract_message_text,
};

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
            span { "Limited to {evidence.max_rows} rows / {byte_budget}" }
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
    overview: String,
    call_id: Option<String>,
    entries: Vec<TurnSummary>,
    first_seq: u64,
    last_seq: u64,
    tool_calls: usize,
    kind_chip: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
struct SeqBar {
    turn_id: i64,
    source: &'static str,
    left: f64,
    width: f64,
}

fn span_from_entries(
    key: String,
    label: String,
    overview: String,
    entries: Vec<TurnSummary>,
    fallback_index: usize,
    kind_chip: &'static str,
) -> CompactSpanGroup {
    let tool_calls = entries.iter().map(|turn| turn.tool_names.len()).sum();
    let call_id = entries
        .iter()
        .find_map(|turn| turn.call_id.clone().filter(|value| !value.is_empty()));
    let seqs = entries
        .iter()
        .flat_map(|turn| turn.event_seqs.iter().copied())
        .collect::<Vec<_>>();
    let (first_seq, last_seq) = if seqs.is_empty() {
        let first = fallback_index as u64;
        (
            first,
            first.saturating_add(entries.len().saturating_sub(1) as u64),
        )
    } else {
        (
            seqs.iter().copied().min().unwrap_or(0),
            seqs.iter().copied().max().unwrap_or(0),
        )
    };
    CompactSpanGroup {
        key,
        label,
        overview,
        call_id,
        entries,
        first_seq,
        last_seq,
        tool_calls,
        kind_chip,
    }
}

const MODALITY_ORDER: &[&str] = &["text", "image", "audio", "tool_call"];

fn union_modalities(entries: &[TurnSummary]) -> Vec<String> {
    MODALITY_ORDER
        .iter()
        .filter(|name| {
            entries
                .iter()
                .any(|turn| turn.modalities.iter().any(|item| item == *name))
        })
        .map(|name| (*name).to_string())
        .collect()
}

fn composition_label(entries: &[TurnSummary]) -> String {
    let users = entries.iter().filter(|turn| turn.source == "user").count();
    let agents = entries.iter().filter(|turn| turn.source == "agent").count();
    let systems = entries
        .iter()
        .filter(|turn| turn.source == "system")
        .count();
    let mut parts = Vec::new();
    if users > 0 {
        parts.push(format!("{users} user"));
    }
    if agents > 0 {
        parts.push(format!("{agents} agent"));
    }
    if systems > 0 {
        parts.push(format!("{systems} system"));
    }
    parts.join(" + ")
}

fn summary_modalities(entries: &[TurnSummary]) -> Vec<String> {
    union_modalities(entries)
        .into_iter()
        .filter(|modality| modality != "text" && modality != "tool_call")
        .collect()
}

fn row_char_count(entries: &[TurnSummary], user_only: bool) -> u64 {
    if user_only {
        entries
            .iter()
            .find(|turn| turn.source == "user")
            .map(|turn| turn.char_count)
            .unwrap_or(0)
    } else {
        entries.iter().map(|turn| turn.char_count).sum()
    }
}

fn format_char_count(count: u64) -> String {
    if count >= 1000 {
        format!("{:.1}k chars", count as f64 / 1000.0)
    } else {
        format!("{count} chars")
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "chat" => "Conversation",
        "system" => "System",
        "user" => "User",
        _ => "Agent",
    }
}

fn structure_meta(group: &CompactSpanGroup) -> String {
    let composition = composition_label(&group.entries);
    if group.kind_chip == "chat" {
        composition
    } else {
        format!(
            "{composition} · {}",
            format_char_count(row_char_count(&group.entries, false))
        )
    }
}

fn format_tool_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "tool" } else { "tools" })
}

fn tool_summary_label(tool_count: usize, tool_names: &[String]) -> String {
    if tool_count == 0 {
        return String::new();
    }
    if tool_names.is_empty() {
        return format_tool_count(tool_count);
    }
    let visible_names = tool_names.iter().take(2).cloned().collect::<Vec<_>>();
    let hidden_name_count = tool_names.len().saturating_sub(visible_names.len());
    let suffix = if hidden_name_count > 0 {
        format!(" +{hidden_name_count}")
    } else {
        String::new()
    };
    format!(
        "{} · {}{suffix}",
        format_tool_count(tool_count),
        visible_names.join(", ")
    )
}

fn group_diagnostic(entries: &[TurnSummary]) -> String {
    let turns = entries.len();
    let tokens = entries
        .iter()
        .filter_map(|turn| turn.total_tokens)
        .sum::<u64>();
    let latency = entries
        .iter()
        .filter_map(|turn| turn.latency_ms)
        .sum::<f64>();
    let tools = entries
        .iter()
        .map(|turn| turn.tool_names.len())
        .sum::<usize>();
    let mut parts = vec![
        if turns == 1 {
            "1 step".into()
        } else {
            format!("{turns} steps")
        },
        composition_label(entries),
    ];
    if tools > 0 {
        parts.push(format_tool_count(tools));
    }
    if tokens > 0 {
        parts.push(format!("{tokens} tokens"));
    }
    if latency > 0.0 {
        parts.push(format_ms(latency));
    }
    parts.join(" · ")
}

fn turn_expanded_facts(turn: &TurnSummary) -> String {
    let mut parts = Vec::new();
    if let Some(model) = &turn.model_name {
        parts.push(format!("Model {model}"));
    }
    if let Some(latency) = turn.latency_ms {
        parts.push(format!("Latency {}", format_ms(latency)));
    }
    if let Some(ttft) = turn.ttft_ms {
        parts.push(format!("TTFT {}", format_ms(ttft)));
    }
    if let Some(tokens) = turn.total_tokens {
        parts.push(format!("{tokens} tokens"));
    }
    if let Some(call_id) = &turn.call_id {
        if !call_id.is_empty() {
            parts.push(format!("Call {call_id}"));
        }
    }
    if let Some(timestamp) = &turn.timestamp {
        parts.push(timestamp.clone());
    }
    parts.join(" · ")
}

fn sequence_caption(first_seq: u64, last_seq: u64) -> String {
    format!("seq {first_seq}–{last_seq}")
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OccupancyRange {
    left: f64,
    width: f64,
}

fn occupancy_range(bars: &[SeqBar], ids: &[i64]) -> Option<OccupancyRange> {
    let mut min_left = f64::INFINITY;
    let mut max_right = f64::NEG_INFINITY;
    for bar in bars {
        if ids.contains(&bar.turn_id) {
            min_left = min_left.min(bar.left);
            max_right = max_right.max(bar.left + bar.width);
        }
    }
    if min_left.is_finite() && max_right > min_left {
        Some(OccupancyRange {
            left: min_left,
            width: max_right - min_left,
        })
    } else {
        None
    }
}

fn focus_line_left(bar: &SeqBar) -> f64 {
    bar.left + bar.width / 2.0
}

fn turn_collapsed_meta(turn: &TurnSummary) -> String {
    let mut parts = Vec::new();
    if let Some(latency) = turn.latency_ms {
        parts.push(format_ms(latency));
    }
    if !turn.tool_names.is_empty() {
        parts.push(format_tool_count(turn.tool_names.len()));
    }
    parts.join(" · ")
}

fn bar_emphasis(
    turn_id: i64,
    exposed: &[i64],
    focused: Option<i64>,
    hovered: &[i64],
) -> &'static str {
    if focused == Some(turn_id) {
        "focused"
    } else if exposed.contains(&turn_id) {
        "exposed"
    } else if hovered.contains(&turn_id) {
        "hovered"
    } else if focused.is_some() || !exposed.is_empty() || !hovered.is_empty() {
        "dimmed"
    } else {
        ""
    }
}

fn chat_overview(entries: &[TurnSummary]) -> String {
    entries
        .iter()
        .find(|turn| turn.source == "user")
        .map(|turn| compact_preview(&turn.preview, 180))
        .unwrap_or_else(|| "No user step".into())
}

fn step_overview(turn: &TurnSummary) -> String {
    compact_preview(&turn.preview, 180)
}

fn session_index_map(turns: &[TurnSummary]) -> HashMap<i64, usize> {
    turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.id, index))
        .collect()
}

fn session_axis_len(turns: &[TurnSummary]) -> usize {
    if turns.iter().any(|turn| !turn.event_seqs.is_empty()) {
        turns
            .iter()
            .flat_map(|turn| turn.event_seqs.iter().copied())
            .max()
            .map(|seq| seq as usize + 1)
            .unwrap_or(turns.len())
            .max(1)
    } else {
        turns.len().max(1)
    }
}

fn turn_session_span(turn: &TurnSummary, session_index: usize) -> (usize, usize) {
    match (
        turn.event_seqs.iter().copied().min(),
        turn.event_seqs.iter().copied().max(),
    ) {
        (Some(first), Some(last)) => (first as usize, last as usize),
        _ => (session_index, session_index),
    }
}

fn seq_bars(
    entries: &[TurnSummary],
    session_index: &HashMap<i64, usize>,
    axis_len: usize,
) -> Vec<SeqBar> {
    let axis_len = axis_len.max(1) as f64;
    entries
        .iter()
        .filter_map(|turn| {
            let index = *session_index.get(&turn.id)?;
            let (first, last) = turn_session_span(turn, index);
            Some(SeqBar {
                turn_id: turn.id,
                source: source_class(&turn.source),
                left: first as f64 / axis_len * 100.0,
                width: (last.saturating_sub(first) + 1) as f64 / axis_len * 100.0,
            })
        })
        .collect()
}

fn chat_span_groups(turns: &[TurnSummary]) -> Vec<CompactSpanGroup> {
    group_chats(turns)
        .into_iter()
        .enumerate()
        .map(|(index, card)| {
            let fallback = turns
                .iter()
                .position(|turn| card.contains_turn(turn.id))
                .unwrap_or(index);
            match card {
                TraceCard::Chat { user, replies } => {
                    let entries = user.into_iter().chain(replies).collect::<Vec<_>>();
                    let overview = chat_overview(&entries);
                    span_from_entries(
                        format!("chat-{index}"),
                        format!("Conversation {}", index + 1),
                        overview,
                        entries,
                        fallback,
                        "chat",
                    )
                }
                TraceCard::System { turn } => {
                    let overview = step_overview(&turn);
                    span_from_entries(
                        format!("system-{index}"),
                        "System".into(),
                        overview,
                        vec![turn],
                        fallback,
                        "system",
                    )
                }
            }
        })
        .collect()
}

fn step_span_groups(turns: &[TurnSummary]) -> Vec<CompactSpanGroup> {
    let mut groups = Vec::with_capacity(turns.len() * 2);
    for (index, turn) in turns.iter().cloned().enumerate() {
        if let Some(user) = prompt_turn(&turn) {
            groups.push(span_from_entries(
                format!("prompt-{}", turn.id),
                format!("Prompt for #{}", turn.id),
                step_overview(&user),
                vec![user],
                index,
                "user",
            ));
        }
        let id = turn.id;
        let overview = step_overview(&turn);
        let kind_chip = source_class(&turn.source);
        groups.push(span_from_entries(
            format!("turn-{id}"),
            format!("#{id}"),
            overview,
            vec![turn],
            index,
            kind_chip,
        ));
    }
    groups
}

fn compact_preview(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        "No text".into()
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

#[derive(Clone, Debug, PartialEq)]
struct HighlightPart {
    text: String,
    matched: bool,
}

fn highlight_parts(text: &str, query: &str) -> Vec<HighlightPart> {
    let query = query.trim();
    if query.is_empty() {
        return vec![HighlightPart {
            text: text.to_string(),
            matched: false,
        }];
    }

    // ASCII lower-casing preserves UTF-8 byte offsets, so the original text
    // can be sliced safely while still making the common English search case
    // insensitive. Non-ASCII terms (including Chinese) remain exact matches.
    let lowered_text = text.to_ascii_lowercase();
    let lowered_query = query.to_ascii_lowercase();
    let mut parts = Vec::new();
    let mut cursor = 0;
    for (start, _) in lowered_text.match_indices(&lowered_query) {
        if start < cursor {
            continue;
        }
        if start > cursor {
            parts.push(HighlightPart {
                text: text[cursor..start].to_string(),
                matched: false,
            });
        }
        let end = start + lowered_query.len();
        parts.push(HighlightPart {
            text: text[start..end].to_string(),
            matched: true,
        });
        cursor = end;
    }
    if cursor < text.len() {
        parts.push(HighlightPart {
            text: text[cursor..].to_string(),
            matched: false,
        });
    }
    if parts.is_empty() {
        parts.push(HighlightPart {
            text: text.to_string(),
            matched: false,
        });
    }
    parts
}

#[component]
pub fn HighlightedText(text: String, #[props(default)] query: String) -> Element {
    let parts = highlight_parts(&text, &query);
    rsx! {
        for (index, part) in parts.into_iter().enumerate() {
            if part.matched {
                mark { key: "hit-{index}", class: "pc2-search-hit", "{part.text}" }
            } else {
                span { key: "text-{index}", "{part.text}" }
            }
        }
    }
}

#[component]
pub fn TrajectoryView(
    turns: Vec<TurnSummary>,
    expanded_turn_id: Option<i64>,
    detail: Option<TurnDetail>,
    loading: bool,
    #[props(default = false)] embedded: bool,
    #[props(default = "chats".to_string())] view: String,
    #[props(default = "all".to_string())] source: String,
    #[props(default)] query: String,
    on_turn: EventHandler<i64>,
    #[props(default)] on_open_drawer: EventHandler<(i64, String, Vec<i64>)>,
) -> Element {
    let mut open_key = use_signal(|| None::<String>);
    let mut last_focus = use_signal(|| None::<i64>);
    let mut hovered_ids = use_signal(Vec::<i64>::new);
    let class = if embedded {
        "pc2-trajectory-component embedded"
    } else {
        "pc2-trajectory-component"
    };
    let groups = if view == "steps" {
        let visible = turns
            .iter()
            .filter(|turn| step_row_visible(turn, &source, &query))
            .cloned()
            .collect::<Vec<_>>();
        step_span_groups(&visible)
    } else {
        chat_span_groups(&turns)
            .into_iter()
            .filter(|group| chat_row_visible(&group.entries, &source, &query))
            .collect::<Vec<_>>()
    };
    let noun = if view == "steps" {
        "steps"
    } else {
        "conversations"
    };
    if groups.is_empty() {
        return rsx! {
            div { class: "pc2-empty pc2-trajectory-empty",
                strong { "No visible {noun}" }
                span { "No loaded {noun} match this filter." }
            }
        };
    }
    let session_index = session_index_map(&turns);
    let axis_len = session_axis_len(&turns);
    let root_bars = seq_bars(&turns, &session_index, axis_len);
    let total_refs = turns
        .iter()
        .map(|turn| turn.event_seqs.len())
        .sum::<usize>();
    let total_tools = turns
        .iter()
        .map(|turn| turn.tool_names.len())
        .sum::<usize>();
    let root_tool_label = format_tool_count(total_tools);
    let root_modalities = summary_modalities(&turns);
    let root_meta = composition_label(&turns);
    let root_drawer_id = turns.iter().find(|turn| turn.id >= 0).map(|turn| turn.id);
    let root_drawer_ids = turns
        .iter()
        .filter(|turn| turn.id >= 0)
        .map(|turn| turn.id)
        .collect::<Vec<_>>();
    if last_focus() != expanded_turn_id {
        last_focus.set(expanded_turn_id);
        if let Some(id) = expanded_turn_id {
            if let Some(group) = groups
                .iter()
                .find(|group| group.entries.iter().any(|turn| turn.id == id))
            {
                open_key.set(Some(group.key.clone()));
            }
        }
    }
    // Visibility tracking is intentionally omitted here. Dioxus's synthetic
    // `onvisible` event traps in the WASM event bridge for these rows in some
    // browsers; focus and hover still provide the sequence emphasis affordance.
    let exposed_ids = Vec::new();
    let hover_ids = hovered_ids();
    let table_class =
        if expanded_turn_id.is_some() || !exposed_ids.is_empty() || !hover_ids.is_empty() {
            "span-table has-emphasis"
        } else {
            "span-table"
        };
    let expose_range = occupancy_range(&root_bars, &exposed_ids);
    let focus_left = expanded_turn_id.and_then(|id| {
        root_bars
            .iter()
            .find(|bar| bar.turn_id == id)
            .map(focus_line_left)
    });
    let root_caption = sequence_caption(0, axis_len.saturating_sub(1) as u64);
    rsx! { div { class,
        div { class: "span-summary", span { strong { "{groups.len()} {noun}" } " · {total_refs} event references" } span { "Sequence window 0 — {axis_len.saturating_sub(1)}" } }
        div { class: "{table_class}", role: "tree", aria_label: "Run step hierarchy",
            div { class: "span-sticky-chrome",
                div { class: "span-table-head", div { "Structure" } div { "Overview" } div { class: "span-axis-head", span { "Sequence / coverage" } div { class: "span-axis-ticks", span { "0" } span { "25%" } span { "50%" } span { "75%" } span { "{axis_len.saturating_sub(1)}" } } } div { "Details" } }
                div { class: "trace-root-summary", div { class: "span-structure root", div { div { class: "span-structure-title", strong { "run" } span { "{groups.len()} {noun}" } } div { class: "root-composition", if !root_meta.is_empty() { span { title: "{root_meta}", "{root_meta}" } } for modality in root_modalities { span { class: "modality-chip {modality}", "{modality}" } } } } } div { class: "span-row-copy root-copy" } OccupancyTrack { bars: root_bars.clone(), expose_range, focus_left, caption: root_caption, title: "Run coverage · {turns.len()} steps", exposed_ids: exposed_ids.clone(), expanded_turn_id, hovered_ids: hover_ids.clone() } div { class: "span-evidence-count", if total_refs > 0 { strong { "{total_refs} events" } } if total_tools > 0 { span { "{root_tool_label}" } } if !embedded { if let Some(id) = root_drawer_id { button { class: "pc2-conversation-drawer-button pc2-run-agenticmd-button", title: "Open run as AgenticMD", aria_label: "Open run as AgenticMD", onclick: move |event| { event.prevent_default(); event.stop_propagation(); on_open_drawer.call((id, "Run".to_string(), root_drawer_ids.clone())); }, "↗" } } } } }
            }
            div { class: "span-children", for group in groups {
                    CompactSpanRow {
                        key: "{group.key}",
                        session_index: session_index.clone(),
                        axis_len,
                        expanded_turn_id,
                        row_open: open_key() == Some(group.key.clone()),
                        expose_range,
                        focus_left,
                        exposed_ids: exposed_ids.clone(),
                        hovered_ids: hover_ids.clone(),
                        detail: detail.clone(),
                        loading,
                        embedded,
                        on_turn,
                        on_open_drawer,
                        on_open: move |key| open_key.set(key),
                        on_hover: move |ids| hovered_ids.set(ids),
                        query: query.clone(),
                        group,
                    }
                } }
        }
    } }
}

#[component]
fn CompactSpanRow(
    group: CompactSpanGroup,
    session_index: HashMap<i64, usize>,
    axis_len: usize,
    expanded_turn_id: Option<i64>,
    row_open: bool,
    expose_range: Option<OccupancyRange>,
    focus_left: Option<f64>,
    exposed_ids: Vec<i64>,
    hovered_ids: Vec<i64>,
    detail: Option<TurnDetail>,
    loading: bool,
    embedded: bool,
    on_turn: EventHandler<i64>,
    #[props(default)] on_open_drawer: EventHandler<(i64, String, Vec<i64>)>,
    on_open: EventHandler<Option<String>>,
    on_hover: EventHandler<Vec<i64>>,
    #[props(default)] query: String,
) -> Element {
    let event_refs = group
        .entries
        .iter()
        .map(|turn| turn.event_seqs.len())
        .sum::<usize>();
    let preview = group.overview.clone();
    let diagnostic = group_diagnostic(&group.entries);
    let modalities = summary_modalities(&group.entries);
    let meta = structure_meta(&group);
    let step_count = group.entries.len();
    let mut tool_names = group
        .entries
        .iter()
        .flat_map(|turn| turn.tool_names.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mut seen_tool_names = HashSet::new();
    tool_names.retain(|name| !name.trim().is_empty() && seen_tool_names.insert(name.clone()));
    let tool_summary = tool_summary_label(group.tool_calls, &tool_names);
    let kind = group.kind_chip;
    let kind_text = kind_label(kind);
    let bars = seq_bars(&group.entries, &session_index, axis_len);
    let caption = sequence_caption(group.first_seq, group.last_seq);
    let has_error = group.entries.iter().any(|turn| turn.has_error);
    let group_key = group.key.clone();
    let hover_ids = group.entries.iter().map(|turn| turn.id).collect::<Vec<_>>();
    let drawer_id = group
        .entries
        .iter()
        .find(|turn| turn.id >= 0)
        .map(|turn| turn.id);
    let drawer_ids = group
        .entries
        .iter()
        .filter(|turn| turn.id >= 0)
        .map(|turn| turn.id)
        .collect::<Vec<_>>();
    let drawer_label = group.label.clone();
    rsx! { details {
        class: if row_open { "span-row is-open" } else { "span-row" },
        open: row_open,
        onmouseenter: move |_| on_hover.call(hover_ids.clone()),
        onmouseleave: move |_| on_hover.call(Vec::new()),
        summary { class: "span-row-summary", onclick: move |event| { event.prevent_default(); on_open.call(if row_open { None } else { Some(group_key.clone()) }); },
            div { class: "span-structure", span { class: "disclosure" } div {
                div { class: "span-structure-title", strong { title: "{group.label}", "{group.label}" }
                    if kind != "chat" { span { class: "phase-badge {kind}", "{kind_text}" } }
                    if has_error { span { class: "pc2-error-chip", "error" } }
                    if step_count > 1 { span { class: "summary-chip", "{step_count} steps" } }
                    if !composition_label(&group.entries).is_empty() { span { class: "summary-chip", "{composition_label(&group.entries)}" } }
                    if !tool_summary.is_empty() { span { class: "summary-chip tool", title: "{tool_summary}", "{tool_summary}" } }
                    for modality in modalities { span { class: "modality-chip {modality}", "{modality}" } }
                    if event_refs > 0 { span { class: "summary-chip event", "{event_refs} events" } }
                }
            } }
            div { class: "span-row-copy",
                strong { class: "overview-line", title: "{preview}", HighlightedText { text: preview.clone(), query: query.clone() } }
                if row_open { span { class: "span-row-diagnostic", title: "{diagnostic}", "{diagnostic}" } }
            }
            OccupancyTrack { bars, expose_range, focus_left, caption: caption.clone(), title: "{caption} · {meta}", exposed_ids, expanded_turn_id, hovered_ids }
            div { class: "span-evidence-count", if event_refs > 0 { span { class: "span-count-chip event", "{event_refs} events" } } }
            if !embedded {
                if let Some(id) = drawer_id {
                    button { class: "pc2-conversation-drawer-button", title: "Open conversation as AgenticMD", aria_label: "Open {drawer_label} as AgenticMD", onclick: move |event| { event.prevent_default(); event.stop_propagation(); on_open_drawer.call((id, drawer_label.clone(), drawer_ids.clone())); }, "↗" }
                }
            }
        }
        if row_open {
            div { class: "span-detail", for turn in group.entries {
                CompactTurnRow {
                    key: "turn-{turn.id}",
                    turn: turn.clone(),
                    expanded: expanded_turn_id == Some(turn.id),
                    detail: detail.clone(),
                    loading,
                    embedded,
                    query: query.clone(),
                    on_turn,
                }
            } }
        }
    } }
}

#[component]
fn OccupancyTrack(
    bars: Vec<SeqBar>,
    expose_range: Option<OccupancyRange>,
    focus_left: Option<f64>,
    caption: String,
    title: String,
    exposed_ids: Vec<i64>,
    expanded_turn_id: Option<i64>,
    hovered_ids: Vec<i64>,
) -> Element {
    rsx! {
        div { class: "span-seq-cell",
            div { class: "span-track", title,
                div { class: "span-grid-lines" }
                if let Some(range) = expose_range {
                    div {
                        class: "span-expose-band",
                        style: "left:{range.left:.4}%;width:{range.width:.4}%",
                        title: "Steps visible in the current list",
                    }
                }
                for bar in bars {
                    div { class: "span-bar {bar.source} {bar_emphasis(bar.turn_id, &exposed_ids, expanded_turn_id, &hovered_ids)}", style: "left:{bar.left:.4}%;width:{bar.width:.4}%" }
                }
                if let Some(left) = focus_left {
                    div { class: "span-focus-line", style: "left:{left:.4}%", title: "Expanded step" }
                    div { class: "span-focus-dot", style: "left:{left:.4}%" }
                }
            }
            span { class: "span-seq-caption", "{caption}" }
        }
    }
}

#[component]
fn CompactTurnRow(
    turn: TurnSummary,
    expanded: bool,
    detail: Option<TurnDetail>,
    loading: bool,
    embedded: bool,
    #[props(default)] query: String,
    on_turn: EventHandler<i64>,
) -> Element {
    let id = turn.id;
    let kind = turn.kind.clone().unwrap_or_else(|| "step".into());
    let preview = compact_preview(&turn.preview, 180);
    let collapsed_meta = turn_collapsed_meta(&turn);
    let expanded_facts = turn_expanded_facts(&turn);
    let tool_count = turn.tool_names.len();
    let tool_label = format_tool_count(tool_count);
    let event_count = turn.event_seqs.len();
    if id < 0 {
        return rsx! {
            div { class: "compact-turn synthetic-prompt",
                span { class: "compact-turn-chevron" }
                span { class: "pc2-role user", "user" }
                span { class: "synthetic-prompt-kind", "initial prompt" }
                span { class: "compact-preview", title: "{preview}", HighlightedText { text: preview.clone(), query: query.clone() } }
            }
        };
    }
    if embedded {
        return rsx! { button { class: "compact-turn pc2-embedded-turn", onclick: move |_| on_turn.call(id), span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", HighlightedText { text: preview.clone(), query: query.clone() } } span { class: "compact-turn-stats", if tool_count > 0 { span { "{tool_label}" } } span { "{event_count} events" } } } };
    }
    rsx! { details { class: if expanded { "compact-turn selected" } else { "compact-turn" }, open: expanded,
        summary { aria_label: "Expand {turn.source} step {id}", onclick: move |event| { event.prevent_default(); on_turn.call(id); }, span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } if expanded { span { class: "compact-kind", "{expanded_facts}" } } else { span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", HighlightedText { text: preview.clone(), query: query.clone() } } span { class: "compact-turn-stats", if !collapsed_meta.is_empty() { span { "{collapsed_meta}" } } if tool_count > 0 { span { "{tool_label}" } } span { "{event_count} events" } } } }
        if expanded { div { class: "compact-turn-body pc2-inline-detail", if loading { div { class: "pc2-inline-loading", span { class: "spinner" } "Loading full step…" } } else if let Some(value) = detail.filter(|value| value.summary.id == id) { InlineTurnDetail { value, query: query.clone() } } else { div { class: "pc2-inline-unavailable", "Details are unavailable for this step." } } } }
    } }
}

#[component]
fn InlineTurnDetail(value: TurnDetail, #[props(default)] query: String) -> Element {
    let message = value.turn.message.clone();
    let message_text = value.turn.text();
    let message_is_text_bearing = extract_message_text(&message).is_some();
    let structured_message = is_structured_json(&message);
    let tool_capable_source = source_can_call_tools(&value.summary.source);
    let native_tool_calls = value.turn.tool_calls.clone().unwrap_or_default();
    let misplaced_tool_call_count = (!tool_capable_source).then_some(native_tool_calls.len());
    let embedded_from_message = if tool_capable_source {
        parse_embedded_tool_calls_from_text(&message_text)
    } else {
        Vec::new()
    };
    let has_embedded_tool_calls = !embedded_from_message.is_empty();
    let mut embedded_seen = HashSet::new();
    for call in &embedded_from_message {
        let _ = embedded_seen.insert((call.name.clone(), call.arguments.to_string()));
    }
    let deduped_wire_calls: Vec<WireToolCall> = if tool_capable_source {
        value
            .wire_tool_calls
            .into_iter()
            .filter(|call| {
                !embedded_seen.contains(&(call.name.clone(), call.arguments.to_string()))
            })
            .collect()
    } else {
        Vec::new()
    };
    let events = serde_json::to_value(&value.events).unwrap_or(Value::Array(Vec::new()));
    let event_block_title = match value.event_provenance {
        EventProvenance::Canonical => crate::terminology::RECORDED_EVENTS,
        EventProvenance::SyntheticFromStoryline => crate::terminology::RECONSTRUCTED_EVENTS,
    };
    let has_any_tool_calls = !embedded_from_message.is_empty()
        || !deduped_wire_calls.is_empty()
        || !native_tool_calls.is_empty();
    rsx! {
        div { class: "pc2-inspector-chips",
            Fact { label: "Step", value: format!("#{}", value.summary.id) }
            Fact { label: "Role", value: value.summary.source.clone() }
            if let Some(kind) = value.summary.kind.clone() { Fact { label: "Type", value: kind } }
            if let Some(model) = value.summary.model_name.clone() { Fact { label: "Model", value: model } }
            if let Some(latency) = value.summary.latency_ms { Fact { label: "Latency", value: format_ms(latency) } }
            if let Some(tokens) = value.summary.total_tokens { Fact { label: "Tokens", value: tokens.to_string() } }
        }
        if let Some(ttft) = value.summary.ttft_ms { Fact { label: "TTFT", value: format_ms(ttft) } }
        if value.summary.prompt_tokens.is_some() || value.summary.completion_tokens.is_some() {
            Fact { label: "Token split", value: format!("{} in · {} out", optional_u64(value.summary.prompt_tokens), optional_u64(value.summary.completion_tokens)) }
        }
        if !value.events.is_empty() {
            Fact { label: "Events", value: value.events.len().to_string() }
        }
        if !embedded_from_message.is_empty() {
            ToolCallCards { calls: embedded_from_message, observation: value.turn.observation.clone() }
        } else if !deduped_wire_calls.is_empty() {
            ToolCallCards { calls: deduped_wire_calls, observation: value.turn.observation.clone() }
        } else if tool_capable_source && !native_tool_calls.is_empty() {
            ToolCallCards {
                observation: value.turn.observation.clone(),
                calls: native_tool_calls
                    .into_iter()
                    .map(|call| WireToolCall {
                        id: Some(call.tool_call_id),
                        name: call.function_name,
                        arguments: call.arguments,
                        result: call.result,
                    })
                .collect()
            }
        }
        if misplaced_tool_call_count.is_some_and(|count| count > 0) {
            div { class: "pc2-protocol-anomaly",
                "Ignored {misplaced_tool_call_count.unwrap_or_default()} tool call(s) attached to a {value.summary.source} turn."
            }
        }
        if !has_embedded_tool_calls {
            if structured_message && !message_is_text_bearing {
                InlineSection { title: "Message", JsonValue { value: message } }
            } else {
                if !message_text.trim().is_empty() && message_text != "No text" {
                    InlineSection { title: "Message", pre { HighlightedText { text: message_text.clone(), query: query.clone() } } }
                }
            }
        }
        if let Some(reasoning) = &value.turn.reasoning_content {
            InlineSection { title: "Reasoning", pre { HighlightedText { text: reasoning.clone(), query: query.clone() } } }
        }
        if !has_any_tool_calls {
            if let Some(observation) = value.turn.observation.clone() {
                InlineSection { title: "Observation", ObservationChips { value: observation.clone() } ObservationBlock { value: observation, tone: "generic" } }
            }
        }
        if !value.events.is_empty() {
            InlineSection { title: event_block_title, JsonValue { value: events } }
        }
        if let Some(extra) = value.turn.extra.clone() {
            InlineSection { title: "Extra", JsonValue { value: extra } }
        }
        if let Some(metrics) = value
            .turn
            .metrics
            .as_ref()
            .and_then(compact_metric_value)
            .filter(metrics_are_renderable)
        {
            InlineSection { title: "Metrics", JsonValue { value: metrics } }
        }
    }
}

#[component]
pub fn StepDrawer(
    detail: Option<TurnDetail>,
    #[props(default = "Step details".to_string())] title: String,
    #[props(default)] conversation_turns: Vec<StorylineTurn>,
    #[props(default)] conversation_details: Vec<TurnDetail>,
    #[props(default)] requested_block_count: usize,
    loading: bool,
    on_close: EventHandler<MouseEvent>,
) -> Element {
    let Some(value) = detail else {
        return rsx! {
            div { class: "pc2-step-drawer-layer",
                button { class: "pc2-step-drawer-backdrop", aria_label: "Close step details", onclick: on_close }
                aside { class: "pc2-step-drawer", role: "dialog", aria_modal: "true", aria_label: "Step details",
                    header { class: "pc2-step-drawer-head",
                        div { strong { "{title}" } }
                        button { class: "pc2-step-drawer-close", aria_label: "Close step details", onclick: on_close, "×" }
                    }
                    div { class: "pc2-step-drawer-loading", if loading { span { class: "spinner" } "Loading step…" } else { "Details are unavailable for this step." } }
                }
            }
        };
    };
    let turn = value.turn.clone();
    let mut agenticmd_turns = if !conversation_details.is_empty() {
        conversation_details
            .iter()
            .map(|item| item.turn.clone())
            .collect::<Vec<_>>()
    } else if !conversation_turns.is_empty() {
        conversation_turns
    } else {
        vec![turn.clone()]
    };
    // Chat rows may contain a synthetic user prompt (negative id) generated
    // from an agent turn's `user_prompt`. It cannot be loaded through the
    // turn-detail API, so reconstruct it locally for the AgenticMD drawer.
    let mut synthetic_user_added = false;
    if agenticmd_turns
        .first()
        .is_some_and(|item| item.source == "agent")
    {
        if let Some(prompt) = value
            .summary
            .user_prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            let synthetic_id = value.summary.id.saturating_neg();
            if !agenticmd_turns.iter().any(|item| item.id == synthetic_id) {
                agenticmd_turns.insert(
                    0,
                    StorylineTurn {
                        id: synthetic_id,
                        kind: Some("llm.request".into()),
                        timestamp: value.turn.timestamp.clone(),
                        source: "user".into(),
                        message: Value::String(prompt.to_string()),
                        reasoning_content: None,
                        tool_calls: None,
                        observation: None,
                        metrics: None,
                        model_name: None,
                        latency_ms: None,
                        ttft_ms: None,
                        extra: None,
                    },
                );
                synthetic_user_added = true;
            }
        }
    }
    let drawer_wire_tool_calls = agenticmd_turns
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let detail_index = drawer_detail_index(index, synthetic_user_added);
            detail_index
                .and_then(|index| conversation_details.get(index))
                .map(|item| item.wire_tool_calls.clone())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let loaded_block_count = agenticmd_turns.len();
    let total_block_count = requested_block_count.max(loaded_block_count);
    let block_count_label = if total_block_count > loaded_block_count {
        format!("{loaded_block_count} / {total_block_count} blocks")
    } else {
        format!(
            "{loaded_block_count} {}",
            if loaded_block_count == 1 {
                "block"
            } else {
                "blocks"
            }
        )
    };
    rsx! {
        div { class: "pc2-step-drawer-layer",
            button { class: "pc2-step-drawer-backdrop", aria_label: "Close step details", onclick: on_close }
            aside { class: "pc2-step-drawer", role: "dialog", aria_modal: "true", aria_label: "{title}",
                header { class: "pc2-step-drawer-head",
                    div { class: "pc2-step-drawer-title",
                        strong { "AgenticMD" }
                        span { class: "pc2-step-drawer-kind", "{block_count_label}" }
                        if loading { span { class: "pc2-step-drawer-loading-state", role: "status", "Loading…" } }
                    }
                    button { class: "pc2-step-drawer-close", aria_label: "Close step details", onclick: on_close, "×" }
                }
                div { class: "pc2-step-drawer-scroll",
                    AgenticMdRenderer {
                        title: title.clone(),
                        turns: agenticmd_turns,
                        wire_tool_calls: drawer_wire_tool_calls,
                    }
                }
            }
        }
    }
}

fn source_can_call_tools(source: &str) -> bool {
    matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "agent" | "assistant" | "model"
    )
}

fn drawer_detail_index(index: usize, synthetic_user_added: bool) -> Option<usize> {
    if synthetic_user_added {
        index.checked_sub(1)
    } else {
        Some(index)
    }
}

#[component]
fn Fact(label: &'static str, value: String) -> Element {
    rsx! { span { class: "pc2-fact-chip", span { "{label}" } code { "{value}" } } }
}

#[component]
fn EvidenceBlock(
    title: &'static str,
    #[props(default = false)] open: bool,
    children: Element,
) -> Element {
    rsx! { details { class: "pc2-evidence-block", open, summary { "{title}" } {children} } }
}

/// A second-level content panel: the step itself remains the only expandable
/// row, while long detail content is clipped and can be expanded in place.
#[component]
fn InlineSection(
    title: &'static str,
    #[props(default = false)] default_expanded: bool,
    children: Element,
) -> Element {
    let mut expanded = use_signal(move || default_expanded);
    rsx! {
        section { class: if expanded() { "pc2-inline-section expanded" } else { "pc2-inline-section" },
            header { class: "pc2-inline-section-head", strong { "{title}" } }
            div { class: "pc2-inline-section-body", {children} }
            button { class: "pc2-inline-section-reveal", aria_expanded: expanded(), aria_label: if expanded() { "Collapse section" } else { "Expand section" }, title: if expanded() { "Collapse" } else { "Expand truncated content" }, onclick: move |_| expanded.set(!expanded()), if expanded() { "⌃" } else { "⌄" } }
        }
    }
}

#[component]
pub(crate) fn ToolCallCards(
    calls: Vec<WireToolCall>,
    #[props(default)] observation: Option<Value>,
) -> Element {
    rsx! {
        div { class: "pc2-tool-call-stack",
            for (index, call) in calls.into_iter().enumerate() {
                {
                    let tone = tool_type_tone(&call.name);
                    let linked_observation = call.result.as_ref().or_else(|| {
                        observation_for_call(observation.as_ref(), call.id.as_deref(), index)
                    });
                    rsx! { div { class: "pc2-tool-call-card pc2-tool-tone-{tone}",
                    div { class: "pc2-tool-call-header",
                        div { class: "pc2-tool-call-head-left",
                            span { class: "pc2-tool-call-type-chip pc2-tool-tone-{tone}", "{tone}" }
                            strong { "{clean_tool_call_name(&call.name)}" }
                        }
                        div { class: "pc2-tool-call-head-right",
                            if let Some(observation) = linked_observation {
                                ObservationChips { value: observation.clone(), tone: tone.to_string() }
                            }
                            span { class: "pc2-tool-call-meta", "{argument_count_label(&call.arguments)}" }
                            if let Some(id) = &call.id {
                                span { class: "pc2-tool-call-id", "#{id}" }
                            }
                        }
                    }
                    ToolCallBody { arguments: call.arguments.clone() }
                    if let Some(observation) = linked_observation {
                        ObservationBlock { value: observation.clone(), tone: tone.to_string() }
                    }
                } } }
            }
        }
    }
}

fn content_needs_expansion(value: &str) -> bool {
    value.chars().count() > 900 || value.lines().count() > 8
}

#[component]
fn ExpandableRegion(clipped: bool, #[props(default)] class: String, children: Element) -> Element {
    let mut expanded = use_signal(|| false);
    let state = if clipped {
        if expanded() { "expanded" } else { "clipped" }
    } else {
        ""
    };
    rsx! {
        div { class: "pc2-expandable-region {class} {state}",
            div { class: "pc2-expandable-region-body", {children} }
            if clipped {
                button {
                    class: "pc2-expandable-region-toggle",
                    aria_expanded: expanded(),
                    aria_label: if expanded() { "Collapse content" } else { "Expand truncated content" },
                    title: if expanded() { "Collapse" } else { "Expand truncated content" },
                    onclick: move |_| expanded.set(!expanded()),
                    if expanded() { "⌃" } else { "⌄" }
                }
            }
        }
    }
}

#[component]
fn ToolCallBody(arguments: Value) -> Element {
    let probe = format_tool_call_arg(&arguments);
    let clipped = content_needs_expansion(&probe);
    rsx! {
        ExpandableRegion { clipped, class: "pc2-tool-call-body-region".to_string(),
            div { class: "pc2-tool-call-body",
                if let Value::Object(args) = &arguments {
                    for (key, val) in args {
                        div { class: "pc2-tool-call-arg",
                            code { "{key}" }
                            span { "{format_tool_call_arg(val)}" }
                        }
                    }
                } else {
                    pre { "{arguments}" }
                }
            }
        }
    }
}

fn tool_type_tone(name: &str) -> &'static str {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("command") || normalized.contains("bash") || normalized.contains("shell")
    {
        "command_execution"
    } else if normalized.starts_with("mcp") || normalized.contains("browser") {
        "mcp"
    } else {
        "function"
    }
}

fn observation_for_call<'a>(
    value: Option<&'a Value>,
    call_id: Option<&str>,
    index: usize,
) -> Option<&'a Value> {
    let value = value?;
    let items = value
        .as_object()
        .and_then(|object| object.get("results"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .or_else(|| value.as_array().map(Vec::as_slice))
        .unwrap_or_else(|| std::slice::from_ref(value));
    call_id
        .and_then(|id| {
            items
                .iter()
                .find(|item| item.get("source_call_id").and_then(Value::as_str) == Some(id))
        })
        .or_else(|| items.get(index))
}

#[component]
fn ObservationBlock(value: Value, tone: String) -> Element {
    let output = observation_output(&value);
    rsx! {
        div { class: "pc2-tool-observation pc2-tool-tone-{tone}",
            if let Some(output) = output {
                ExpandableRegion { clipped: content_needs_expansion(&output), class: "pc2-tool-observation-region".to_string(),
                    pre { class: "pc2-tool-observation-output", "{output}" }
                }
            }
        }
    }
}

#[component]
fn ObservationChips(
    value: Value,
    #[props(default = "generic".to_string())] tone: String,
) -> Element {
    let exit_code = value.get("exit_code").and_then(Value::as_i64);
    let status = value.get("status").and_then(Value::as_str);
    let observed_type = value.get("type").and_then(Value::as_str);
    let type_label = if tone == "generic" {
        observed_type.unwrap_or("observation")
    } else {
        tone.as_str()
    };
    let show_type = observed_type.is_some() || tone != "generic";
    rsx! {
        div { class: "pc2-tool-observation-chips",
            if let Some(code) = exit_code { Fact { label: "exit_code", value: code.to_string() } }
            if let Some(status) = status { Fact { label: "status", value: status.to_string() } }
            if show_type {
                span { class: "pc2-fact-chip pc2-tool-observation-type-chip pc2-tool-tone-{tone}", span { "type" } code { "{type_label}" } }
            }
        }
    }
}

fn observation_output(value: &Value) -> Option<String> {
    if let Value::String(text) = value {
        return Some(text.clone());
    }
    let Some(object) = value.as_object() else {
        return Some(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()));
    };
    for key in ["content", "aggregated_output", "output", "result"] {
        if let Some(output) = object.get(key) {
            return Some(match output {
                Value::String(text) => text.clone(),
                _ => serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string()),
            });
        }
    }
    None
}

fn clean_tool_call_name(name: &str) -> String {
    clean_embedded_text(name)
}

/// Trim whitespace plus literal `\n` / `\r` / `\t` escape sequences that models
/// often emit around embedded tool call fields (e.g. `<parameter=command>\ncat foo\n</parameter>`).
fn clean_embedded_text(value: &str) -> String {
    let mut text = value.trim();
    while let Some(stripped) = text
        .strip_prefix("\\r\\n")
        .or_else(|| text.strip_prefix("\\n"))
        .or_else(|| text.strip_prefix("\\r"))
        .or_else(|| text.strip_prefix("\\t"))
    {
        text = stripped.trim_start();
    }
    while let Some(stripped) = text
        .strip_suffix("\\r\\n")
        .or_else(|| text.strip_suffix("\\n"))
        .or_else(|| text.strip_suffix("\\r"))
        .or_else(|| text.strip_suffix("\\t"))
    {
        text = stripped.trim_end();
    }
    text.to_string()
}

fn argument_count_label(arguments: &Value) -> String {
    match arguments {
        Value::Object(args) => format!(
            "{} arg{}",
            args.len(),
            if args.len() == 1 { "" } else { "s" }
        ),
        _ => "raw".to_string(),
    }
}

fn format_tool_call_arg(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
        Value::Object(obj) => serde_json::to_string(obj).unwrap_or_default(),
    }
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

fn parse_embedded_tool_calls_from_text(text: &str) -> Vec<WireToolCall> {
    let mut calls = Vec::new();
    let mut remaining = text;
    loop {
        let (after_offset, end_tag) = if let Some(offset) = remaining.find("<tool_call>") {
            (offset + "<tool_call>".len(), "</tool_call>")
        } else if let Some(offset) = remaining.find("<function=") {
            (offset + "<function=".len(), "</function>")
        } else {
            break;
        };
        let after = &remaining[after_offset..];
        let name_end = after.find(['>', '\n', '<']).unwrap_or(after.len());
        let name = clean_embedded_text(&after[..name_end]);
        let after_name = &after[name_end..];
        let (block, rest) = if let Some(end) = after_name.find(end_tag) {
            (&after_name[..end], &after_name[end + end_tag.len()..])
        } else {
            (after_name, "")
        };
        let mut arguments = serde_json::Map::new();
        let mut param_remaining = block;
        while let Some((_, after_param)) = param_remaining.split_once("<parameter=") {
            let Some((key, after_opening)) = after_param.split_once('>') else {
                break;
            };
            let key = key.trim();
            if key.is_empty() {
                param_remaining = after_opening;
                continue;
            }
            let (value, rest_param) = after_opening
                .split_once("</parameter>")
                .unwrap_or((after_opening, ""));
            arguments.insert(key.to_string(), Value::String(clean_embedded_text(value)));
            param_remaining = rest_param;
        }
        if !name.is_empty() {
            calls.push(WireToolCall {
                id: Some(format!(
                    "embedded-{name}-{}-{}-{}-0",
                    name.len(),
                    calls.len(),
                    text.len()
                )),
                name: name.to_string(),
                arguments: Value::Object(arguments),
                result: None,
            });
        }
        remaining = rest;
    }
    calls
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

    #[test]
    fn long_detail_content_gets_an_expand_affordance() {
        assert!(!content_needs_expansion("short\nvalue"));
        assert!(content_needs_expansion(&"x".repeat(901)));
        assert!(content_needs_expansion(&vec!["line"; 9].join("\n")));
    }

    #[test]
    fn search_highlighting_is_case_insensitive_without_changing_unicode_offsets() {
        let parts = highlight_parts("验证 FTS verification", "fts");
        assert_eq!(
            parts,
            vec![
                HighlightPart {
                    text: "验证 ".into(),
                    matched: false
                },
                HighlightPart {
                    text: "FTS".into(),
                    matched: true
                },
                HighlightPart {
                    text: " verification".into(),
                    matched: false
                },
            ]
        );
        let chinese = highlight_parts("用于验证中文", "验证");
        assert!(
            chinese
                .iter()
                .any(|part| part.matched && part.text == "验证")
        );
    }

    fn turn(id: i64, source: &str, seqs: &[u64]) -> TurnSummary {
        TurnSummary {
            id,
            source: source.into(),
            kind: None,
            timestamp: None,
            call_id: None,
            preview: format!("{source}-{id}"),
            user_prompt: None,
            char_count: 0,
            modalities: Vec::new(),
            model_name: None,
            latency_ms: None,
            ttft_ms: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            tool_names: Vec::new(),
            event_seqs: seqs.to_vec(),
            has_error: false,
        }
    }

    #[test]
    fn chats_keep_one_timeline_row_for_a_user_and_following_agents() {
        let turns = vec![
            turn(1, "user", &[0]),
            turn(2, "agent", &[2, 5]),
            turn(3, "agent", &[6]),
        ];
        let groups = chat_span_groups(&turns);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "Conversation 1");
        assert_eq!(groups[0].overview, "user-1");
        assert_eq!(
            groups[0]
                .entries
                .iter()
                .map(|turn| turn.id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!((groups[0].first_seq, groups[0].last_seq), (0, 6));
        let session_index = session_index_map(&turns);
        let bars = seq_bars(&groups[0].entries, &session_index, session_axis_len(&turns));
        assert_eq!(
            bars.iter().map(|bar| bar.source).collect::<Vec<_>>(),
            vec!["user", "agent", "agent"]
        );
        assert!((bars[0].left - 0.0).abs() < 1e-9);
        assert!((bars[0].width - 100.0 / 7.0).abs() < 1e-9);
        assert!((bars[1].left - 200.0 / 7.0).abs() < 1e-9);
        assert!((bars[1].width - 400.0 / 7.0).abs() < 1e-9);
        assert!((bars[2].left - 600.0 / 7.0).abs() < 1e-9);
    }

    #[test]
    fn chat_structure_uses_user_chars_and_union_modalities() {
        let mut user = turn(1, "user", &[0]);
        user.preview = "Please continue".into();
        user.char_count = 15;
        user.modalities = vec!["text".into()];
        let mut agent = turn(2, "agent", &[1]);
        agent.preview = "<tool_call>ls".into();
        agent.char_count = 80;
        agent.modalities = vec!["text".into(), "tool_call".into()];
        agent.tool_names = vec!["execute_bash".into()];
        let groups = chat_span_groups(&[user, agent]);
        assert_eq!(groups[0].kind_chip, "chat");
        assert_eq!(groups[0].overview, "Please continue");
        assert_eq!(
            union_modalities(&groups[0].entries),
            vec!["text", "tool_call"]
        );
        assert_eq!(row_char_count(&groups[0].entries, true), 15);
        assert_eq!(composition_label(&groups[0].entries), "1 user + 1 agent");
        assert_eq!(summary_modalities(&groups[0].entries), Vec::<String>::new());
        assert_eq!(format_char_count(15), "15 chars");
        assert_eq!(format_char_count(1200), "1.2k chars");
    }

    #[test]
    fn tool_summary_is_compact_and_does_not_repeat_names() {
        let names = vec![
            "bash_command".to_string(),
            "bash_command".to_string(),
            " ".to_string(),
            "mcp_search".to_string(),
            "browser_open".to_string(),
        ];
        let mut unique = Vec::new();
        let mut seen = HashSet::new();
        for name in names {
            if !name.trim().is_empty() && seen.insert(name.clone()) {
                unique.push(name);
            }
        }
        assert_eq!(
            tool_summary_label(4, &unique),
            "4 tools · bash_command, mcp_search +1"
        );
        assert_eq!(
            tool_summary_label(1, &["execute_bash".into()]),
            "1 tool · execute_bash"
        );
        assert_eq!(format_tool_count(1), "1 tool");
        assert_eq!(format_tool_count(2), "2 tools");
        assert_eq!(tool_summary_label(0, &unique), "");
    }

    #[test]
    fn chat_without_user_keeps_chat_kind_and_no_user_overview() {
        let mut agent = turn(2, "agent", &[0]);
        agent.modalities = vec!["text".into()];
        agent.char_count = 12;
        let groups = chat_span_groups(&[agent]);
        assert_eq!(groups[0].kind_chip, "chat");
        assert_eq!(groups[0].overview, "No user step");
        assert_eq!(row_char_count(&groups[0].entries, true), 0);
    }

    #[test]
    fn tool_calls_are_only_renderable_for_agent_roles() {
        assert!(source_can_call_tools("agent"));
        assert!(source_can_call_tools(" assistant "));
        assert!(!source_can_call_tools("user"));
        assert!(!source_can_call_tools("system"));
    }

    #[test]
    fn synthetic_user_does_not_steal_agent_tool_calls() {
        assert_eq!(drawer_detail_index(0, true), None);
        assert_eq!(drawer_detail_index(1, true), Some(0));
        assert_eq!(drawer_detail_index(2, true), Some(1));
        assert_eq!(drawer_detail_index(0, false), Some(0));
    }

    #[test]
    fn steps_empty_preview_reads_as_no_text() {
        let mut turn = turn(3, "agent", &[0]);
        turn.preview.clear();
        let groups = step_span_groups(&[turn]);
        assert_eq!(groups[0].kind_chip, "agent");
        assert_eq!(groups[0].overview, "No text");
    }

    #[test]
    fn expanded_chat_overview_is_diagnostics_not_user_text() {
        let mut user = turn(1, "user", &[0]);
        user.preview = "Please continue".into();
        user.total_tokens = Some(100);
        let mut agent = turn(2, "agent", &[1]);
        agent.preview = "running ls".into();
        agent.model_name = Some("glm".into());
        agent.latency_ms = Some(2020.0);
        agent.total_tokens = Some(400);
        agent.tool_names = vec!["execute_bash".into()];
        let diagnostic = group_diagnostic(&[user.clone(), agent.clone()]);
        assert!(diagnostic.contains("2 steps"));
        assert!(diagnostic.contains("1 user + 1 agent · 1 tool"));
        assert!(diagnostic.contains("500 tokens"));
        assert!(diagnostic.contains("2.02s"));
        assert!(!diagnostic.contains("seq"));
        assert!(!diagnostic.contains("Please continue"));
        assert!(!diagnostic.contains("running ls"));
        let expanded = turn_expanded_facts(&agent);
        assert!(expanded.contains("Model glm"));
        assert!(expanded.contains("Latency 2.02s"));
        assert!(!expanded.contains("running ls"));
        assert!(!expanded.contains("seq"));
        assert_eq!(sequence_caption(2, 3), "seq 2–3");
        assert!(!structure_meta(&chat_span_groups(&[user, agent])[0]).contains("seq"));
    }

    #[test]
    fn occupancy_marks_split_exposed_range_from_expanded_turn() {
        let turns = vec![
            turn(1, "user", &[0]),
            turn(2, "agent", &[1]),
            turn(3, "user", &[2]),
            turn(4, "agent", &[3]),
        ];
        let session_index = session_index_map(&turns);
        let bars = seq_bars(&turns, &session_index, session_axis_len(&turns));
        let exposed = occupancy_range(&bars, &[1, 2]).expect("early turns form a range");
        assert!((exposed.left - 0.0).abs() < 1e-9);
        assert!((exposed.width - 50.0).abs() < 1e-9);
        let focus = focus_line_left(bars.iter().find(|bar| bar.turn_id == 4).unwrap());
        assert!((focus - 87.5).abs() < 1e-9);
        assert!(occupancy_range(&bars, &[]).is_none());
    }

    #[test]
    fn sequence_emphasis_orders_focus_viewport_then_hover() {
        assert_eq!(bar_emphasis(2, &[1, 2], Some(2), &[2]), "focused");
        assert_eq!(bar_emphasis(1, &[1, 2], Some(2), &[1]), "exposed");
        assert_eq!(bar_emphasis(8, &[1, 2], Some(2), &[8]), "hovered");
        assert_eq!(bar_emphasis(9, &[1, 2], Some(2), &[]), "dimmed");
        assert_eq!(bar_emphasis(9, &[], None, &[]), "");
    }

    #[test]
    fn sequence_axis_keeps_session_relative_position_and_type_colors() {
        let turns = vec![
            turn(1, "system", &[]),
            turn(2, "user", &[]),
            turn(3, "agent", &[]),
            turn(4, "user", &[]),
        ];
        let session_index = session_index_map(&turns);
        let axis_len = session_axis_len(&turns);
        let root = seq_bars(&turns, &session_index, axis_len);
        assert_eq!(
            root.iter().map(|bar| bar.source).collect::<Vec<_>>(),
            vec!["system", "user", "agent", "user"]
        );
        assert!((root[0].width - 25.0).abs() < 1e-9);
        assert!((root[3].left - 75.0).abs() < 1e-9);

        let later_chat = seq_bars(&turns[3..], &session_index, axis_len);
        assert_eq!(later_chat[0].source, "user");
        assert!((later_chat[0].left - 75.0).abs() < 1e-9);
        assert!((later_chat[0].width - 25.0).abs() < 1e-9);
    }

    #[test]
    fn source_filter_keeps_chat_members() {
        let turns = vec![
            turn(1, "user", &[0]),
            turn(2, "agent", &[2]),
            turn(3, "system", &[4]),
        ];
        let visible = chat_span_groups(&turns)
            .into_iter()
            .filter(|group| chat_row_visible(&group.entries, "agent", ""))
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible[0]
                .entries
                .iter()
                .map(|turn| turn.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(visible[0].overview, "user-1");
    }

    #[test]
    fn steps_keep_one_timeline_row_per_turn() {
        let turns = vec![turn(1, "user", &[0]), turn(2, "agent", &[3])];
        let groups = step_span_groups(&turns);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "#1");
        assert_eq!(groups[1].first_seq, 3);
    }

    #[test]
    fn parse_embedded_tool_calls_from_text_extracts_multiple_calls() {
        let text = "<tool_call>execute_bash<parameter=command>cat /workspace/README.md</parameter></tool_call><tool_call>execute_bash<parameter=command>ls</parameter></tool_call>";
        let calls = parse_embedded_tool_calls_from_text(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "execute_bash");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"command": "cat /workspace/README.md"})
        );
        assert_eq!(calls[1].name, "execute_bash");
        assert_eq!(calls[1].arguments, serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn parse_embedded_function_call_from_text_extracts_parameters() {
        let text = "<function=execute_bash><parameter=command>pwd</parameter></function>";
        let calls = parse_embedded_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "execute_bash");
        assert_eq!(calls[0].arguments, serde_json::json!({"command": "pwd"}));
    }

    #[test]
    fn parse_embedded_tool_call_strips_literal_escape_sequences() {
        let text = "<tool_call>execute_bash\\n<parameter=command>\\ncat /workspace/README.md\\n</parameter>";
        let calls = parse_embedded_tool_calls_from_text(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "execute_bash");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"command": "cat /workspace/README.md"})
        );
    }

    #[test]
    fn observation_output_renders_scalar_tool_results() {
        assert_eq!(
            observation_output(&serde_json::json!("command output")),
            Some("command output".into())
        );
        assert_eq!(
            observation_output(&serde_json::json!(42)),
            Some("42".into())
        );
    }
}
