//! Import agent sessions into trajectory storage from IDE logs and agentgateway telemetry.

pub mod daemon;
mod debug_setup;
pub mod replay_dead_letter;
pub mod usage;
pub use debug_setup::{enable_if_requested as enable_capture_debug, CaptureDebugContext};
mod merge;
mod project_path;
pub mod providers;
mod record;

use std::fs::File;
use std::io::BufReader;

use anyhow::{Context, Result};
use clap::ValueEnum;
use persisting_pchronicle::TrajectoryStorageFormat;

pub use record::CaptureRecord;

/// Capture trajectory storage format.
///
/// Both modes persist canonical events to Lance. `md` additionally maintains a
/// best-effort live Markdown view; `lance` omits that view.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum CaptureFormat {
    /// Canonical Lance plus a live human-readable Markdown view.
    #[value(name = "md")]
    #[default]
    Markdown,
    /// Lance event log (canonical); no live Markdown sidecar during capture.
    #[value(name = "lance")]
    Lance,
}

impl From<CaptureFormat> for TrajectoryStorageFormat {
    fn from(_v: CaptureFormat) -> Self {
        TrajectoryStorageFormat::Lance
    }
}

impl CaptureFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Lance => "lance",
        }
    }

    pub fn writes_markdown(self) -> bool {
        matches!(self, Self::Markdown)
    }

    /// Live markdown upsert inside [`CaptureEngine`] (`-f md` only).
    pub fn stream_markdown_in_engine(self) -> bool {
        matches!(self, Self::Markdown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn capture_format_uses_canonical_lance_name() {
        assert_eq!(
            CaptureFormat::from_str("lance", false).unwrap(),
            CaptureFormat::Lance
        );
        assert!(CaptureFormat::from_str("bin", false).is_err());
    }

    #[test]
    fn lance_does_not_stream_live_markdown() {
        assert!(!CaptureFormat::Lance.stream_markdown_in_engine());
        assert!(!CaptureFormat::Lance.writes_markdown());
    }

    #[test]
    fn markdown_streams_live_markdown() {
        assert!(CaptureFormat::Markdown.stream_markdown_in_engine());
        assert!(CaptureFormat::Markdown.writes_markdown());
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum CaptureProvider {
    /// Claude Code + Cursor JSONL under `~/.claude` / `~/.cursor`.
    #[default]
    Ide,
    /// agentgateway OTLP / envelope JSONL (`--gateway-input`).
    Gateway,
    /// IDE logs and gateway export together.
    All,
}

pub struct CaptureImportOptions {
    pub providers: CaptureProvider,
    pub since_days: u64,
    pub project_filter: Option<String>,
    pub all_projects: bool,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub merge_subagents: bool,
    pub gateway_input: Option<String>,
    pub dry_run: bool,
}

pub fn collect_pending(opts: &CaptureImportOptions) -> Result<Vec<record::PendingRecord>> {
    let project_filter = if opts.all_projects {
        None
    } else if let Some(ref p) = opts.project_filter {
        Some(p.clone())
    } else {
        Some(project_path::encode_project_path(
            &std::env::current_dir()
                .context("current_dir")?
                .to_string_lossy(),
        ))
    };

    let mut pending = Vec::new();

    let use_ide = matches!(opts.providers, CaptureProvider::Ide | CaptureProvider::All);
    let use_gateway = matches!(
        opts.providers,
        CaptureProvider::Gateway | CaptureProvider::All
    );

    if use_ide {
        let pf = project_filter.as_deref();
        pending.extend(providers::claude::collect(
            providers::claude::ClaudeOptions {
                project_filter: pf,
                since_days: Some(opts.since_days),
                session_id: opts.session_id.as_deref(),
                merge_subagents: opts.merge_subagents,
            },
        )?);
        pending.extend(providers::cursor::collect(
            providers::cursor::CursorOptions {
                project_filter: pf,
                since_days: Some(opts.since_days),
                session_id: opts.session_id.as_deref(),
                merge_subagents: opts.merge_subagents,
            },
        )?);
    }

    if use_gateway {
        let path = opts
            .gateway_input
            .as_deref()
            .context("gateway provider requires --gateway-input (file path or `-` for stdin)")?;
        let reader: Box<dyn std::io::Read> = if path == "-" {
            Box::new(std::io::stdin())
        } else {
            Box::new(File::open(path).with_context(|| format!("open gateway input {path}"))?)
        };
        pending.extend(providers::gateway::collect_from_reader(BufReader::new(
            reader,
        ))?);
    }

    Ok(pending)
}

pub struct CaptureImportSummary {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub record_count: usize,
    pub sources: std::collections::HashMap<String, usize>,
}

pub fn import_to_trajectory(
    storage: &str,
    opts: &CaptureImportOptions,
    append_records: impl FnOnce(&str, &str, &str, Vec<record::CaptureRecord>) -> Result<()>,
) -> Result<CaptureImportSummary> {
    let pending = collect_pending(opts)?;
    if pending.is_empty() {
        anyhow::bail!("capture: no events found (check --since, --project, providers)");
    }

    let session_id = merge::resolve_session_id(opts.session_id.as_deref(), &pending)?;
    let agent_id = opts
        .agent_id
        .clone()
        .unwrap_or_else(|| default_agent_id(opts));

    let filtered: Vec<_> = pending
        .into_iter()
        .filter(|p| {
            p.session_id
                .as_ref()
                .map(|s| s == &session_id)
                .unwrap_or(true)
        })
        .collect();

    let records = merge::merge_and_assign_seq(filtered);
    let summary = CaptureImportSummary {
        storage: storage.to_string(),
        agent_id: agent_id.clone(),
        session_id: session_id.clone(),
        record_count: records.len(),
        sources: count_sources(&records),
    };

    if opts.dry_run {
        return Ok(summary);
    }

    append_records(storage, &agent_id, &session_id, records)?;
    Ok(summary)
}

fn default_agent_id(opts: &CaptureImportOptions) -> String {
    if let Some(ref p) = opts.project_filter {
        return p.clone();
    }
    if opts.all_projects {
        return "capture".to_string();
    }
    project_path::encode_project_path(
        &std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string()),
    )
}

fn count_sources(records: &[CaptureRecord]) -> std::collections::HashMap<String, usize> {
    let mut m = std::collections::HashMap::new();
    for r in records {
        *m.entry(r.source.clone()).or_insert(0) += 1;
    }
    m
}
