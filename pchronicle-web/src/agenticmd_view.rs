//! Standalone AgenticMD renderer for the trace inspector.
//!
//! This module intentionally owns both the small syntax recognizer and the
//! presentation. A step can therefore be rendered as AgenticMD anywhere in
//! the application without coupling the renderer to the timeline or drawer.

use dioxus::prelude::*;
use serde_json::Value;

use crate::components::ToolCallCards;
use crate::json_value::JsonValue;
use crate::model::{StorylineTurn, ToolCall, WireToolCall, extract_message_text};

#[derive(Clone, Debug, PartialEq)]
enum BodyNode {
    Text(String),
    Comment(String),
    Code {
        language: String,
        body: String,
    },
    Xml {
        tag: String,
        children: Vec<BodyNode>,
        self_closing: bool,
    },
    ToolCall {
        protocol: &'static str,
        name: String,
        arguments: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct CodeToken {
    class: &'static str,
    text: String,
}

fn agenticmd_message(turn: &StorylineTurn) -> String {
    match &turn.message {
        Value::String(value) => value.clone(),
        value => serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into()),
    }
}

#[cfg(test)]
fn agenticmd_header(turn: &StorylineTurn) -> (String, Value) {
    let body = agenticmd_message(turn);
    let (type_name, encoding) = if matches!(turn.message, Value::String(_)) {
        ("text", "text")
    } else {
        ("json", "json")
    };
    let mut storyline = serde_json::to_value(turn)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    storyline.remove("msg");
    let metadata = serde_json::json!({
        "type": type_name,
        "length": body.len(),
        "source": turn.source,
        "step_id": turn.id,
        "message_encoding": encoding,
        "storyline": storyline,
    });
    let line = format!(
        "<!-- persisting:block:{} {} -->",
        turn.source,
        serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".into())
    );
    (line, metadata)
}

fn parse_body_nodes(body: &str) -> Vec<BodyNode> {
    let mut nodes = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let rest = &body[cursor..];
        if let Some((language, content, consumed)) = take_code_fence(rest) {
            nodes.push(BodyNode::Code {
                language,
                body: content,
            });
            cursor += consumed;
            continue;
        }
        if let Some(end) = rest.find("-->").filter(|_| rest.starts_with("<!--")) {
            nodes.push(BodyNode::Comment(rest[..end + 3].to_string()));
            cursor += end + 3;
            continue;
        }
        if let Some((node, consumed)) = take_tool_node(rest) {
            nodes.push(node);
            cursor += consumed;
            continue;
        }
        if let Some((node, consumed)) = take_xml_node(rest) {
            nodes.push(node);
            cursor += consumed;
            continue;
        }
        if rest.starts_with('<') {
            if let Some(end) = rest.find('>') {
                let tag = rest[1..end]
                    .split_whitespace()
                    .next()
                    .unwrap_or("tag")
                    .trim_end_matches('/');
                let line_end = rest[end + 1..]
                    .find('\n')
                    .map(|offset| end + 1 + offset)
                    .unwrap_or(rest.len());
                nodes.push(BodyNode::Xml {
                    tag: tag.to_string(),
                    children: vec![BodyNode::Text(rest[..line_end].to_string())],
                    self_closing: false,
                });
                cursor += line_end.max(1);
                continue;
            }
        }
        let next = [
            "```",
            "<!--",
            "<tool_call>",
            "<function=",
            "<mcp_",
            "<mcp-",
            "<",
        ]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .filter(|offset| *offset > 0)
        .min()
        .unwrap_or(rest.len());
        let text = rest[..next].to_string();
        if !text.trim().is_empty() {
            nodes.push(BodyNode::Text(text));
        }
        cursor += next.max(1);
    }
    nodes
}

