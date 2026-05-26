//! Post-run reconcile: replay Lance via engine, compare with live markdown.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use persisting_capture::{
    build_run_report, run_env::read_run_session, write_run_reconcile_report, CaptureRecord,
    RunReconcileReport,
};
use persisting_proto::{
    TrajectoryReplayRequest, TrajectoryReplayResponse, TrajectoryStorageFormat,
};

use super::CaptureFormat;

/// Replay all sessions under a capture run and write `.capture/reconcile.json`.
pub fn reconcile_run_after_flush(
    storage: &Path,
    agent_id: &str,
    format: CaptureFormat,
    mut replay: impl FnMut(TrajectoryReplayRequest) -> Result<TrajectoryReplayResponse>,
) -> Result<RunReconcileReport> {
    if !format.stream_markdown_in_engine() {
        anyhow::bail!("reconcile requires markdown capture format");
    }
    let root_session = read_run_session(storage)
        .with_context(|| format!("read run_session under {}", storage.display()))?;
    let run_dir = storage.join(agent_id).join(&root_session);
    let md_paths = persisting_capture::list_run_markdown_paths(&run_dir)?;
    if md_paths.is_empty() {
        anyhow::bail!(
            "no markdown files under {}; nothing to reconcile",
            run_dir.display()
        );
    }

    let storage_fmt: TrajectoryStorageFormat = format.into();
    let mut lance_by_session = BTreeMap::new();
    for md_path in &md_paths {
        let session_id = md_path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("markdown path stem")?
            .to_string();
        let root = if session_id == root_session {
            None
        } else {
            Some(root_session.clone())
        };
        let resp = replay(TrajectoryReplayRequest {
            storage: storage.display().to_string(),
            agent_id: agent_id.to_string(),
            session_id: session_id.clone(),
            offset: 0,
            limit: None,
            storage_format: storage_fmt,
            root_session_id: root,
        })
        .with_context(|| format!("replay Lance session {session_id}"))?;
        let records = decode_replay_records(&resp.records)?;
        lance_by_session.insert(session_id, records);
    }

    let report = build_run_report(&root_session, agent_id, &run_dir, &lance_by_session)?;
    let path = write_run_reconcile_report(storage, &report)?;
    if report.ok {
        eprintln!(
            "[persisting-cli] capture reconcile: ok ({} sessions) → {}",
            report.sessions.len(),
            path.display()
        );
    } else {
        eprintln!(
            "[persisting-cli] capture reconcile: MISMATCH ({} sessions) → {}",
            report.sessions.len(),
            path.display()
        );
        for s in &report.sessions {
            if !s.ok() {
                eprintln!(
                    "[persisting-cli]   session {} missing={:?} extra={:?} structural={:?}",
                    s.session_id, s.missing_in_md, s.extra_in_md, s.structural_issues
                );
            }
        }
    }
    Ok(report)
}

fn decode_replay_records(lines: &[String]) -> Result<Vec<CaptureRecord>> {
    lines
        .iter()
        .enumerate()
        .map(|(i, json)| {
            serde_json::from_str::<CaptureRecord>(json)
                .with_context(|| format!("decode replay record[{i}]"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_replay_records_parses_json_lines() {
        let lines = vec![r#"{"seq":1,"source":"x","kind":"llm.request","payload":{}}"#.into()];
        let recs = decode_replay_records(&lines).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, "llm.request");
    }
}
