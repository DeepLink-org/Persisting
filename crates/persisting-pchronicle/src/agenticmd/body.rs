//! Agenticmd block body helpers (subagent footers).

/// Strip `<!-- persisting:subagent-* -->` footer lines from a block body.
///
/// Footers are human-readable only; they must not round-trip into event message fields.
pub fn strip_subagent_footer_from_body(body: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        if is_subagent_footer_line(line) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim_end().to_string()
}

/// True when `line` is a standalone HTML comment footer (after trim).
pub fn is_subagent_footer_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("<!-- persisting:subagent-") && t.ends_with("-->")
}

/// Append visible subagent ref footer for markdown trajectory readers.
pub fn append_subagent_refs_footer(body: &str, payload: &serde_json::Value) -> String {
    let mut parts = vec![body.to_string()];
    if let Some(traj) = payload.get("subagent_trajectory").and_then(|v| v.as_str()) {
        parts.push(format!("<!-- persisting:subagent-self {traj} -->"));
    }
    if let Some(paths) = payload
        .get("subagent_trajectories")
        .and_then(|v| v.as_array())
    {
        let refs: Vec<_> = paths.iter().filter_map(|p| p.as_str()).collect();
        if !refs.is_empty() {
            parts.push(format!(
                "<!-- persisting:subagent-refs {} -->",
                refs.join(" ")
            ));
        }
    }
    if parts.len() == 1 {
        return body.to_string();
    }
    format!("{}\n", parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_subagent_self_and_refs_lines() {
        let body = "hello\n<!-- persisting:subagent-self agent-abc.md -->\n<!-- persisting:subagent-refs a.md b.md -->\n";
        assert_eq!(strip_subagent_footer_from_body(body), "hello");
    }

    #[test]
    fn keeps_inline_text_with_similar_substring() {
        let body = "see <!-- persisting:subagent-self x --> in prose";
        assert_eq!(
            strip_subagent_footer_from_body(body),
            "see <!-- persisting:subagent-self x --> in prose"
        );
    }

    #[test]
    fn append_footer_from_payload() {
        let out = append_subagent_refs_footer(
            "done",
            &json!({
                "subagent_trajectory": "agent-abc.md",
                "subagent_trajectories": ["a.md", "b.md"],
            }),
        );
        assert!(out.contains("persisting:subagent-self agent-abc.md"));
        assert!(out.contains("persisting:subagent-refs a.md b.md"));
    }
}