/// Parse a balanced XML-like element, retaining nested elements as a tree.
/// AgenticMD tags are intentionally only XML-shaped (not full XML): values can
/// contain arbitrary text, Markdown, code fences, or tool blocks.
fn take_xml_node(input: &str) -> Option<(BodyNode, usize)> {
    let (tag, open_end, self_closing) = parse_xml_tag(input, false)?;
    if self_closing {
        return Some((
            BodyNode::Xml {
                tag,
                children: Vec::new(),
                self_closing: true,
            },
            open_end,
        ));
    }

    let mut children = Vec::new();
    let mut cursor = open_end;
    while cursor < input.len() {
        let rest = &input[cursor..];
        if let Some((closing_tag, consumed, _)) = parse_xml_tag(rest, true) {
            if closing_tag != tag {
                return None;
            }
            return Some((
                BodyNode::Xml {
                    tag,
                    children,
                    self_closing: false,
                },
                cursor + consumed,
            ));
        }
        if let Some((language, body, consumed)) = take_code_fence(rest) {
            children.push(BodyNode::Code { language, body });
            cursor += consumed;
            continue;
        }
        if let Some((node, consumed)) = take_tool_node(rest) {
            children.push(node);
            cursor += consumed;
            continue;
        }
        if let Some((node, consumed)) = take_xml_node(rest) {
            children.push(node);
            cursor += consumed;
            continue;
        }

        let next = rest.find('<').unwrap_or(rest.len());
        let consumed = next.max(1);
        children.push(BodyNode::Text(rest[..next].to_string()));
        cursor += consumed;
    }
    None
}

fn parse_xml_tag(input: &str, closing: bool) -> Option<(String, usize, bool)> {
    if !input.starts_with('<') || input.starts_with("<!--") || input.starts_with("<![") {
        return None;
    }
    let mut start = 1;
    if closing {
        if !input[1..].starts_with('/') {
            return None;
        }
        start += 1;
    } else if input[1..].starts_with('/')
        || input[1..].starts_with('!')
        || input[1..].starts_with('?')
    {
        return None;
    }
    let end = input.find('>')?;
    let raw = &input[start..end];
    let trimmed = raw.trim();
    if trimmed.is_empty() || (!closing && trimmed.ends_with('/')) {
        if closing {
            return None;
        }
    }
    let name = trimmed
        .trim_end_matches('/')
        .split_whitespace()
        .next()?
        .to_string();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'))
    {
        return None;
    }
    Some((name, end + 1, !closing && trimmed.ends_with('/')))
}

fn take_code_fence(input: &str) -> Option<(String, String, usize)> {
    if !input.starts_with("```") {
        return None;
    }
    let first_newline = input.find('\n')?;
    let language = input[3..first_newline].trim().to_string();
    let closing_offset = input[first_newline + 1..].find("\n```")?;
    let body_start = first_newline + 1;
    let body_end = body_start + closing_offset;
    let consumed = body_end + 4;
    Some((language, input[body_start..body_end].to_string(), consumed))
}

fn take_tool_node(input: &str) -> Option<(BodyNode, usize)> {
    let (protocol, open, close) = if input.starts_with("<tool_call>") {
        ("DSML", "<tool_call>", "</tool_call>")
    } else if input.starts_with("<function=") {
        ("function", "<function=", "</function>")
    } else if input.starts_with("<mcp_call>") {
        ("MCP", "<mcp_call>", "</mcp_call>")
    } else if input.starts_with("<mcp_") || input.starts_with("<mcp-") {
        ("MCP", "<", ">")
    } else {
        return None;
    };
    let after_open = &input[open.len()..];
    let name_end = after_open
        .find(['>', '\n', '<'])
        .unwrap_or(after_open.len());
    let name = if open == "<function=" {
        after_open[..name_end].trim().to_string()
    } else if open == "<mcp_call>" {
        after_open[..name_end].trim().to_string()
    } else if open == "<" {
        input[1..input.find('>')?].trim().to_string()
    } else {
        clean_tool_name(after_open[..name_end].trim())
    };
    let tail = &after_open[name_end..];
    let (arguments, consumed) = if close == ">" {
        (tail.to_string(), input.find('>')? + 1)
    } else if let Some(end) = tail.find(close) {
        (
            tail[..end].trim().to_string(),
            open.len() + name_end + end + close.len(),
        )
    } else {
        (tail.to_string(), input.len())
    };
    Some((
        BodyNode::ToolCall {
            protocol,
            name,
            arguments,
        },
        consumed.max(1),
    ))
}

