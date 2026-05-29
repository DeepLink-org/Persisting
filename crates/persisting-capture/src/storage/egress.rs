//! Offline trajectory egress: export bundles and parse append payloads.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::story_coords::StoryCoords;

/// Result of copying story files to an export directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub out_dir: String,
    pub files_copied: usize,
    pub source_paths: Vec<String>,
    pub note: String,
}

/// Parse newline-separated RON engine lines for append (validates each line).
pub fn parse_engine_records(records_ronl: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (line_number, line) in records_ronl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _: ron::value::Value = ron::from_str(line).map_err(|err| {
            anyhow::anyhow!("invalid record at line {}: {}", line_number + 1, err)
        })?;
        out.push(line.to_string());
    }
    Ok(out)
}

/// Directories to copy for [`export_story_bundle`].
pub fn export_source_dirs(coords: &StoryCoords, include_subagents: bool) -> Result<Vec<PathBuf>> {
    let run = coords.run_dir()?;
    if coords.root_session_id.is_none() {
        return Ok(vec![run]);
    }
    let root = coords.root_session_id.as_ref().context("root_session_id")?;
    if include_subagents && coords.session_id == *root {
        return Ok(vec![run]);
    }
    if coords.session_id == *root {
        return Ok(vec![run]);
    }
    let sub = run.join("subagents").join(&coords.session_id);
    if sub.is_dir() {
        return Ok(vec![sub]);
    }
    Ok(vec![run])
}

/// Copy story tree(s) into `{out_dir}/{agent_id}/…` preserving relative layout under storage.
pub fn export_story_bundle(
    coords: &StoryCoords,
    out_dir: &Path,
    include_subagents: bool,
) -> Result<ExportOutcome> {
    let sources = export_source_dirs(coords, include_subagents)?;
    if sources.is_empty() {
        anyhow::bail!("no export source directories for story");
    }

    let storage = Path::new(&coords.storage);
    let mut files_copied = 0usize;
    let mut source_paths = Vec::new();

    for src in &sources {
        source_paths.push(src.display().to_string());
        let rel = src
            .strip_prefix(storage)
            .unwrap_or(src.as_path())
            .to_path_buf();
        let dst = out_dir.join(rel);
        files_copied += copy_dir_recursive(src, &dst)?;
    }

    let note = if include_subagents && coords.root_session_id.is_some() {
        format!(
            "exported {} file(s) from {} source dir(s) (include_subagents={include_subagents})",
            files_copied,
            sources.len()
        )
    } else {
        format!(
            "exported {} file(s) for story {} (agent {})",
            files_copied, coords.session_id, coords.agent_id
        )
    };

    Ok(ExportOutcome {
        out_dir: out_dir.display().to_string(),
        files_copied,
        source_paths,
        note,
    })
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<usize> {
    if !src.is_dir() {
        anyhow::bail!("export source is not a directory: {}", src.display());
    }
    std::fs::create_dir_all(dst).with_context(|| format!("create_dir_all {}", dst.display()))?;
    let mut n = 0usize;
    for entry in std::fs::read_dir(src).with_context(|| format!("read_dir {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            n += copy_dir_recursive(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_engine_records_skips_blanks() {
        let v = parse_engine_records("(a:1)\n\n(b:2)\n").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn export_flat_session() {
        let base = std::env::temp_dir().join(format!("cap-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = base.join("store");
        let session = store.join("agent-a").join("sess-1");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("sess-1.md"), "# x\n").unwrap();

        let coords = StoryCoords::new(store.to_str().unwrap(), "agent-a", "sess-1", None);
        let out = base.join("out");
        let r = export_story_bundle(&coords, &out, false).unwrap();
        assert!(r.files_copied >= 1);
        assert!(out
            .join("agent-a")
            .join("sess-1")
            .join("sess-1.md")
            .exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}
