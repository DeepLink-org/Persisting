//! `trajectory add` input → engine append lines; storage backend from input filename.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use persisting_capture::dialogue::import_markdown_to_engine_lines;
use persisting_capture::markdown_trajectory::is_trajectory_markdown_path;
use persisting_capture::record::json_to_engine_line;
use persisting_proto::TrajectoryStorageFormat;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum TrajectoryStorageCli {
    #[default]
    Auto,
    Lance,
    Markdown,
    Both,
}

impl From<TrajectoryStorageCli> for TrajectoryStorageFormat {
    fn from(v: TrajectoryStorageCli) -> Self {
        match v {
            TrajectoryStorageCli::Auto => TrajectoryStorageFormat::Auto,
            TrajectoryStorageCli::Lance => TrajectoryStorageFormat::Lance,
            TrajectoryStorageCli::Markdown => TrajectoryStorageFormat::Markdown,
            TrajectoryStorageCli::Both => TrajectoryStorageFormat::Both,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum TrajectoryAddFormat {
    /// Infer from `--input` path (`0001.md` / legacy `.tlv.md` → markdown, `.jsonl` → jsonl, …).
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
            TrajectoryAddFormat::Markdown => import_markdown_to_engine_lines(raw),
            TrajectoryAddFormat::Jsonl => lines_from_jsonl(raw),
            TrajectoryAddFormat::Toml => lines_from_toml(raw),
            TrajectoryAddFormat::Auto => {
                bail!("internal error: resolve add format before prepare_append_batch")
            }
        }
    }
}

/// Numbered session markdown (`0001.md`) or legacy `.tlv.md` → markdown storage; …
pub fn infer_storage_format_from_path(input_path: &str) -> Option<TrajectoryStorageFormat> {
    if input_path == "-" {
        return None;
    }
    if is_trajectory_markdown_path(Path::new(input_path)) {
        return Some(TrajectoryStorageFormat::Markdown);
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
            json_to_engine_line(&v)
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
            json_to_engine_line(&v)
        })
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_md_uses_markdown_storage_and_parser() {
        let p = "examples/foo/0001.md";
        assert_eq!(
            infer_storage_format_from_path(p),
            Some(TrajectoryStorageFormat::Markdown)
        );
        assert_eq!(
            TrajectoryFormatManager::resolve_add_format(p, TrajectoryAddFormat::Auto).unwrap(),
            TrajectoryAddFormat::Markdown
        );
    }

    #[test]
    fn legacy_tlv_md_still_markdown() {
        let p = "examples/foo/trajectory.tlv.md";
        assert_eq!(
            infer_storage_format_from_path(p),
            Some(TrajectoryStorageFormat::Markdown)
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
            TrajectoryFormatManager::resolve_storage_format("0001.md", TrajectoryStorageCli::Lance),
            TrajectoryStorageFormat::Lance
        );
    }
}