fn clean_tool_name(value: &str) -> String {
    value
        .split(['\n', '<'])
        .next()
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn highlight_code(source: &str, language: &str) -> Vec<CodeToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |tokens: &mut Vec<CodeToken>, current: &mut String| {
        if !current.is_empty() {
            let text = std::mem::take(current);
            let class = if text.chars().all(|char| char.is_ascii_digit()) {
                "number"
            } else if is_keyword(&text, language) {
                "keyword"
            } else {
                "plain"
            };
            tokens.push(CodeToken { class, text });
        }
    };
    let chars = source.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        if char == '"' || char == '\'' || char == '`' {
            flush(&mut tokens, &mut current);
            let quote = char;
            let mut text = String::from(char);
            index += 1;
            while index < chars.len() {
                text.push(chars[index]);
                if chars[index] == quote && chars.get(index.saturating_sub(1)) != Some(&'\\') {
                    index += 1;
                    break;
                }
                index += 1;
            }
            tokens.push(CodeToken {
                class: "string",
                text,
            });
        } else if (char == '#' || (char == '/' && chars.get(index + 1) == Some(&'/')))
            && current.is_empty()
        {
            flush(&mut tokens, &mut current);
            let mut text = String::new();
            while index < chars.len() && chars[index] != '\n' {
                text.push(chars[index]);
                index += 1;
            }
            tokens.push(CodeToken {
                class: "comment",
                text,
            });
        } else if char.is_ascii_alphanumeric() || char == '_' {
            current.push(char);
            index += 1;
        } else {
            flush(&mut tokens, &mut current);
            tokens.push(CodeToken {
                class: "punctuation",
                text: char.to_string(),
            });
            index += 1;
        }
    }
    flush(&mut tokens, &mut current);
    tokens
}

fn is_keyword(word: &str, language: &str) -> bool {
    let common = [
        "fn", "let", "mut", "if", "else", "for", "in", "return", "match", "struct", "true",
        "false", "null", "const", "function", "class", "def", "import", "from", "async", "await",
        "new", "SELECT", "FROM", "WHERE", "AND", "OR",
    ];
    let language = language.to_ascii_lowercase();
    common.contains(&word) || (language == "json" && matches!(word, "true" | "false" | "null"))
}

#[component]
fn HighlightedCodeFallback(body: String, language: String) -> Element {
    let tokens = highlight_code(&body, &language);
    rsx! { pre { class: "pc2-agenticmd-code language-{language}", for token in tokens { span { class: "pc2-code-token {token.class}", "{token.text}" } } } }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "native-code"))]
#[component]
fn HighlightedCode(body: String, language: String) -> Element {
    if let Some(language) = dioxus_code::Language::from_slug(&language.to_ascii_lowercase()) {
        let source = dioxus_code::SourceCode::new(language, body);
        rsx! {
            dioxus_code::Code {
                src: source,
                theme: dioxus_code::CodeTheme::fixed(dioxus_code::Theme::GITHUB_LIGHT),
            }
        }
    } else {
        rsx! { HighlightedCodeFallback { body, language } }
    }
}

#[cfg(any(target_arch = "wasm32", not(feature = "native-code")))]
#[component]
fn HighlightedCode(body: String, language: String) -> Element {
    rsx! { HighlightedCodeFallback { body, language } }
}

#[component]
fn CommentBlock(raw: String, chips: Vec<(String, String)>) -> Element {
    rsx! { details { class: "pc2-agenticmd-comment-block", open: true,
        summary { class: "pc2-agenticmd-comment-summary",
            span { class: "pc2-agenticmd-comment-icon", "#" }
            div { class: "pc2-agenticmd-chips",
                span { class: "pc2-agenticmd-chip pc2-agenticmd-chip-kind", "annotation" }
                for (index, (label, value)) in chips.into_iter().enumerate() {
                    span { key: "chip-{index}", class: "pc2-agenticmd-chip", span { "{label}" } code { "{value}" } }
                }
            }
            span { class: "pc2-agenticmd-comment-expand", "raw" }
        }
        div { class: "pc2-agenticmd-comment-raw", pre { "{raw}" } }
    } }
}

