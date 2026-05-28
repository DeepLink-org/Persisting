//! Lightweight post-run reconcile: compare live markdown call_ids vs Lance dialogue records.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::engine::{rebuild_session_story, story_call_ids, story_user_turn_count};
use crate::markdown_trajectory::read_blocks_from_file;
use crate::record::CaptureRecord;
use crate::storage::markdown_pipeline::MarkdownPipeline;

/// Per-session reconcile outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionReconcile {
    pub session_id: String,
    pub md_path: String,
    pub md_block_count: usize,
    pub md_call_ids: Vec<String>,
    pub lance_call_ids: Vec<String>,
    pub story_call_ids: Vec<String>,
    pub story_turn_count: u64,
    pub missing_in_md: Vec<String>,
    pub extra_in_md: Vec<String>,
    pub story_missing_in_md: Vec<String>,
    pub story_extra_in_md: Vec<String>,
    pub structural_issues: Vec<String>,
}

impl SessionReconcile {
    pub fn ok(&self) -> bool {
        self.missing_in_md.is_empty()
            && self.extra_in_md.is_empty()
            && self.story_missing_in_md.is_empty()
            && self.story_extra_in_md.is_empty()
            && self.structural_issues.is_empty()
    }
}

/// Run-level reconcile report written to `{storage}/.capture/reconcile.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunReconcileReport {
    pub root_session: String,
    pub agent_id: String,
    pub ok: bool,
    pub finished_at: String,
    pub sessions: Vec<SessionReconcile>,
}

