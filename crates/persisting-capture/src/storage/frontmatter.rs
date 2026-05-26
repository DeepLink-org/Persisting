//! Session-level YAML frontmatter rollup for live markdown trajectories.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::markdown::{
    format_duration_human, is_subagent_session_storage_key, read_blocks_from_file, BLOCK_LAYOUT,
};
use super::session::{trajectory_run_dir, CaptureRoute};
use super::session_client::{resolve_client_meta_for_run_dir, SessionClientMeta};
use crate::engine::Story;
use crate::session_index::{SessionIndexStore, SessionSummary};

/// Rollup stats embedded in trajectory markdown frontmatter.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SessionFrontmatterSummary {
    pub session: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub turns: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<SessionClientMeta>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Serialize)]
struct FrontmatterDoc<'a> {
    format: &'static str,
    block: &'static str,
    #[serde(flatten)]
    summary: &'a SessionFrontmatterSummary,
}

/// Serialize YAML frontmatter block (including opening/closing `---`).
pub fn format_session_frontmatter(summary: &SessionFrontmatterSummary) -> Result<String> {
    let doc = FrontmatterDoc {
        format: "persisting:1.0",
        block: BLOCK_LAYOUT,
        summary,
    };
    let yaml = serde_yaml::to_string(&doc).context("serialize session frontmatter")?;
    Ok(format!("---\n{yaml}---\n\n"))
}

/// Build rollup from markdown blocks + `sessions.json` + run directory siblings.
///
/// When `story` is present, turn count comes from the story read model instead of markdown blocks.
pub fn build_session_frontmatter_summary(
    storage: &Path,
    agent_id: &str,
    route: &CaptureRoute,
    md_path: &Path,
    story: Option<&Story>,
) -> Result<SessionFrontmatterSummary> {
    let run_dir = trajectory_run_dir(storage, agent_id, route);
    let client = resolve_client_meta_for_run_dir(storage, &run_dir);

    let turns = turns_from_story_or_markdown(story, md_path)?;
    let index_row = lookup_session_index(storage, agent_id, &route.session_id);

    let (model, provider, started, duration, total_tokens, cost) =
        index_stats(index_row.as_ref(), &route.session_id);

    Ok(SessionFrontmatterSummary {
        session: route.session_id.clone(),
        agent: agent_id.to_string(),
        model,
        provider,
        started,
        duration,
        turns,
        total_tokens,
        estimated_cost_usd: cost,
        subagents: list_subagent_stems(&run_dir, &route.storage_session_id),
        client,
    })
}

fn turns_from_story_or_markdown(story: Option<&Story>, md_path: &Path) -> Result<u64> {
    if let Some(story) = story {
        return Ok(crate::engine::story_user_turn_count(story));
    }
    count_user_turns(md_path)
}

fn count_user_turns(md_path: &Path) -> Result<u64> {
    if !md_path.is_file() {
        return Ok(0);
    }
    let blocks = read_blocks_from_file(md_path)?;
    Ok(blocks.iter().filter(|b| b.role() == Some("user")).count() as u64)
}

fn lookup_session_index(
    storage: &Path,
    agent_id: &str,
    session_id: &str,
) -> Option<SessionSummary> {
    SessionIndexStore::load(storage).ok().and_then(|index| {
        index
            .sessions
            .into_iter()
            .find(|s| s.agent_id == agent_id && s.session_id == session_id)
    })
}

fn index_stats(
    row: Option<&SessionSummary>,
    fallback_session: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    u64,
    Option<f64>,
) {
    let Some(row) = row else {
        return (None, None, None, None, 0, None);
    };
    let model = (!row.model.is_empty() && row.model != "_unknown" && row.model != "unknown")
        .then(|| row.model.clone());
    let provider = (row.provider != "unknown").then(|| row.provider.clone());
    let started = Some(row.first_seen.to_rfc3339());
    let duration = Some(format_duration_between(row.first_seen, row.last_seen));
    let total_tokens = row.usage.total_tokens;
    let cost = (row.estimated_cost_usd > 0.0).then_some(row.estimated_cost_usd);
    let _ = fallback_session;
    (model, provider, started, duration, total_tokens, cost)
}

fn format_duration_between(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let secs = (end - start).num_seconds().max(0) as u64;
    format_duration_human(secs)
}

/// List subagent markdown stems in a run directory (excluding the main session file).
pub fn list_subagent_stems(run_dir: &Path, main_storage_key: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
        .filter(|stem| is_subagent_session_storage_key(stem))
        .filter(|stem| stem != main_storage_key)
        .collect();
    stems.sort();
    stems.dedup();
    stems
}

/// Replace the YAML preamble of an on-disk markdown trajectory (blocks unchanged).
pub fn refresh_document_frontmatter(
    storage: &Path,
    agent_id: &str,
    route: &CaptureRoute,
    md_path: &Path,
    story: Option<&Story>,
) -> Result<SessionFrontmatterSummary> {
    if !md_path.is_file() {
        anyhow::bail!("markdown file missing: {}", md_path.display());
    }
    let summary = build_session_frontmatter_summary(storage, agent_id, route, md_path, story)?;
    let content =
        std::fs::read_to_string(md_path).with_context(|| format!("read {}", md_path.display()))?;
    let body_start = super::markdown::document_body_start(&content)?;
    let header = format_session_frontmatter(&summary)?;
    std::fs::write(md_path, format!("{header}{}", &content[body_start..]))
        .with_context(|| format!("write {}", md_path.display()))?;
    Ok(summary)
}