/// Render one Conversation using the AgenticMD document template.
///
/// The document deliberately mirrors the interchange shape:
/// `# Title`, metadata, then one `# Step N` section per turn. Empty optional
/// sections are omitted, while the sections that do render are visible by
/// default so the drawer is immediately useful.
#[component]
pub fn AgenticMdRenderer(
    #[props(default = "Conversation".to_string())] title: String,
    #[props(default)] turns: Vec<StorylineTurn>,
    #[props(default)] wire_tool_calls: Vec<Vec<WireToolCall>>,
) -> Element {
    let users = turns.iter().filter(|turn| turn.source == "user").count();
    let agents = turns.iter().filter(|turn| turn.source == "agent").count();
    let systems = turns.iter().filter(|turn| turn.source == "system").count();
    let tools = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let wire_count = wire_tool_calls.get(index).map_or(0, Vec::len);
            if wire_count > 0 {
                wire_count
            } else {
                turn.tool_calls.as_ref().map_or(0, Vec::len)
            }
        })
        .sum::<usize>();
    let mut metadata = Vec::new();
    if !turns.is_empty() {
        metadata.push(("blocks".to_string(), turns.len().to_string()));
    }
    if users > 0 {
        metadata.push(("user".to_string(), users.to_string()));
    }
    if agents > 0 {
        metadata.push(("agent".to_string(), agents.to_string()));
    }
    if systems > 0 {
        metadata.push(("system".to_string(), systems.to_string()));
    }
    if tools > 0 {
        metadata.push(("tools".to_string(), tools.to_string()));
    }
    let steps = conversation_steps(turns, wire_tool_calls);
    let has_metadata = !metadata.is_empty();
    rsx! {
        article { class: "pc2-agenticmd-renderer",
            h1 { class: "pc2-agenticmd-document-title", "{title}" }
            if has_metadata {
                hr { class: "pc2-agenticmd-rule" }
                div { class: "pc2-agenticmd-document-meta",
                    for (index, (label, value)) in metadata.into_iter().enumerate() {
                        span { key: "document-meta-{index}", class: "pc2-agenticmd-chip", span { "{label}" } code { "{value}" } }
                    }
                }
            }
            if has_metadata {
                hr { class: "pc2-agenticmd-rule" }
            }
            for (index, (user, turn, calls)) in steps.into_iter().enumerate() {
                if index > 0 { hr { class: "pc2-agenticmd-rule" } }
                AgenticMdStep {
                    user,
                    turn,
                    wire_tool_calls: calls,
                }
            }
        }
    }
}

fn conversation_steps(
    turns: Vec<StorylineTurn>,
    wire_tool_calls: Vec<Vec<WireToolCall>>,
) -> Vec<(Option<StorylineTurn>, StorylineTurn, Vec<WireToolCall>)> {
    let mut steps = Vec::new();
    let mut pending_user = None;
    for (index, turn) in turns.into_iter().enumerate() {
        let calls = wire_tool_calls.get(index).cloned().unwrap_or_default();
        if turn.source == "user" {
            if let Some(previous) = pending_user.replace(turn) {
                steps.push((None, previous, Vec::new()));
            }
        } else if turn.source == "agent" {
            steps.push((pending_user.take(), turn, calls));
        } else {
            if let Some(previous) = pending_user.take() {
                steps.push((None, previous, Vec::new()));
            }
            steps.push((None, turn, calls));
        }
    }
    if let Some(user) = pending_user {
        steps.push((None, user, Vec::new()));
    }
    steps
}

fn message_nodes(turn: &StorylineTurn) -> Vec<BodyNode> {
    let body = agenticmd_message(turn);
    if body.trim().is_empty() || body.trim() == "No text" {
        Vec::new()
    } else if matches!(turn.message, Value::String(_)) {
        parse_body_nodes(&body)
    } else if let Some(text) = extract_message_text(&turn.message) {
        // OpenAI content parts are structured JSON, but their text payload is
        // still the readable message. Render that payload as AgenticMD instead
        // of exposing the transport envelope and its null-heavy fields.
        parse_body_nodes(&text)
    } else {
        vec![BodyNode::Code {
            language: "json".into(),
            body,
        }]
    }
}

