//! Shared text normalization and formatting helpers.

use serde_json::Value;

pub(crate) fn unwrap_session_tag(s: &str) -> &str {
    let t = s.trim();
    if let Some(inner) = t
        .strip_prefix("<session>")
        .and_then(|x| x.strip_suffix("</session>"))
    {
        return inner.trim();
    }
    t
}

pub(crate) fn is_system_injection(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("<system-reminder>")
        || t.starts_with("<system-reminder")
        || t.starts_with("<local-command")
        || t.starts_with("[SUGGESTION MODE:")
        || t.starts_with("SUGGESTION MODE:")
}

pub(crate) fn is_codex_context_injection(s: &str) -> bool {
    if is_system_injection(s) {
        return true;
    }
    let t = s.trim_start();
    t.starts_with("<permissions")
        || t.starts_with("<environment")
        || t.starts_with("You are Codex")
        || t.contains("</permissions instructions>")
}

pub(crate) fn format_tool_result_block(part: &Value) -> Option<String> {
    let id = part
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("tool");
    let content = tool_result_content(part.get("content")?);
    Some(format!("```tool_result:{id}\n{content}\n```"))
}

pub(crate) fn tool_result_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| p.get("content").and_then(|c| c.as_str()))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

pub(crate) fn normalize_json(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return serde_json::to_string_pretty(&v).unwrap_or_else(|_| trimmed.to_string());
    }
    trimmed.to_string()
}

pub(crate) fn join_assistant_parts(
    text: &str,
    tools: impl IntoIterator<Item = (String, String)>,
) -> String {
    let mut parts = Vec::new();
    if !text.trim().is_empty() {
        parts.push(text.to_string());
    }
    for (name, args) in tools {
        if name.trim().is_empty() && args.trim().is_empty() {
            continue;
        }
        let name = if name.trim().is_empty() {
            "tool".to_string()
        } else {
            name
        };
        let args = {
            let normalized = normalize_json(&args);
            if normalized.trim().is_empty() {
                "{}".to_string()
            } else {
                normalized
            }
        };
        parts.push(format!("```tool:{name}\n{args}\n```"));
    }
    parts.join("\n\n")
}
