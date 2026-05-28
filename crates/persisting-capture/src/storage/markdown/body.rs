//! Block body helpers: subagent footers and visible message text.

/// Block-header JSON schema version (`"v"` field). Bump when breaking block JSON layout.
pub const BLOCK_FORMAT_VERSION: u64 = 1;

/// Optional string alias for tools that prefer semver-style labels (`"1.0"`).
pub const BLOCK_FORMAT_BLOCK: &str = "1.0";

/// Strip [`persisting:subagent-*`](super::types::BLOCK_MARKER) footer lines from a block body.
///
/// Footers are written for human readers; they must not round-trip into `CaptureRecord` message
/// fields or [`CaptureRecord::visible_*`](crate::record::CaptureRecord) text.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