#[component]
fn AgenticMdStep(
    user: Option<StorylineTurn>,
    turn: StorylineTurn,
    #[props(default)] wire_tool_calls: Vec<WireToolCall>,
) -> Element {
    let user_body = user.as_ref().map(agenticmd_message).unwrap_or_default();
    let user_nodes = user.as_ref().map(message_nodes).unwrap_or_default();
    let body = agenticmd_message(&turn);
    let mut nodes = message_nodes(&turn);
    let native_calls = turn
        .tool_calls
        .clone()
        .unwrap_or_default()
        .iter()
        .map(native_tool_call)
        .collect::<Vec<_>>();
    let mut timeline_calls = wire_tool_calls;
    timeline_calls.extend(native_calls);
    if timeline_calls.is_empty() {
        let parsed_calls = nodes
            .iter()
            .filter_map(|node| match node {
                BodyNode::ToolCall {
                    name, arguments, ..
                } => Some(WireToolCall {
                    id: None,
                    name: name.clone(),
                    arguments: parse_tool_arguments(arguments),
                    result: None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        timeline_calls = parsed_calls;
    }
    nodes.retain(|node| !matches!(node, BodyNode::ToolCall { .. }));
    let has_message = !body.trim().is_empty() && body.trim() != "No text";
    let renderable_metrics = turn.metrics.as_ref().and_then(compact_metric_value);
    let has_metrics = renderable_metrics
        .as_ref()
        .is_some_and(metrics_are_renderable);
    let has_tools = !timeline_calls.is_empty();
    let has_observation = turn
        .observation
        .as_ref()
        .is_some_and(|value| !value.is_null());
    let has_reasoning = turn
        .reasoning_content
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_user_message = !user_body.trim().is_empty() && user_body.trim() != "No text";
    let has_agent_content = turn.source != "user"
        && (has_message || has_tools || has_metrics || has_reasoning || has_observation);
    let step_id = if turn.source == "user" {
        turn.id.saturating_abs()
    } else {
        turn.id
    };
    rsx! {
        section { class: "pc2-agenticmd-step",
            h1 { class: "pc2-agenticmd-step-heading", "Step {step_id}" }
            if has_user_message {
                h2 { class: "pc2-agenticmd-section-heading", "User" }
                AgenticMdBody { nodes: user_nodes }
            }
            if turn.source == "user" {
                if has_message && !has_user_message {
                    h2 { class: "pc2-agenticmd-section-heading", "User" }
                    AgenticMdBody { nodes: nodes.clone() }
                }
            } else if has_agent_content {
                h2 { class: "pc2-agenticmd-section-heading", "{role_heading(&turn.source)}" }
                if has_message { AgenticMdBody { nodes: nodes.clone() } }
                if has_reasoning {
                    section { class: "pc2-agenticmd-content-block pc2-agenticmd-reasoning",
                        header { "Reasoning" }
                        pre { "{turn.reasoning_content.clone().unwrap_or_default()}" }
                    }
                }
                if has_tools {
                    ToolCallCards { calls: timeline_calls, observation: turn.observation.clone() }
                }
                if has_observation && !has_tools {
                    section { class: "pc2-agenticmd-content-block pc2-agenticmd-observation",
                        header { "Observation" }
                        JsonValue { value: turn.observation.clone().unwrap_or(Value::Null), default_open: true }
                    }
                }
                if has_metrics {
                    section { class: "pc2-agenticmd-content-block pc2-agenticmd-metrics",
                        header { "Metrics" }
                        JsonValue { value: renderable_metrics.clone().unwrap_or(Value::Null), default_open: true }
                    }
                }
            }
        }
    }
}

fn role_heading(source: &str) -> &'static str {
    if source == "system" {
        "System"
    } else {
        "Agent"
    }
}

#[component]
fn AgenticMdBody(nodes: Vec<BodyNode>) -> Element {
    rsx! {
        for (index, node) in nodes.into_iter().enumerate() {
            div { key: "node-{index}", class: "pc2-agenticmd-node",
                match node {
                    BodyNode::Text(text) => rsx! { MarkdownText { text } },
                    BodyNode::Comment(raw) => rsx! { CommentBlock { raw, chips: Vec::new() } },
                    BodyNode::Code { language, body } => rsx! { HighlightedCode { body, language } },
                BodyNode::Xml {
                    tag,
                    children,
                    self_closing,
                } => {
                        let display_tag = if self_closing {
                            format!("<{tag} />")
                        } else {
                            format!("<{tag}>")
                        };
                        let closing_tag = if self_closing {
                            String::new()
                        } else {
                            format!("</{tag}>")
                        };
                        let plain_text = xml_plain_text(&children);
                        rsx! { div { class: "pc2-agenticmd-xml",
                            div { class: "pc2-agenticmd-xml-header",
                                span { class: "pc2-agenticmd-xml-tag", "{display_tag}" }
                                span { class: "pc2-agenticmd-xml-close", "{closing_tag}" }
                            }
                            if let Some(text) = plain_text {
                                code { class: "pc2-agenticmd-xml-value", "{text}" }
                            } else if !children.is_empty() {
                                div { class: "pc2-agenticmd-xml-body", AgenticMdBody { nodes: children } }
                            }
                        } }
                    },
                    BodyNode::ToolCall { .. } => rsx! {},
                }
            }
        }
    }
}

fn xml_plain_text(nodes: &[BodyNode]) -> Option<String> {
    let mut text = String::new();
    for node in nodes {
        let BodyNode::Text(value) = node else {
            return None;
        };
        text.push_str(value);
    }
    (!text.trim().is_empty()).then_some(text.trim().to_string())
}

pub(crate) fn metrics_are_renderable(value: &Value) -> bool {
    if value.is_null() || value.as_object().is_some_and(|object| object.is_empty()) {
        return false;
    }
    if value.get("type").and_then(Value::as_str) == Some("token_count") {
        return has_positive_token_value(value);
    }
    true
}

pub(crate) fn compact_metric_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Object(object) => {
            let compacted = object
                .iter()
                .filter_map(|(key, value)| {
                    compact_metric_value(value).map(|value| (key.clone(), value))
                })
                .collect::<serde_json::Map<_, _>>();
            (!compacted.is_empty()).then_some(Value::Object(compacted))
        }
        Value::Array(values) => {
            let compacted = values
                .iter()
                .filter_map(compact_metric_value)
                .collect::<Vec<_>>();
            (!compacted.is_empty()).then_some(Value::Array(compacted))
        }
        _ => Some(value.clone()),
    }
}

fn has_positive_token_value(value: &Value) -> bool {
    const TOKEN_KEYS: &[&str] = &[
        "cached_input_tokens",
        "completion_tokens",
        "completion_tokens_len",
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "prompt_tokens_len",
        "reasoning_output_tokens",
        "total_tokens",
    ];
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (TOKEN_KEYS.contains(&key.as_str())
                && value
                    .as_f64()
                    .is_some_and(|count| count.is_finite() && count > 0.0))
                || (!TOKEN_KEYS.contains(&key.as_str()) && has_positive_token_value(value))
        }),
        Value::Array(values) => values.iter().any(has_positive_token_value),
        _ => false,
    }
}

