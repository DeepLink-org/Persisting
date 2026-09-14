//! Repair non-standard JSON tokens that scientific / Python dumps emit.

use std::borrow::Cow;

/// Replace bare `NaN` / `Infinity` / `-Infinity` tokens with `null`.
///
/// Python `json.dumps` allows these by default; `serde_json` rejects them, so
/// ACTF fingerprinting and decode both fail with "cannot detect import format"
/// even when the document is otherwise a clear ACTF dump.
pub(crate) fn sanitize_json_nonfinite(input: &str) -> Cow<'_, str> {
    if !input.contains("NaN") && !input.contains("Infinity") {
        return Cow::Borrowed(input);
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push(b);
            i += 1;
            continue;
        }
        if match_bare_token(bytes, i, b"-Infinity") {
            out.extend_from_slice(b"null");
            i += "-Infinity".len();
            continue;
        }
        if match_bare_token(bytes, i, b"Infinity") {
            out.extend_from_slice(b"null");
            i += "Infinity".len();
            continue;
        }
        if match_bare_token(bytes, i, b"NaN") {
            out.extend_from_slice(b"null");
            i += "NaN".len();
            continue;
        }
        out.push(b);
        i += 1;
    }
    match String::from_utf8(out) {
        Ok(text) => Cow::Owned(text),
        Err(_) => Cow::Borrowed(input),
    }
}

fn match_bare_token(bytes: &[u8], index: usize, token: &[u8]) -> bool {
    if !bytes[index..].starts_with(token) {
        return false;
    }
    let before_ok = index == 0
        || matches!(
            bytes[index - 1],
            b':' | b'[' | b',' | b' ' | b'\t' | b'\n' | b'\r'
        );
    let after = index + token.len();
    let after_ok = after >= bytes.len()
        || matches!(
            bytes[after],
            b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r'
        );
    before_ok && after_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn replaces_nonfinite_outside_strings_only() {
        let input = r#"{"score": NaN, "note": "NaN", "hi": Infinity, "lo": -Infinity}"#;
        let sanitized = sanitize_json_nonfinite(input);
        let value: Value = serde_json::from_str(&sanitized).unwrap();
        assert!(value["score"].is_null());
        assert_eq!(value["note"], "NaN");
        assert!(value["hi"].is_null());
        assert!(value["lo"].is_null());
    }

    #[test]
    fn leaves_standard_json_untouched() {
        let input = r#"{"score": 1.5}"#;
        assert!(matches!(sanitize_json_nonfinite(input), Cow::Borrowed(_)));
    }
}