/// Refresh frontmatter for every `{run_dir}/*.md` file.
///
/// Loads `.capture/story_snapshots.json` when `stories` is not provided.
pub fn refresh_run_markdown_frontmatter(
    storage: &Path,
    agent_id: &str,
    root_session: &str,
    stories: Option<&std::collections::HashMap<String, Story>>,
) -> Result<Vec<(PathBuf, SessionFrontmatterSummary)>> {
    let run_dir = storage.join(agent_id).join(root_session);
    if !run_dir.is_dir() {
        return Ok(Vec::new());
    }
    let loaded = crate::engine::load_story_snapshots(storage).unwrap_or_default();
    let stories = stories.unwrap_or(&loaded);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&run_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let route = CaptureRoute::for_run_markdown_stem(root_session, stem);
        let story = stories.get(stem);
        let summary =
            refresh_document_frontmatter(storage, agent_id, &route, &path, story)?;
        out.push((path, summary));
    }
    out.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    Ok(out)
}

/// One-line human summary for stderr (run end).
pub fn format_run_summary_line(md_path: &Path, summary: &SessionFrontmatterSummary) -> String {
    let cost = summary
        .estimated_cost_usd
        .map(|c| format!(" ${c:.4}"))
        .unwrap_or_default();
    let duration = summary.duration.as_deref().unwrap_or("?");
    let subagents = if summary.subagents.is_empty() {
        String::new()
    } else {
        format!(" | subagents: {}", summary.subagents.join(", "))
    };
    format!(
        "Session: {} | turns: {} | tokens: {}{} | duration: {}{}",
        md_path.display(),
        summary.turns,
        summary.total_tokens,
        cost,
        duration,
        subagents
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_trajectory::{encode_block_with_header, BlockHeader};
    use std::collections::BTreeMap;

    fn user_block(body: &str) -> String {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), serde_json::json!("user"));
        encode_block_with_header(
            BlockHeader {
                type_name: "dialogue".into(),
                length: body.len(),
                fields,
            },
            body.as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn refresh_frontmatter_preserves_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "---\nformat: persisting:1.0\n---\n\n{}",
                user_block("hello")
            ),
        )
        .unwrap();
        let route = CaptureRoute {
            root_session: Some("run-test".into()),
            session_id: "run-test".into(),
            storage_session_id: "run-test".into(),
            subagent_id: None,
        };
        let summary = refresh_document_frontmatter(dir.path(), "agent", &route, &md, None).unwrap();
        assert_eq!(summary.turns, 1);
        assert_eq!(summary.session, "run-test");
        let text = std::fs::read_to_string(&md).unwrap();
        assert!(text.contains("turns: 1"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn frontmatter_prefers_story_turn_count_over_markdown_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "---\nformat: persisting:1.0\n---\n\n{}",
                user_block("only one block in md")
            ),
        )
        .unwrap();
        let route = CaptureRoute {
            root_session: Some("run-test".into()),
            session_id: "run-test".into(),
            storage_session_id: "run-test".into(),
            subagent_id: None,
        };

        let call = crate::Call {
            call_id: "c1".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let mut records = Vec::new();
        for text in ["first", "second"] {
            records.push(crate::sink::llm_request_summary_record(
                Some("run-test".into()),
                Some("agent".into()),
                "m",
                "/v1/chat/completions",
                10,
                "chat",
                "openai",
                Some(text.into()),
                None,
                &call,
                crate::config::CaptureLevel::Dialogue,
                None,
            ));
        }
        let story = crate::engine::rebuild_session_story("run-test", "run-test", &records);

        let from_md =
            build_session_frontmatter_summary(dir.path(), "agent", &route, &md, None).unwrap();
        assert_eq!(from_md.turns, 1);

        let from_story =
            build_session_frontmatter_summary(dir.path(), "agent", &route, &md, Some(&story))
                .unwrap();
        assert_eq!(from_story.turns, 2);
    }

    #[test]
    fn refresh_run_uses_persisted_story_snapshots() {
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        let agent = "agent-1";
        let root = "run-test";
        let run_dir = dir.path().join(agent).join(root);
        std::fs::create_dir_all(&run_dir).unwrap();
        let md = run_dir.join(format!("{root}.md"));
        std::fs::write(
            &md,
            format!(
                "---\nformat: persisting:1.0\n---\n\n{}",
                user_block("hello")
            ),
        )
        .unwrap();

        let call = crate::Call {
            call_id: "c1".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = crate::sink::llm_request_summary_record(
            Some(root.into()),
            Some(agent.into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            Some("hello".into()),
            None,
            &call,
            crate::config::CaptureLevel::Dialogue,
            None,
        );
        let mut req2 = req.clone();
        req2.call_id = Some("c2".into());
        req2.payload["user_content"] = serde_json::json!("second turn");
        let story = crate::engine::rebuild_session_story(root, root, &[req, req2]);
        assert_eq!(crate::engine::story_user_turn_count(&story), 2);

        let mut snapshots = HashMap::new();
        snapshots.insert(root.to_string(), story);
        crate::engine::persist_story_snapshots(dir.path(), &snapshots).unwrap();

        let entries =
            refresh_run_markdown_frontmatter(dir.path(), agent, root, None).unwrap();
        let summary = entries
            .iter()
            .find(|(p, _)| p == &md)
            .map(|(_, s)| s)
            .expect("main session summary");
        assert_eq!(summary.turns, 2);
    }
}
