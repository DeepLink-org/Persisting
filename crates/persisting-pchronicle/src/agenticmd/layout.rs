//! Session markdown path resolution and filename rules.

use std::path::{Path, PathBuf};

/// Max filename stem length for `{session_id}.md` on disk.
const SESSION_FILENAME_MAX_LEN: usize = 128;

/// Encode a logical session id as a bounded, filesystem-safe filename stem.
///
/// ASCII filename characters are preserved. Every other UTF-8 byte is encoded
/// as `~HH`, making the representation injective until the fixed filename
/// budget is exhausted. Overlong encodings retain a complete readable prefix
/// and a digest of the full logical key.
pub fn session_filename_stem(session_id: &str) -> String {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return "session".to_string();
    }

    let mut encoded = String::with_capacity(trimmed.len().min(SESSION_FILENAME_MAX_LEN));
    for byte in trimmed.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(*byte as char);
        } else {
            encoded.push('~');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }

    if encoded.len() <= SESSION_FILENAME_MAX_LEN {
        return encoded;
    }

    let digest = blake3::hash(trimmed.as_bytes()).to_hex();
    let suffix = format!("~h{}", &digest[..16]);
    let prefix_limit = SESSION_FILENAME_MAX_LEN - suffix.len();
    let mut prefix = String::with_capacity(prefix_limit);
    for byte in trimmed.as_bytes() {
        let token_len = if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            1
        } else {
            3
        };
        if prefix.len() + token_len > prefix_limit {
            break;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            prefix.push(*byte as char);
        } else {
            prefix.push('~');
            prefix.push_str(&format!("{byte:02X}"));
        }
    }
    prefix.push_str(&suffix);
    prefix
}

/// Backwards-compatible name for [`session_filename_stem`].
pub fn sanitize_session_filename(session_id: &str) -> String {
    session_filename_stem(session_id)
}

fn legacy_session_filename_stem(session_id: &str) -> String {
    let trimmed = session_id.trim();
    let mut out = String::with_capacity(trimmed.len().min(SESSION_FILENAME_MAX_LEN));
    for c in trimmed.chars() {
        if out.len() >= SESSION_FILENAME_MAX_LEN {
            break;
        }
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_string()
    } else {
        out
    }
}

/// Markdown filename for a logical session (`{session_id}.md`).
pub fn session_markdown_filename(session_key: &str) -> String {
    format!("{}.md", session_filename_stem(session_key))
}

/// `{run_dir}/{session_key}.md`
pub fn session_markdown_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    run_dir.join(session_markdown_filename(session_key))
}

/// Path to append markdown blocks for one session key under a run directory.
pub fn session_markdown_write_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    locate_session_markdown_for_key(run_dir, session_key)
        .unwrap_or_else(|| session_markdown_path_for_key(run_dir, session_key))
}

/// Whether `session_key` names a subagent markdown stem (`agent-{id}`).
pub fn is_subagent_session_storage_key(session_key: &str) -> bool {
    session_key
        .trim()
        .strip_prefix("agent-")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn is_subagent_markdown_filename(name: &str) -> bool {
    name.strip_suffix(".md")
        .is_some_and(is_subagent_session_storage_key)
}

/// Capture-run bucket markdown: `{run_dir}/run-{id}.md` when the directory itself is `run-*`.
pub fn locate_run_bucket_markdown(run_dir: &Path) -> Option<PathBuf> {
    let stem = run_dir.file_name()?.to_str()?;
    if !stem.starts_with("run-") {
        return None;
    }
    let path = run_dir.join(session_markdown_filename(stem));
    path.is_file().then_some(path)
}

/// Find canonical `{key}.md` for one session under a run directory.
pub fn locate_session_markdown_for_key(run_dir: &Path, session_key: &str) -> Option<PathBuf> {
    let named = session_markdown_path_for_key(run_dir, session_key);
    if named.is_file() {
        return Some(named);
    }
    // Subagent keys always map to sibling `agent-{id}.md`; never inherit the main run file.
    if is_subagent_session_storage_key(session_key) {
        return None;
    }
    // Read-through compatibility for files written before the injective
    // filename codec was introduced. New writes always use `named` above.
    let legacy = run_dir.join(format!("{}.md", legacy_session_filename_stem(session_key)));
    if legacy != named && legacy.is_file() {
        return Some(legacy);
    }
    // Main session in a capture run: prefer the run bucket file over subagent siblings.
    if let Some(run_md) = locate_run_bucket_markdown(run_dir) {
        return Some(run_md);
    }
    None
}

/// Find the first canonical session-key markdown file in a directory.
pub fn locate_session_markdown(session_dir: &Path) -> Option<PathBuf> {
    let mut session_key_files = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(session_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if is_session_key_markdown_name(name) && !is_subagent_markdown_filename(name) {
                session_key_files.push(path);
            }
        }
    }
    session_key_files.sort();
    session_key_files.into_iter().next()
}

