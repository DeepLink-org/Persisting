use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        parts.push(format!("{tools} tool"));
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
        parts.push(format!("{} tool", turn.tool_names.len()));
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

#[component]
pub fn TrajectoryView(
    turns: Vec<TurnSummary>,
    expanded_turn_id: Option<i64>,
    detail: Option<TurnDetail>,
    loading: bool,
    #[props(default)] context: Option<Vec<StorylineTurn>>,
    #[props(default = false)] embedded: bool,
    #[props(default = "chats".to_string())] view: String,
    #[props(default = "all".to_string())] source: String,
    #[props(default)] query: String,
    #[props(default = false)] context_loading: bool,
    on_turn: EventHandler<i64>,
    #[props(default)] on_context: EventHandler<()>,
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
    let root_modalities = summary_modalities(&turns);
    let root_meta = composition_label(&turns);
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
                div { class: "trace-root-summary", div { class: "span-structure root", div { div { class: "span-structure-title", strong { "run" } span { "{groups.len()} {noun}" } } div { class: "span-structure-chips", for modality in root_modalities { span { class: "modality-chip {modality}", "{modality}" } } } span { "{root_meta}" } } } div { class: "span-row-copy root-copy" } OccupancyTrack { bars: root_bars.clone(), expose_range, focus_left, caption: root_caption, title: "Run coverage · {turns.len()} steps", exposed_ids: exposed_ids.clone(), expanded_turn_id, hovered_ids: hover_ids.clone() } div { class: "span-evidence-count", if total_refs > 0 { strong { "{total_refs} events" } } if total_tools > 0 { span { "{total_tools} tools" } } } }
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
                        context: context.clone(),
                        context_loading,
                        on_turn,
                        on_context,
                        on_open: move |key| open_key.set(key),
                        on_hover: move |ids| hovered_ids.set(ids),
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
    #[props(default)] context: Option<Vec<StorylineTurn>>,
    #[props(default = false)] context_loading: bool,
    on_turn: EventHandler<i64>,
    #[props(default)] on_context: EventHandler<()>,
    on_open: EventHandler<Option<String>>,
    on_hover: EventHandler<Vec<i64>>,
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
    let kind = group.kind_chip;
    let kind_text = kind_label(kind);
    let bars = seq_bars(&group.entries, &session_index, axis_len);
    let caption = sequence_caption(group.first_seq, group.last_seq);
    let has_error = group.entries.iter().any(|turn| turn.has_error);
    let group_key = group.key.clone();
    let hover_ids = group.entries.iter().map(|turn| turn.id).collect::<Vec<_>>();
    rsx! { details {
        class: if row_open { "span-row is-open" } else { "span-row" },
        open: row_open,
        onmouseenter: move |_| on_hover.call(hover_ids.clone()),
        onmouseleave: move |_| on_hover.call(Vec::new()),
        summary { class: "span-row-summary", onclick: move |event| { event.prevent_default(); on_open.call(if row_open { None } else { Some(group_key.clone()) }); },
            div { class: "span-structure", span { class: "disclosure" } div { div { class: "span-structure-title", strong { title: "{group.label}", "{group.label}" } span { class: "phase-badge {kind}", "{kind_text}" } if has_error { span { class: "pc2-error-chip", "error" } } } div { class: "span-structure-chips", for modality in modalities { span { class: "modality-chip {modality}", "{modality}" } } } span { "{meta}" } } }
            div { class: "span-row-copy",
                strong { class: "overview-line", title: "{preview}", "{preview}" }
                if row_open { span { class: "span-row-diagnostic", title: "{diagnostic}", "{diagnostic}" } }
            }
            OccupancyTrack { bars, expose_range, focus_left, caption: caption.clone(), title: "{caption} · {meta}", exposed_ids, expanded_turn_id, hovered_ids }
            div { class: "span-evidence-count", if event_refs > 0 { strong { "{event_refs} events" } } if group.tool_calls > 0 { span { "{group.tool_calls} tools" } } }
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
                    context: context.clone(),
                    context_loading,
                    on_turn,
                    on_context,
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
    #[props(default)] context: Option<Vec<StorylineTurn>>,
    #[props(default = false)] context_loading: bool,
    on_turn: EventHandler<i64>,
    #[props(default)] on_context: EventHandler<()>,
) -> Element {
    let id = turn.id;
    let kind = turn.kind.clone().unwrap_or_else(|| "step".into());
    let preview = compact_preview(&turn.preview, 180);
    let collapsed_meta = turn_collapsed_meta(&turn);
    let expanded_facts = turn_expanded_facts(&turn);
    let tool_count = turn.tool_names.len();
    let event_count = turn.event_seqs.len();
    if id < 0 {
        return rsx! {
            div { class: "compact-turn synthetic-prompt",
                span { class: "compact-turn-chevron" }
                span { class: "pc2-role user", "user" }
                span { class: "synthetic-prompt-kind", "initial prompt" }
                span { class: "compact-preview", title: "{preview}", "{preview}" }
            }
        };
    }
    if embedded {
        return rsx! { button { class: "compact-turn pc2-embedded-turn", onclick: move |_| on_turn.call(id), span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", "{preview}" } span { class: "compact-turn-stats", if tool_count > 0 { span { "{tool_count} tools" } } span { "{event_count} events" } } } };
    }
    rsx! { details { class: if expanded { "compact-turn selected" } else { "compact-turn" }, open: expanded,
        summary { aria_label: "Expand {turn.source} step {id}", onclick: move |event| { event.prevent_default(); on_turn.call(id); }, span { class: "compact-turn-chevron" } span { class: "pc2-role {turn.source}", "{turn.source}" } code { "#{id}" } if expanded { span { class: "compact-kind", "{expanded_facts}" } } else { span { class: "compact-kind", "{kind}" } span { class: "compact-preview", title: "{preview}", "{preview}" } span { class: "compact-turn-stats", if !collapsed_meta.is_empty() { span { "{collapsed_meta}" } } if tool_count > 0 { span { "{tool_count} tools" } } span { "{event_count} events" } } } }
        if expanded { div { class: "compact-turn-body pc2-inline-detail", if loading { div { class: "pc2-inline-loading", span { class: "spinner" } "Loading full step…" } } else if let Some(value) = detail.filter(|value| value.summary.id == id) { InlineTurnDetail { value, context: context.clone(), context_loading, on_context } } else { div { class: "pc2-inline-unavailable", "Details are unavailable for this step." } } } }
    } }
}

#[component]
fn InlineTurnDetail(
    value: TurnDetail,
    #[props(default)] context: Option<Vec<StorylineTurn>>,
    #[props(default = false)] context_loading: bool,
    #[props(default)] on_context: EventHandler<()>,
) -> Element {
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
    let tool_block_title = if embedded_from_message.len() == 1 {
        "Tool call"
    } else {
        "Tool calls"
    };
    let focus_id = value.summary.id;
    rsx! {
        ContextRebuild { turns: context, focus_id, loading: context_loading, on_load: on_context }
        div { class: "pc2-inspector-facts", Fact { label: "Step", value: format!("#{}", value.summary.id) } Fact { label: "Role", value: value.summary.source.clone() } Fact { label: "Type", value: value.summary.kind.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Model", value: value.summary.model_name.clone().unwrap_or_else(|| "unavailable".into()) } Fact { label: "Latency", value: value.summary.latency_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "TTFT", value: value.summary.ttft_ms.map(format_ms).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Tokens", value: value.summary.total_tokens.map(|tokens| tokens.to_string()).unwrap_or_else(|| "unavailable".into()) } Fact { label: "Token split", value: format!("{} in · {} out", optional_u64(value.summary.prompt_tokens), optional_u64(value.summary.completion_tokens)) } Fact { label: "Events", value: value.events.len().to_string() } }
        if !embedded_from_message.is_empty() {
            EvidenceBlock { title: tool_block_title, open: true, ToolCallCards { calls: embedded_from_message } }
        } else if !deduped_wire_calls.is_empty() {
            EvidenceBlock { title: "Tool calls", ToolCallCards { calls: deduped_wire_calls } }
        } else if !native_tool_calls.is_empty() {
            EvidenceBlock {
                title: "Tool calls",
                ToolCallCards {
                    calls: native_tool_calls
                        .into_iter()
                        .map(|call| WireToolCall {
                        id: Some(call.tool_call_id),
                        name: call.function_name,
                        arguments: call.arguments,
                    })
                    .collect()
                }
            }
        }
        if misplaced_tool_call_count.is_some_and(|count| count > 0) {
            div { class: "pc2-protocol-anomaly",
                "Ignored {misplaced_tool_call_count.unwrap_or_default()} tool call(s) attached to a {value.summary.source} turn."
            }
        }
        if !has_embedded_tool_calls {
            if structured_message && !message_is_text_bearing {
                EvidenceBlock { title: "Message", open: true, JsonValue { value: message } }
            } else {
                EvidenceBlock { title: "Message", open: true, pre { "{message_text}" } }
            }
        }
        if let Some(reasoning) = &value.turn.reasoning_content {
            EvidenceBlock { title: "Reasoning", pre { "{reasoning.clone()}" } }
        }
        if let Some(observation) = value.turn.observation.clone() {
            EvidenceBlock { title: "Observation", JsonValue { value: observation } }
        }
        if !value.events.is_empty() {
            EvidenceBlock { title: event_block_title, JsonValue { value: events } }
        }
        if let Some(extra) = value.turn.extra.clone() {
            EvidenceBlock { title: "Extra", JsonValue { value: extra } }
        }
        if let Some(metrics) = value.turn.metrics.clone() {
            EvidenceBlock { title: "Metrics", JsonValue { value: metrics } }
        }
    }
}

fn source_can_call_tools(source: &str) -> bool {
    matches!(
        source.trim().to_ascii_lowercase().as_str(),
        "agent" | "assistant" | "model"
    )
}

#[component]
fn Fact(label: &'static str, value: String) -> Element {
    rsx! { div { span { "{label}" } code { "{value}" } } }
}

/// "What the model saw" at a given step: every storyline message recorded
/// before the focus turn, replayed in order. This is the reconstruction
/// primitive — the recorded trajectory up to the decision point, not a
/// summary of it.
/// Index of the focus turn inside the recorded storyline. Context rebuild
/// slices everything before this index; a missing turn yields an unavailable
/// context body.
fn context_focus_index(turns: &[StorylineTurn], focus_id: i64) -> Option<usize> {
    turns.iter().position(|turn| turn.id == focus_id)
}

#[component]
fn ContextRebuild(
    turns: Option<Vec<StorylineTurn>>,
    focus_id: i64,
    loading: bool,
    on_load: EventHandler<()>,
) -> Element {
    let mut open = use_signal(|| false);
    let mut last_focus = use_signal(|| None::<i64>);
    if last_focus() != Some(focus_id) {
        last_focus.set(Some(focus_id));
        open.set(false);
    }
    let context_loaded = turns.is_some();
    let loaded_context = turns.as_ref().and_then(|turns| {
        context_focus_index(turns, focus_id).map(|focus_index| &turns[..focus_index])
    });
    let (context_count, total_chars) = if open() {
        let count = loaded_context.map_or(0, |context| context.len());
        let chars = loaded_context.map_or(0, |context| {
            context
                .iter()
                .map(|turn| turn.text().chars().count() as u64)
                .sum::<u64>()
        });
        (count, chars)
    } else {
        (0, 0)
    };
    rsx! {
        section { class: "pc2-context-rebuild",
            button {
                class: "pc2-context-head",
                aria_expanded: open(),
                onclick: move |_| {
                    let next = !open();
                    open.set(next);
                    if next && !context_loaded && !loading {
                        on_load.call(());
                    }
                },
                span { class: "pc2-context-disclosure", if open() { "▾" } else { "▸" } }
                strong { "Context at this step" }
                span { if context_loaded { "what the model saw before step #{focus_id}" } else { "Expand to load context" } }
                if context_loaded && open() {
                    span { class: "pc2-context-stats", "{context_count} messages · {format_char_count(total_chars)}" }
                }
            }
            if open() && loading {
                div { class: "pc2-inline-loading", span { class: "spinner" } "Loading context…" }
            } else if open() && loaded_context.is_none() {
                p { class: "pc2-context-empty", "Context is unavailable for this step." }
            } else if open() && loaded_context.is_some_and(|context| context.is_empty()) {
                p { class: "pc2-context-empty", "No earlier messages — this step starts the run." }
            } else if open() {
                div { class: "pc2-context-list",
                    for turn in loaded_context.into_iter().flatten() {
                        ContextMessage { key: "ctx-{turn.id}", turn: turn.clone() }
                    }
                }
                div { class: "pc2-context-boundary", span { "step #{focus_id} used the context above ↓" } }
            }
        }
    }
}

#[component]
fn ContextMessage(turn: StorylineTurn) -> Element {
    let text = turn.text();
    let char_count = text.chars().count();
    let preview = compact_preview(&text, 240);
    rsx! {
        div { class: "pc2-context-message",
            div { class: "pc2-context-message-head",
                span { class: "pc2-role {turn.source}", "{turn.source}" }
                code { "#{turn.id}" }
                span { class: "pc2-context-message-meta", "{format_char_count(char_count as u64)}" }
            }
            if char_count > 240 {
                details { class: "pc2-context-message-body",
                    summary { "{preview}" }
                    pre { "{text}" }
                }
            } else {
                p { class: "pc2-context-message-body", "{preview}" }
            }
        }
    }
}

#[component]
fn EvidenceBlock(
    title: &'static str,
    #[props(default = false)] open: bool,
    children: Element,
) -> Element {
    rsx! { details { class: "pc2-evidence-block", open, summary { "{title}" } {children} } }
}

#[component]
fn ToolCallCards(calls: Vec<WireToolCall>) -> Element {
    rsx! {
        div { class: "pc2-tool-call-stack",
            for call in calls {
                div { class: "pc2-tool-call-card",
                    div { class: "pc2-tool-call-header",
                        div { class: "pc2-tool-call-head-left",
                            span { class: "pc2-tool-call-type", "function" }
                            strong { "{clean_tool_call_name(&call.name)}" }
                        }
                        div { class: "pc2-tool-call-head-right",
                            span { class: "pc2-tool-call-meta", "{argument_count_label(&call.arguments)}" }
                            if let Some(id) = &call.id {
                                span { class: "pc2-tool-call-id", "#{id}" }
                            }
                        }
                    }
                    div { class: "pc2-tool-call-body",
                        if let Value::Object(args) = &call.arguments {
                            for (key, val) in args {
                                div { class: "pc2-tool-call-arg",
                                    code { "{key}" }
                                    span { "{format_tool_call_arg(val)}" }
                                }
                            }
                        } else {
                            pre { "{call.arguments}" }
                        }
                    }
                    details { class: "pc2-tool-call-raw",
                        summary { "Raw call" }
                        pre { "{serde_json::to_string_pretty(&call).unwrap_or_default()}" }
                    }
                }
            }
        }
    }
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

    fn storyline_turn(id: i64) -> StorylineTurn {
        StorylineTurn {
            id,
            kind: None,
            timestamp: None,
            source: "agent".into(),
            message: Value::String(format!("message-{id}")),
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
        }
    }

    #[test]
    fn context_rebuild_slices_everything_before_the_focus_turn() {
        let turns = vec![storyline_turn(1), storyline_turn(2), storyline_turn(3)];
        assert_eq!(context_focus_index(&turns, 2), Some(1));
        assert_eq!(context_focus_index(&turns, 1), Some(0));
        assert_eq!(context_focus_index(&turns, 9), None);
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
        assert_eq!(
            composition_label(&groups[0].entries),
            "1 user + 1 agent"
        );
        assert_eq!(summary_modalities(&groups[0].entries), Vec::<String>::new());
        assert_eq!(format_char_count(15), "15 chars");
        assert_eq!(format_char_count(1200), "1.2k chars");
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
}
