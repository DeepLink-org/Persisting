//! `trajectory add` input → pChronicle event lines; storage target inferred separately.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use persisting_pchronicle::TrajectoryStorageFormat;
use persisting_pchronicle::{
    encode_event_lines, is_trajectory_markdown_path, markdown_document_to_event_lines, EventRecord,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum TrajectoryStorageCli {
    /// Use the canonical trajectory store.
    #[default]
    Auto,
    /// Lance raw event log (canonical).
    Lance,
}

impl From<TrajectoryStorageCli> for TrajectoryStorageFormat {
    fn from(v: TrajectoryStorageCli) -> Self {
        match v {
            TrajectoryStorageCli::Auto => TrajectoryStorageFormat::Auto,
            TrajectoryStorageCli::Lance => TrajectoryStorageFormat::Lance,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum TrajectoryAddFormat {
    /// Infer from `--input` path (`{session_id}.md` → markdown, `.jsonl` → jsonl, …).
    #[default]
    Auto,
    Toml,
    Jsonl,
    Markdown,
}

pub struct TrajectoryFormatManager;

impl TrajectoryFormatManager {
    pub fn resolve_add_format(
        input_path: &str,
        explicit: TrajectoryAddFormat,
    ) -> Result<TrajectoryAddFormat> {
        match explicit {
            TrajectoryAddFormat::Auto => infer_add_format_from_path(input_path),
            f => Ok(f),
        }
    }

    pub fn resolve_storage_format(
        input_path: &str,
        explicit: TrajectoryStorageCli,
    ) -> TrajectoryStorageFormat {
        match explicit {
            TrajectoryStorageCli::Auto => {
                infer_storage_format_from_path(input_path).unwrap_or(TrajectoryStorageFormat::Auto)
            }
            f => f.into(),
        }
    }

    pub fn prepare_append_batch(format: TrajectoryAddFormat, raw: &str) -> Result<String> {
        match format {
            TrajectoryAddFormat::Markdown => Ok(markdown_document_to_event_lines(raw)?.join("\n")),
            TrajectoryAddFormat::Jsonl => lines_from_jsonl(raw),
            TrajectoryAddFormat::Toml => lines_from_toml(raw),
            TrajectoryAddFormat::Auto => {
                bail!("internal error: resolve add format before prepare_append_batch")
            }
        }
    }
}

/// Canonical session markdown (`{session_id}.md`) defaults to Lance append after parsing.
pub fn infer_storage_format_from_path(input_path: &str) -> Option<TrajectoryStorageFormat> {
    if input_path == "-" {
        return None;
    }
    if is_trajectory_markdown_path(Path::new(input_path)) {
        return Some(TrajectoryStorageFormat::Lance);
    }
    let lower = input_path.to_ascii_lowercase();
    if lower.ends_with(".jsonl")
        || lower.ends_with(".json")
        || lower.ends_with(".toml")
        || lower.ends_with(".ron")
    {
        return Some(TrajectoryStorageFormat::Lance);
    }
    None
}

fn infer_add_format_from_path(input_path: &str) -> Result<TrajectoryAddFormat> {
    if input_path == "-" {
        bail!("when --input is '-' (stdin), set --format to toml, jsonl, or markdown");
    }
    if is_trajectory_markdown_path(Path::new(input_path)) {
        return Ok(TrajectoryAddFormat::Markdown);
    }
    let lower = input_path.to_ascii_lowercase();
    if lower.ends_with(".jsonl") || lower.ends_with(".json") {
        return Ok(TrajectoryAddFormat::Jsonl);
    }
    if lower.ends_with(".toml") {
        return Ok(TrajectoryAddFormat::Toml);
    }
    Ok(TrajectoryAddFormat::Toml)
}

fn lines_from_jsonl(src: &str) -> Result<String> {
    src.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let v: serde_json::Value = serde_json::from_str(line.trim())
                .with_context(|| format!("jsonl line {}", i + 1))?;
            event_value_to_event_line(v).with_context(|| format!("jsonl line {}", i + 1))
        })
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

fn lines_from_toml(src: &str) -> Result<String> {
    let root: toml::Value = toml::from_str(src).context("parse trajectory TOML")?;
    let arr = root
        .get("records")
        .ok_or_else(|| anyhow::anyhow!("TOML must define `records` array"))?
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("`records` must be an array"))?;
    arr.iter()
        .enumerate()
        .map(|(i, item)| {
            let v = serde_json::to_value(item).with_context(|| format!("toml records[{i}]"))?;
            event_value_to_event_line(v).with_context(|| format!("toml records[{i}]"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

fn event_value_to_event_line(value: serde_json::Value) -> Result<String> {
    let event: EventRecord = serde_json::from_value(value).context("decode EventRecord")?;
    encode_event_lines(&[event])?
        .into_iter()
        .next()
        .context("encode EventRecord produced no line")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_session_md_uses_lance_storage_and_markdown_parser() {
        let p = "examples/foo/run-20260101-example.md";
        assert_eq!(
            infer_storage_format_from_path(p),
            Some(TrajectoryStorageFormat::Lance)
        );
        assert_eq!(
            TrajectoryFormatManager::resolve_add_format(p, TrajectoryAddFormat::Auto).unwrap(),
            TrajectoryAddFormat::Markdown
        );
    }

    #[test]
    fn jsonl_uses_lance_storage() {
        let p = "batch.jsonl";
        assert_eq!(
            infer_storage_format_from_path(p),
            Some(TrajectoryStorageFormat::Lance)
        );
        assert_eq!(
            TrajectoryFormatManager::resolve_add_format(p, TrajectoryAddFormat::Auto).unwrap(),
            TrajectoryAddFormat::Jsonl
        );
    }

    #[test]
    fn explicit_storage_overrides_filename() {
        assert_eq!(
            TrajectoryFormatManager::resolve_storage_format(
                "run-20260101-example.md",
                TrajectoryStorageCli::Lance
            ),
            TrajectoryStorageFormat::Lance
        );
    }

    #[test]
    fn stdin_auto_storage_stays_auto() {
        assert_eq!(
            TrajectoryFormatManager::resolve_storage_format("-", TrajectoryStorageCli::Auto),
            TrajectoryStorageFormat::Auto
        );
    }

    #[test]
    fn prepare_append_batch_jsonl() {
        let raw = r#"{"seq":0,"source":"test","kind":"note","timestamp":null,"session_id":null,"agent_id":null,"parent_uuid":null,"trace_id":null,"call_id":null,"subagent_id":null,"parent_agent_id":null,"branch":null,"parent_call_id":null,"payload":{"content":"x"}}"#;
        let out =
            TrajectoryFormatManager::prepare_append_batch(TrajectoryAddFormat::Jsonl, raw).unwrap();
        assert!(out.contains("kind"));
        assert!(out.lines().count() >= 1);
    }

    #[test]
    fn storage_cli_converts_to_proto() {
        assert!(matches!(
            TrajectoryStorageFormat::from(TrajectoryStorageCli::Lance),
            TrajectoryStorageFormat::Lance
        ));
    }

    #[test]
    fn resolve_capture_run_dir_to_header_session_stem() {
        use persisting_pchronicle::resolve_traj_read_location;

        let path = "store/deepseek-proxy/run-20260529-020451-705391000";
        if !std::path::Path::new(path).is_dir() {
            return;
        }
        let loc = resolve_traj_read_location("test", path.into(), None, None, None).unwrap();
        assert_eq!(
            loc.session_id, "5e0dfcdb-56ee-49d1-8921-4aeefeea3b17",
            "got session_id={}",
            loc.session_id
        );
    }
}