/// List `{run_dir}/*.md` trajectory files (sorted).
pub fn list_run_markdown_paths(run_dir: &Path) -> Result<Vec<PathBuf>> {
    if !run_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in
        std::fs::read_dir(run_dir).with_context(|| format!("read_dir {}", run_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Extract visible dialogue `call_id`s that Lance records would materialize into markdown.
pub fn expected_markdown_call_ids(records: &[CaptureRecord]) -> BTreeSet<String> {
    MarkdownPipeline::call_ids_from_records(records)
}

/// Index one live markdown file: block count, call_ids, structural issues.
pub fn index_markdown_path(path: &Path) -> Result<(usize, BTreeSet<String>, Vec<String>)> {
    let raw = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };
    let structural = structural_issues(&raw);
    let blocks = read_blocks_from_file(path)?;
    let mut ids = BTreeSet::new();
    for block in &blocks {
        if let Some(id) = block.header.fields.get("call_id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                ids.insert(id.to_string());
            }
        }
    }
    Ok((blocks.len(), ids, structural))
}

fn structural_issues(raw: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if raw.contains("\n\n\n\n") {
        issues.push("excessive_blank_lines".into());
    }
    issues
}

fn set_diff(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let missing: Vec<_> = expected.difference(actual).cloned().collect();
    let extra: Vec<_> = actual.difference(expected).cloned().collect();
    (missing, extra)
}

fn sorted_vec(set: &BTreeSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

/// Compare one markdown file against Lance records for the same session.
pub fn reconcile_session(
    session_id: &str,
    root_session: &str,
    md_path: &Path,
    lance_records: &[CaptureRecord],
) -> Result<SessionReconcile> {
    let (md_block_count, md_ids, structural_issues) = index_markdown_path(md_path)?;
    let lance_ids = expected_markdown_call_ids(lance_records);
    let (missing_in_md, extra_in_md) = set_diff(&lance_ids, &md_ids);

    let story = if lance_records.is_empty() {
        None
    } else {
        Some(rebuild_session_story(
            session_id,
            root_session,
            lance_records,
        ))
    };
    let (story_call_ids_vec, story_turn_count, story_missing_in_md, story_extra_in_md) =
        if let Some(ref story) = story {
            let ids = story_call_ids(story);
            let (sm, se) = set_diff(&ids, &md_ids);
            (sorted_vec(&ids), story_user_turn_count(story), sm, se)
        } else {
            (Vec::new(), 0, Vec::new(), Vec::new())
        };

    Ok(SessionReconcile {
        session_id: session_id.to_string(),
        md_path: md_path.display().to_string(),
        md_block_count,
        md_call_ids: sorted_vec(&md_ids),
        lance_call_ids: sorted_vec(&lance_ids),
        story_call_ids: story_call_ids_vec,
        story_turn_count,
        missing_in_md,
        extra_in_md,
        story_missing_in_md,
        story_extra_in_md,
        structural_issues,
    })
}

/// Build a run-level report from per-session Lance record batches keyed by storage session id.
pub fn build_run_report(
    root_session: &str,
    agent_id: &str,
    run_dir: &Path,
    lance_by_session: &BTreeMap<String, Vec<CaptureRecord>>,
) -> Result<RunReconcileReport> {
    let mut sessions = Vec::new();
    for md_path in list_run_markdown_paths(run_dir)? {
        let session_id = md_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let records = lance_by_session
            .get(&session_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        sessions.push(reconcile_session(
            &session_id,
            root_session,
            &md_path,
            records,
        )?);
    }
    let ok = sessions.iter().all(SessionReconcile::ok);
    Ok(RunReconcileReport {
        root_session: root_session.to_string(),
        agent_id: agent_id.to_string(),
        ok,
        finished_at: chrono::Utc::now().to_rfc3339(),
        sessions,
    })
}

/// Write report to `{storage}/.capture/reconcile.json`.
pub fn write_run_reconcile_report(storage: &Path, report: &RunReconcileReport) -> Result<PathBuf> {
    let dir = storage.join(".capture");
    std::fs::create_dir_all(&dir).context("create .capture")?;
    let path = dir.join("reconcile.json");
    let json = serde_json::to_string_pretty(report).context("serialize reconcile report")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::markdown_trajectory::{encode_block_with_header, BlockHeader};
    use crate::sink::{llm_request_summary_record, llm_response_record_with_content};
    use crate::Call;
    use std::collections::BTreeMap as Map;

    fn block(call_id: &str, role: &str, body: &str) -> String {
        let mut fields = Map::new();
        fields.insert("call_id".into(), serde_json::json!(call_id));
        fields.insert("role".into(), serde_json::json!(role));
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
    fn reconcile_detects_missing_and_extra_call_ids() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "{}\n{}",
                block("call-a", "user", "hello"),
                block("call-a", "assistant", "hi")
            ),
        )
        .unwrap();

        let call = Call {
            call_id: "call-a".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("run-test".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            1,
            "chat",
            "openai",
            Some("hello".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("run-test".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({ "status": 200 }),
            false,
            Some("hi".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let mut extra_call = call;
        extra_call.call_id = "call-b".into();
        let orphan = llm_request_summary_record(
            Some("run-test".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            1,
            "chat",
            "openai",
            Some("orphan".into()),
            None,
            &extra_call,
            CaptureLevel::Dialogue,
            None,
        );

        let report = reconcile_session("run-test", "run-test", &md, &[req, resp, orphan]).unwrap();
        assert!(report.extra_in_md.is_empty());
        assert_eq!(report.missing_in_md, vec!["call-b"]);
        assert!(!report.ok());
    }

    #[test]
    fn structural_issue_flags_excessive_blank_lines() {
        assert!(structural_issues("a\n\n\n\nb").contains(&"excessive_blank_lines".into()));
        assert!(structural_issues("a\n\nb").is_empty());
    }

    #[test]
    fn expected_call_ids_skips_replayed_requests() {
        let call = Call {
            call_id: "call-a".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let mut req1 = llm_request_summary_record(
            Some("run".into()),
            Some("agent".into()),
            "m",
            "/v1/messages",
            1,
            "messages",
            "anthropic",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        req1.call_id = Some("call-a".into());
        req1.payload["user_message_count"] = serde_json::json!(1);

        let mut req2 = req1.clone();
        req2.call_id = Some("call-replay".into());

        let mut req3 = req1.clone();
        req3.call_id = Some("call-b".into());
        req3.payload["user_message_count"] = serde_json::json!(2);
        req3.payload["user_content"] = serde_json::json!("你好");

        let ids = expected_markdown_call_ids(&[req1, req2, req3]);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("call-a"));
        assert!(ids.contains("call-b"));
        assert!(!ids.contains("call-replay"));
    }

    #[test]
    fn reconcile_story_projection_aligns_when_md_matches() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "{}\n{}",
                block("call-a", "user", "hello"),
                block("call-a", "assistant", "hi")
            ),
        )
        .unwrap();

        let call = Call {
            call_id: "call-a".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("run-test".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            1,
            "chat",
            "openai",
            Some("hello".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("run-test".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({ "status": 200 }),
            false,
            Some("hi".into()),
            &call,
            CaptureLevel::Dialogue,
        );

        let report = reconcile_session("run-test", "run-test", &md, &[req, resp]).unwrap();
        assert!(report.ok());
        assert_eq!(report.story_turn_count, 1);
        assert_eq!(report.story_call_ids, vec!["call-a"]);
        assert!(report.story_missing_in_md.is_empty());
        assert!(report.story_extra_in_md.is_empty());
    }

    #[test]
    fn reconcile_story_detects_extra_md_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "{}\n{}\n{}",
                block("call-a", "user", "hello"),
                block("call-a", "assistant", "hi"),
                block("call-ghost", "user", "phantom")
            ),
        )
        .unwrap();

        let call = Call {
            call_id: "call-a".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("run-test".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            1,
            "chat",
            "openai",
            Some("hello".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("run-test".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("hi".into()),
            &call,
            CaptureLevel::Dialogue,
        );

        let report = reconcile_session("run-test", "run-test", &md, &[req, resp]).unwrap();
        assert!(report.story_extra_in_md.contains(&"call-ghost".to_string()));
        assert!(!report.ok());
    }

    #[test]
    fn build_run_report_ok_when_all_sessions_match() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-test");
        std::fs::create_dir_all(&run_dir).unwrap();
        let md = run_dir.join("run-test.md");
        std::fs::write(
            &md,
            format!(
                "{}\n{}",
                block("call-a", "user", "hello"),
                block("call-a", "assistant", "hi")
            ),
        )
        .unwrap();

        let call = Call {
            call_id: "call-a".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("run-test".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            1,
            "chat",
            "openai",
            Some("hello".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("run-test".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("hi".into()),
            &call,
            CaptureLevel::Dialogue,
        );

        let mut lance_by_session = Map::new();
        lance_by_session.insert("run-test".to_string(), vec![req, resp]);
        let report = build_run_report("run-test", "agent", &run_dir, &lance_by_session).unwrap();
        assert!(report.ok);
        assert_eq!(report.sessions.len(), 1);
    }
}
