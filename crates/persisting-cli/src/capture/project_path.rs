//! Project directory slug (Claude Code / Cursor style).

/// Encode a filesystem path to the project directory name under `~/.claude/projects/` or
/// `~/.cursor/projects/`.
pub fn encode_project_path(path: &str) -> String {
    const SANITIZED_CHARS: &[char] = &['/', '.', '_', '\\'];

    path.chars()
        .map(|c| {
            if !c.is_ascii() || SANITIZED_CHARS.contains(&c) {
                '-'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_unix_path() {
        assert_eq!(encode_project_path("/Users/foo/bar"), "-Users-foo-bar");
    }
}
