//! Shared helpers for invoking user `--python` with a usable module path.
//!
//! Semantic block: [`crate::blocks::ids::PYTHON_ENV`].

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Build PYTHONPATH from an explicit existing value + extras (testable without mutating env).
pub fn merge_pythonpath_parts(existing: Option<&OsStr>, extras: &[PathBuf]) -> Option<String> {
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(cur) = existing {
        for p in env::split_paths(cur) {
            if !p.as_os_str().is_empty() && !parts.iter().any(|x| x == &p) {
                parts.push(p);
            }
        }
    }
    for e in extras {
        let p = e.canonicalize().unwrap_or_else(|_| e.to_path_buf());
        if !parts.iter().any(|x| x == &p) {
            parts.push(p);
        }
    }
    if parts.is_empty() {
        None
    } else {
        env::join_paths(&parts)
            .ok()
            .map(|os| os.to_string_lossy().into_owned())
    }
}

/// Build PYTHONPATH: existing env + extras (plan dir, `-E` paths, …).
pub fn merge_pythonpath(extras: &[PathBuf]) -> Option<String> {
    merge_pythonpath_parts(env::var_os("PYTHONPATH").as_deref(), extras)
}

/// Default extras for a plan script: its parent directory (so sibling modules import).
pub fn pythonpath_for_script(script: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(parent) = script.parent() {
        out.push(parent.to_path_buf());
    }
    out
}

pub fn apply_pythonpath(cmd: &mut tokio::process::Command, extras: &[PathBuf]) {
    if let Some(pp) = merge_pythonpath(extras) {
        cmd.env("PYTHONPATH", pp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn merge_dedups_and_preserves_order() {
        let existing = OsString::from("/a:/b");
        let extras = vec![PathBuf::from("/b"), PathBuf::from("/c")];
        let merged = merge_pythonpath_parts(Some(&existing), &extras).unwrap();
        let parts: Vec<_> = env::split_paths(&merged).collect();
        assert!(
            parts
                .iter()
                .any(|p| p.ends_with("a") || p == Path::new("/a"))
        );
        assert_eq!(
            parts
                .iter()
                .filter(|p| p.ends_with("b") || p.as_path() == Path::new("/b"))
                .count(),
            1
        );
        assert!(
            parts
                .iter()
                .any(|p| p.ends_with("c") || p == Path::new("/c"))
        );
    }

    #[test]
    fn merge_empty_returns_none() {
        assert!(merge_pythonpath_parts(None, &[]).is_none());
    }

    #[test]
    fn pythonpath_for_script_uses_parent() {
        let p = Path::new("/tmp/plans/task.py");
        let extras = pythonpath_for_script(p);
        assert_eq!(extras, vec![PathBuf::from("/tmp/plans")]);
    }
}