#[component]
fn MarkdownText(text: String) -> Element {
    rsx! {
        div { class: "pc2-agenticmd-markdown",
            for (index, paragraph) in text.split("\n\n").enumerate().filter(|(_, value)| !value.trim().is_empty()) {
                div { key: "paragraph-{index}", class: "pc2-agenticmd-paragraph", for line in paragraph.lines() {
                    if let Some(item) = line.strip_prefix("- ") {
                        div { class: "pc2-agenticmd-list-item", span { "•" } InlineMarkdown { text: item.to_string() } }
                    } else if let Some(item) = line.strip_prefix("* ") {
                        div { class: "pc2-agenticmd-list-item", span { "•" } InlineMarkdown { text: item.to_string() } }
                    } else if let Some(item) = line.strip_prefix("### ") {
                        h3 { InlineMarkdown { text: item.to_string() } }
                    } else if let Some(item) = line.strip_prefix("## ") {
                        h3 { InlineMarkdown { text: item.to_string() } }
                    } else if let Some(item) = line.strip_prefix("# ") {
                        h3 { InlineMarkdown { text: item.to_string() } }
                    } else {
                        p { InlineMarkdown { text: line.to_string() } }
                    }
                } }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum InlineMarkdownNode {
    Text(String),
    Strong(String),
    Emphasis(String),
    Code(String),
}

fn parse_inline_markdown(text: &str) -> Vec<InlineMarkdownNode> {
    let mut nodes = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let marker = ["**", "__", "`", "*", "_"]
            .iter()
            .filter_map(|marker| rest.find(marker).map(|offset| (offset, *marker)))
            .min_by_key(|(offset, marker)| (*offset, std::cmp::Reverse(marker.len())));
        let Some((offset, marker)) = marker else {
            nodes.push(InlineMarkdownNode::Text(rest.to_string()));
            break;
        };
        if offset > 0 {
            nodes.push(InlineMarkdownNode::Text(rest[..offset].to_string()));
            cursor += offset;
            continue;
        }
        let content_start = marker.len();
        let Some(end) = rest[content_start..].find(marker) else {
            nodes.push(InlineMarkdownNode::Text(rest.to_string()));
            break;
        };
        let content = rest[content_start..content_start + end].to_string();
        if content.is_empty() {
            nodes.push(InlineMarkdownNode::Text(marker.to_string()));
            cursor += marker.len();
            continue;
        }
        let node = match marker {
            "**" | "__" => InlineMarkdownNode::Strong(content),
            "`" => InlineMarkdownNode::Code(content),
            "*" | "_" => InlineMarkdownNode::Emphasis(content),
            _ => InlineMarkdownNode::Text(content),
        };
        nodes.push(node);
        cursor += content_start + end + marker.len();
    }
    nodes
}

#[component]
fn InlineMarkdown(text: String) -> Element {
    rsx! {
        for (index, node) in parse_inline_markdown(&text).into_iter().enumerate() {
            match node {
                InlineMarkdownNode::Text(value) => rsx! { span { key: "inline-{index}", "{value}" } },
                InlineMarkdownNode::Strong(value) => rsx! { strong { key: "inline-{index}", "{value}" } },
                InlineMarkdownNode::Emphasis(value) => rsx! { em { key: "inline-{index}", "{value}" } },
                InlineMarkdownNode::Code(value) => rsx! { code { key: "inline-{index}", "{value}" } },
            }
        }
    }
}

fn parse_tool_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_string()))
}

