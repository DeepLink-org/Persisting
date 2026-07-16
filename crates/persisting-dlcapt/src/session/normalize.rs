pub const DEFAULT_MAX_STEM_LEN: usize = 128;

pub fn normalize_session_id(raw: &str, preserve_raw: bool, max_stem_len: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = if preserve_raw {
        trimmed.to_string()
    } else {
        sanitize_path_segment(trimmed)
    };

    if normalized.is_empty() {
        return None;
    }

    Some(normalized.chars().take(max_stem_len).collect())
}

fn sanitize_path_segment(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_should_sanitize_unsafe_chars_by_default() {
        assert_eq!(
            normalize_session_id("feishu/main", false, DEFAULT_MAX_STEM_LEN).as_deref(),
            Some("feishu-main")
        );
    }

    #[test]
    fn normalize_should_preserve_raw_when_enabled() {
        assert_eq!(
            normalize_session_id("feishu/main", true, DEFAULT_MAX_STEM_LEN).as_deref(),
            Some("feishu/main")
        );
    }
}