/// Whether `path` names a canonical `{session_id}.md` file.
pub fn is_trajectory_markdown_path(path: impl AsRef<Path>) -> bool {
    let Some(name) = path.as_ref().file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    is_session_key_markdown_name(name)
}

fn is_session_key_markdown_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    stem.starts_with("agent-")
        || stem.starts_with("run-")
        || (stem.contains('-') && stem.len() >= 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: std::path::PathBuf) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, "# test\n").unwrap();
    }

    #[test]
    fn locate_run_bucket_resolves_run_stem_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-20260101-abc");
        touch(run_dir.join("run-20260101-abc.md"));

        assert_eq!(
            locate_run_bucket_markdown(&run_dir)
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some("run-20260101-abc.md".to_string())
        );
    }

    #[test]
    fn locate_run_bucket_returns_none_for_non_run_directory() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("flat-session");
        touch(run_dir.join("flat-session.md"));
        assert!(locate_run_bucket_markdown(&run_dir).is_none());
    }

    #[test]
    fn subagent_session_key_does_not_inherit_run_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-capture-001");
        touch(run_dir.join("run-capture-001.md"));
        touch(run_dir.join("agent-worker.md"));

        assert_eq!(
            locate_session_markdown_for_key(&run_dir, "agent-worker")
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some("agent-worker.md".to_string())
        );
    }

    #[test]
    fn main_session_prefers_run_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-main-bucket");
        touch(run_dir.join("run-main-bucket.md"));

        let resolved = locate_session_markdown_for_key(&run_dir, "header-session-uuid");
        assert_eq!(
            resolved.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some("run-main-bucket.md".to_string())
        );
    }

    #[test]
    fn canonical_session_markdown_detected() {
        assert!(is_trajectory_markdown_path("agent-abc123.md"));
        assert!(is_trajectory_markdown_path(
            "5e27e4a7-f42a-42a9-8448-79608bd95c53.md"
        ));
        assert!(!is_trajectory_markdown_path("notes.md"));
        assert!(!is_trajectory_markdown_path("0001.md"));
        assert!(!is_trajectory_markdown_path("trajectory.tlv.md"));
    }

    #[test]
    fn subagent_key_does_not_fallback_to_main_run_md() {
        let dir = tempfile::tempdir().unwrap();
        let main_md = dir.path().join("run-20260524-160709-170794000.md");
        std::fs::write(&main_md, "main").unwrap();
        let subagent_key = "agent-ad67e572475568b5a";
        assert!(is_subagent_session_storage_key(subagent_key));
        assert!(!is_subagent_session_storage_key(
            "37343ad1-ed7d-49dc-b080-9c4afd9873c2"
        ));
        assert_eq!(
            locate_session_markdown_for_key(dir.path(), subagent_key),
            None
        );
        assert_eq!(
            session_markdown_write_path_for_key(dir.path(), subagent_key),
            dir.path().join("agent-ad67e572475568b5a.md")
        );
        assert_eq!(
            locate_session_markdown_for_key(dir.path(), "37343ad1-ed7d-49dc-b080-9c4afd9873c2"),
            None
        );
        assert!(main_md.is_file());
    }

    #[test]
    fn encoded_session_ids_are_disambiguated() {
        assert_ne!(
            sanitize_session_filename("a/b"),
            sanitize_session_filename("a?b")
        );
        assert_ne!(
            sanitize_session_filename(&"x".repeat(129)),
            sanitize_session_filename(&format!("{}y", "x".repeat(128)))
        );
    }

    #[test]
    fn existing_legacy_filename_is_read_without_reusing_legacy_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("a_b.md");
        std::fs::write(&legacy, "legacy").unwrap();
        assert_eq!(
            locate_session_markdown_for_key(dir.path(), "a/b"),
            Some(legacy)
        );
        assert_ne!(session_markdown_filename("a/b"), "a_b.md");
    }

    #[test]
    fn main_session_prefers_run_md_over_subagent_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-20260524-161537-122998000");
        std::fs::create_dir_all(&run_dir).unwrap();
        let main_md = run_dir.join("run-20260524-161537-122998000.md");
        std::fs::write(&main_md, "main").unwrap();
        std::fs::write(run_dir.join("agent-a2560e716f0b8b526.md"), "sub").unwrap();
        std::fs::write(run_dir.join("agent-a0df18417539eecd0.md"), "sub2").unwrap();

        let header_session = "fb47835b-e10d-4b29-abc3-68f4594ebce3";
        assert_eq!(
            locate_session_markdown_for_key(&run_dir, header_session),
            Some(main_md.clone())
        );
        assert_eq!(
            session_markdown_write_path_for_key(&run_dir, header_session),
            main_md
        );
        assert_eq!(
            locate_session_markdown(&run_dir),
            Some(run_dir.join("run-20260524-161537-122998000.md"))
        );
    }
}