fn native_tool_call(call: &ToolCall) -> WireToolCall {
    WireToolCall {
        id: Some(call.tool_call_id.clone()),
        name: call.function_name.clone(),
        arguments: call.arguments.clone(),
        result: call.result.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(message: Value) -> StorylineTurn {
        StorylineTurn {
            id: 4,
            kind: Some("autonomous".into()),
            timestamp: None,
            source: "agent".into(),
            message,
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
    fn native_tool_call_keeps_imported_result_for_rendering() {
        let call = ToolCall {
            tool_call_id: "call-1".into(),
            function_name: "exec_command".into(),
            arguments: serde_json::json!({"cmd": "pwd"}),
            result: Some(serde_json::json!({"content": "/tmp"})),
            duration_ms: None,
        };
        let wire = native_tool_call(&call);
        assert_eq!(wire.id.as_deref(), Some("call-1"));
        assert_eq!(wire.result, call.result);
    }

    #[test]
    fn parses_code_comments_xml_and_dsml() {
        let body = "```rust\nlet value = 1;\n```\n<!-- note -->\n<runtime>ok</runtime>\n<tool_call>execute_bash<parameter=command>ls</parameter></tool_call>";
        let nodes = parse_body_nodes(body);
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node, BodyNode::Code { language, .. } if language == "rust"))
        );
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node, BodyNode::Comment(value) if value.contains("note")))
        );
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node, BodyNode::Xml { tag, .. } if tag == "runtime"))
        );
        assert!(nodes.iter().any(|node| matches!(node, BodyNode::ToolCall { protocol, name, .. } if *protocol == "DSML" && name == "execute_bash")));
    }

    #[test]
    fn parses_balanced_xml_as_nested_nodes() {
        let nodes = parse_body_nodes(
            "<environment_context>\n  <cwd>/workspace</cwd>\n  <shell>zsh</shell>\n</environment_context>",
        );
        let BodyNode::Xml { tag, children, .. } = &nodes[0] else {
            panic!("expected outer XML node")
        };
        assert_eq!(tag, "environment_context");
        let nested = children
            .iter()
            .filter_map(|node| match node {
                BodyNode::Xml { tag, children, .. } => Some((tag.as_str(), children)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[0].0, "cwd");
        assert_eq!(xml_plain_text(nested[0].1).as_deref(), Some("/workspace"));
        assert_eq!(nested[1].0, "shell");
        assert_eq!(xml_plain_text(nested[1].1).as_deref(), Some("zsh"));
    }

    #[test]
    fn code_highlighter_marks_keywords_strings_and_numbers() {
        let tokens = highlight_code("let x = 42;\n\"ok\"", "rust");
        assert!(
            tokens
                .iter()
                .any(|token| token.class == "keyword" && token.text == "let")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.class == "number" && token.text == "42")
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.class == "string" && token.text == "\"ok\"")
        );
    }

    #[test]
    fn header_uses_persisting_block_shape() {
        let (header, _) = agenticmd_header(&turn(Value::String("hello".into())));
        assert!(header.starts_with("<!-- persisting:block:agent "));
        assert!(header.contains("\"step_id\":4"));
        assert!(header.contains("\"message_encoding\":\"text\""));
    }

    #[test]
    fn structured_message_is_rendered_as_highlightable_json() {
        let value = turn(serde_json::json!({"ok": true}));
        let body = agenticmd_message(&value);
        let nodes = if matches!(value.message, Value::String(_)) {
            parse_body_nodes(&body)
        } else {
            vec![BodyNode::Code {
                language: "json".into(),
                body,
            }]
        };
        assert!(matches!(&nodes[0], BodyNode::Code { language, .. } if language == "json"));
    }

    #[test]
    fn structured_content_parts_render_their_text_payload() {
        let value = turn(serde_json::json!([
            {"type": "text", "text": "You are a helpful agent."},
            {"type": "image_url", "image_url": {"url": "https://example.test/image.png"}}
        ]));
        let nodes = message_nodes(&value);
        assert!(nodes.iter().any(|node| {
            matches!(node, BodyNode::Text(text) if text.contains("helpful agent"))
        }));
        assert!(
            !nodes
                .iter()
                .any(|node| matches!(node, BodyNode::Code { language, .. } if language == "json"))
        );
    }

    #[test]
    fn conversation_template_pairs_a_user_with_the_following_agent_step() {
        let mut user = turn(Value::String("prompt".into()));
        user.id = -7;
        user.source = "user".into();
        let mut agent = turn(Value::String("answer".into()));
        agent.id = 7;
        let steps = conversation_steps(vec![user, agent], vec![Vec::new(), Vec::new()]);
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].0.as_ref().map(|turn| turn.source.as_str()),
            Some("user")
        );
        assert_eq!(steps[0].1.source, "agent");
        assert_eq!(steps[0].1.id, 7);
    }

    #[test]
    fn empty_optional_message_produces_no_agenticmd_body_block() {
        let value = turn(Value::String("  \n".into()));
        assert!(message_nodes(&value).is_empty());
    }

    #[test]
    fn empty_token_metrics_are_not_renderable() {
        assert!(!metrics_are_renderable(&serde_json::json!({
            "type": "token_count",
            "info": {
                "model_context_window": 258400,
                "last_token_usage": {"input_tokens": 0, "output_tokens": 0},
                "rate_limits": {"limit_id": "codex", "credits": null}
            }
        })));
        assert!(metrics_are_renderable(&serde_json::json!({
            "type": "token_count",
            "info": {"last_token_usage": {"input_tokens": 12}}
        })));
    }

    #[test]
    fn compacts_null_metric_fields_before_rendering() {
        assert_eq!(
            compact_metric_value(&serde_json::json!({
                "type": "token_count",
                "info": {
                    "last_token_usage": {"input_tokens": 12, "output_tokens": null},
                    "rate_limits": {"credits": null}
                }
            })),
            Some(serde_json::json!({
                "type": "token_count",
                "info": {"last_token_usage": {"input_tokens": 12}}
            }))
        );
    }

    #[test]
    fn inline_markdown_keeps_emphasis_and_code_as_distinct_nodes() {
        let nodes = parse_inline_markdown("**total** revenue in `USD`");
        assert!(nodes.contains(&InlineMarkdownNode::Strong("total".into())));
        assert!(nodes.contains(&InlineMarkdownNode::Code("USD".into())));
    }
}
