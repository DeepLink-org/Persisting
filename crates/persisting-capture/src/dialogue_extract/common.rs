//! Shared text normalization and formatting helpers.

use serde_json::Value;

const IMAGE_PROMPT_PREVIEW_CHARS: usize = 200;
const HASH_HEX_CHARS: usize = 12;

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

/// Parse `data:{media_type};base64,{payload}` image URLs.
pub(crate) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.trim().strip_prefix("data:")?;
    rest.split_once(";base64,")
}

pub(crate) fn format_image_url_placeholder(url: &str) -> String {
    if let Some((media_type, data)) = parse_data_url(url) {
        format_image_base64_placeholder(media_type, data)
    } else {
        format!("[image: url:{url}]")
    }
}

pub(crate) fn format_image_base64_placeholder(media_type: &str, base64_data: &str) -> String {
    let approx_bytes = base64_data.len().saturating_mul(3) / 4;
    let size = format_approx_bytes(approx_bytes);
    let hash = short_content_hash(base64_data);
    format!("[image: base64:{size} {media_type} hash={hash}]")
}

pub(crate) fn format_anthropic_image_placeholder(part: &Value) -> Option<String> {
    let source = part.get("source")?;
    match source.get("type")?.as_str()? {
        "url" => {
            let url = source.get("url")?.as_str()?;
            Some(format_image_url_placeholder(url))
        }
        "base64" => {
            let media_type = source
                .get("media_type")
                .and_then(|m| m.as_str())
                .unwrap_or("image/*");
            let data = source.get("data")?.as_str()?;
            Some(format_image_base64_placeholder(media_type, data))
        }
        _ => None,
    }
}

pub(crate) fn format_openai_image_url_part(part: &Value) -> Option<String> {
    let url = part
        .get("image_url")
        .and_then(|iu| iu.get("url"))
        .and_then(|u| u.as_str())
        .or_else(|| part.get("url").and_then(|u| u.as_str()))?;
    Some(format_image_url_placeholder(url))
}

pub(crate) fn format_input_image_placeholder(part: &Value) -> Option<String> {
    part.get("image_url")
        .and_then(|u| u.as_str())
        .map(format_image_url_placeholder)
}

pub(crate) fn format_image_generation_placeholder(item: &Value) -> String {
    let id = item
        .get("id")
        .or_else(|| item.get("item_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("ig");
    let format = item
        .get("output_format")
        .and_then(|f| f.as_str())
        .unwrap_or("png");
    let size = item
        .get("size")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty());
    let mut label = format!("[image_generated: {id}, {format}");
    if let Some(s) = size {
        label.push_str(&format!(", {s}"));
    }
    if let Some(result) = item
        .get("result")
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty())
    {
        let approx_bytes = result.len().saturating_mul(3) / 4;
        if approx_bytes > 0 {
            label.push_str(&format!(", ~{}", format_approx_bytes(approx_bytes)));
        }
    }
    label.push(']');
    if let Some(prompt) = item
        .get("revised_prompt")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
    {
        let preview = truncate_preview(prompt, IMAGE_PROMPT_PREVIEW_CHARS);
        return format!("{label}\nprompt: {preview}");
    }
    label
}

/// Append one user-visible content part (text, tool_result, or image) to `out`.
pub(crate) fn append_user_content_part(out: &mut Vec<String>, part: &Value) {
    let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "text" | "input_text" => {
            if let Some(text) = part.get("text").and_then(|x| x.as_str()) {
                let text = unwrap_session_tag(text);
                if !is_codex_context_injection(text) && !text.trim().is_empty() {
                    out.push(text.to_string());
                }
            }
        }
        "tool_result" | "function_call_output" => {
            if let Some(formatted) = format_tool_result_block(part) {
                out.push(formatted);
            }
        }
        "image" => {
            if let Some(formatted) = format_anthropic_image_placeholder(part) {
                out.push(formatted);
            }
        }
        "image_url" => {
            if let Some(formatted) = format_openai_image_url_part(part) {
                out.push(formatted);
            }
        }
        "input_image" => {
            if let Some(formatted) = format_input_image_placeholder(part) {
                out.push(formatted);
            }
        }
        _ => {}
    }
}

pub(crate) fn user_content_part_is_visible(part: &Value) -> bool {
    let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "text" | "input_text" => part
            .get("text")
            .and_then(|x| x.as_str())
            .is_some_and(|text| {
                let text = unwrap_session_tag(text);
                !is_codex_context_injection(text) && !text.trim().is_empty()
            }),
        "tool_result" | "function_call_output" => format_tool_result_block(part).is_some(),
        "image" => format_anthropic_image_placeholder(part).is_some(),
        "image_url" => format_openai_image_url_part(part).is_some(),
        "input_image" => format_input_image_placeholder(part).is_some(),
        _ => false,
    }
}

pub(crate) fn format_visible_content_from_parts(parts: &[Value]) -> Option<String> {
    let mut out = Vec::new();
    for part in parts {
        append_user_content_part(&mut out, part);
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n\n"))
    }
}

fn format_approx_bytes(bytes: usize) -> String {
    if bytes >= 1_048_576 {
        format!("{}MB", bytes / 1_048_576)
    } else if bytes >= 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

fn short_content_hash(data: &str) -> String {
    blake3::hash(data.as_bytes()).to_hex()[..HASH_HEX_CHARS.min(blake3::OUT_LEN * 2)].to_string()
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}…", &text[..end])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_image_placeholder_includes_size_and_hash() {
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let out = format_image_url_placeholder(url);
        assert!(out.starts_with("[image: base64:"));
        assert!(out.contains("image/png"));
        assert!(out.contains("hash="));
    }

    #[test]
    fn https_image_url_placeholder() {
        let out = format_image_url_placeholder("https://example.com/a.png");
        assert_eq!(out, "[image: url:https://example.com/a.png]");
    }
}
