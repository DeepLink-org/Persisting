//! Aggregate judge sidecar rows under `{run}/layers/`.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use persisting_proto::{
    JudgeRubricSummary, JudgeStatsSession, SessionJudgeStats, TrajectoryJudgeStatsRequest,
    TrajectoryJudgeStatsResponse,
};

use super::expand::{drop_lifecycle_run_partitions, expand_story_locations};
use super::layers::{
    layers_dir, load_manifest, read_judge_rows, JudgeRow, MANUAL_RATIONALE_PREFIX, STORY_CALL_ID,
};
use super::path::{list_traj_read_locations, StoryCoords};

pub async fn judge_stats_async(
    request: TrajectoryJudgeStatsRequest,
) -> Result<TrajectoryJudgeStatsResponse> {
    let mut locations = list_traj_read_locations(
        request.storage.clone(),
        request.agent_id.clone(),
        request.session_id.clone(),
        request.root_session_id.clone(),
    )?;
    if request.session_id.is_none() {
        locations = expand_story_locations(locations).await?;
        locations = drop_lifecycle_run_partitions(locations);
    }
    if locations.is_empty() {
        anyhow::bail!("judge stats: no sessions found under {}", request.storage);
    }

    let mut rows_by_run: HashMap<RunKey, Vec<JudgeRow>> = HashMap::new();
    for loc in &locations {
        let run = run_bucket_coords(loc);
        rows_by_run.entry(run).or_insert_with(|| Vec::new());
    }
    for (run, rows) in rows_by_run.iter_mut() {
        *rows = load_judge_rows_for_run(run).await?;
    }

    let mut all_rows = Vec::new();
    let mut seen_row = HashSet::new();
    for rows in rows_by_run.values() {
        for r in rows {
            let key = (r.session_id.clone(), r.call_id.clone(), r.rubric_id.clone());
            if seen_row.insert(key) {
                all_rows.push(r.clone());
            }
        }
    }

    let mut sessions = Vec::with_capacity(locations.len());
    for loc in &locations {
        let run = run_bucket_coords(loc);
        let run_rows = rows_by_run.get(&run).map(Vec::as_slice).unwrap_or(&[]);
        let scoped: Vec<_> = run_rows
            .iter()
            .filter(|r| r.session_id == loc.session_id)
            .cloned()
            .collect();
        sessions.push(session_entry(loc, &run, &scoped));
    }

    let rubrics = summarize_rubrics(&all_rows);
    let judged_session_count = sessions.iter().filter(|s| s.judgment_count > 0).count();
    let judgment_count = all_rows.len();
    let session_count = sessions.len();
    let rubric_count = rubrics.len();
    let storage = request.storage.clone();
    let note = format!(
        "Judge stats: {judged_session_count}/{session_count} session(s) with sidecar rows, {judgment_count} judgment(s), {rubric_count} rubric(s)"
    );

    Ok(TrajectoryJudgeStatsResponse {
        storage: storage.clone(),
        session_count,
        judged_session_count,
        judgment_count,
        rubric_count,
        sessions,
        rubrics,
        status: if judgment_count > 0 {
            "ok".to_string()
        } else {
            "empty".to_string()
        },
        note,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RunKey {
    storage: String,
    agent_id: String,
    root: String,
}

fn run_bucket_coords(loc: &StoryCoords) -> RunKey {
    let root = loc
        .root_session_id
        .clone()
        .unwrap_or_else(|| loc.session_id.clone());
    RunKey {
        storage: loc.storage.clone(),
        agent_id: loc.agent_id.clone(),
        root,
    }
}

fn run_session_coords(key: &RunKey) -> StoryCoords {
    StoryCoords::new(
        &key.storage,
        &key.agent_id,
        &key.root,
        Some(key.root.clone()),
    )
}

async fn load_judge_rows_for_run(key: &RunKey) -> Result<Vec<JudgeRow>> {
    let run = run_session_coords(key);
    let layers = layers_dir(&run)?;
    if !layers.is_dir() {
        return Ok(Vec::new());
    }

    let mut rows = Vec::new();
    let manifest = load_manifest(&run)?;
    if !manifest.layers.is_empty() {
        for entry in &manifest.layers {
            if !entry.name.starts_with("judge_") {
                continue;
            }
            let path = layers.join(&entry.path);
            rows.extend(read_judge_rows(&path).await?);
        }
    } else {
        for entry in std::fs::read_dir(&layers)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("judge_") && name.ends_with(".vortex") {
                rows.extend(read_judge_rows(&entry.path()).await?);
            }
        }
    }
    Ok(rows)
}

/// Judge sidecar summary for one session (shared by `traj stats` and `traj judge stats`).
pub async fn session_judge_stats(loc: &StoryCoords) -> SessionJudgeStats {
    let run = run_bucket_coords(loc);
    let rows = load_judge_rows_for_run(&run).await.unwrap_or_default();
    let scoped: Vec<_> = rows
        .iter()
        .filter(|r| r.session_id == loc.session_id)
        .cloned()
        .collect();
    session_entry(loc, &run, &scoped).into()
}

fn session_entry(loc: &StoryCoords, run: &RunKey, rows: &[JudgeRow]) -> JudgeStatsSession {
    let (verdict_pass, verdict_partial, verdict_fail) = verdict_counts(rows);
    let manual_count = rows
        .iter()
        .filter(|r| r.rationale.starts_with(MANUAL_RATIONALE_PREFIX))
        .count();
    let turn_judgments = rows.iter().filter(|r| r.call_id != STORY_CALL_ID).count();
    let story_judgments = rows.len().saturating_sub(turn_judgments);
    let rubric_ids = distinct_rubric_ids(rows);
    let layers_path = layers_dir(&run_session_coords(run))
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    JudgeStatsSession {
        storage: loc.storage.clone(),
        agent_id: loc.agent_id.clone(),
        session_id: loc.session_id.clone(),
        root_session_id: loc.root_session_id.clone(),
        judgment_count: rows.len(),
        turn_judgments,
        story_judgments,
        rubric_ids,
        avg_score: avg_score(rows),
        verdict_pass,
        verdict_partial,
        verdict_fail,
        manual_count,
        layers_path,
        status: if rows.is_empty() {
            "empty".to_string()
        } else {
            "ok".to_string()
        },
    }
}

fn summarize_rubrics(rows: &[JudgeRow]) -> Vec<JudgeRubricSummary> {
    let mut ids = distinct_rubric_ids(rows);
    ids.sort();
    ids.into_iter()
        .map(|rubric_id| {
            let scoped: Vec<_> = rows.iter().filter(|r| r.rubric_id == rubric_id).collect();
            let (verdict_pass, verdict_partial, verdict_fail) = verdict_counts_ref(&scoped);
            let manual_count = scoped
                .iter()
                .filter(|r| r.rationale.starts_with(MANUAL_RATIONALE_PREFIX))
                .count();
            JudgeRubricSummary {
                rubric_id,
                judgment_count: scoped.len(),
                avg_score: avg_score_ref(&scoped).unwrap_or(0.0),
                verdict_pass,
                verdict_partial,
                verdict_fail,
                manual_count,
            }
        })
        .collect()
}

fn distinct_rubric_ids(rows: &[JudgeRow]) -> Vec<String> {
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r.rubric_id.clone());
    }
    let mut v: Vec<_> = set.into_iter().collect();
    v.sort();
    v
}

fn verdict_counts(rows: &[JudgeRow]) -> (usize, usize, usize) {
    verdict_counts_ref(&rows.iter().collect::<Vec<_>>())
}

fn verdict_counts_ref(rows: &[&JudgeRow]) -> (usize, usize, usize) {
    let mut pass = 0usize;
    let mut partial = 0usize;
    let mut fail = 0usize;
    for r in rows {
        match r.verdict.as_str() {
            "pass" => pass += 1,
            "partial" => partial += 1,
            "fail" => fail += 1,
            _ => {}
        }
    }
    (pass, partial, fail)
}

fn avg_score(rows: &[JudgeRow]) -> Option<f64> {
    avg_score_ref(&rows.iter().collect::<Vec<_>>())
}

fn avg_score_ref(rows: &[&JudgeRow]) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }
    let sum: i64 = rows.iter().map(|r| r.score).sum();
    Some(sum as f64 / rows.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::judge::judge_async;
    use persisting_capture::config::CaptureLevel;
    use persisting_capture::engine::Call;
    use persisting_capture::sink::{llm_request_summary_record, llm_response_record_with_content};
    use persisting_proto::{
        JudgeMethod, JudgeScope, JudgeScoreInput, TrajectoryAppendRequest, TrajectoryJudgeRequest,
        TrajectoryJudgeStatsRequest, TrajectoryStorageFormat,
    };

    use crate::trajectory::append_async;

    #[tokio::test]
    async fn judge_stats_aggregates_sessions_and_rubrics() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store").to_string_lossy().into_owned();
        let call = Call {
            call_id: "c1".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("story-a".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("story-a".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("ok".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let lines = [
            persisting_capture::record::record_to_engine_line(&req).unwrap(),
            persisting_capture::record::record_to_engine_line(&resp).unwrap(),
        ];
        append_async(TrajectoryAppendRequest {
            storage: storage.clone(),
            agent_id: "agent".into(),
            session_id: "story-a".into(),
            root_session_id: Some("run-1".into()),
            records_ronl: lines.join("\n") + "\n",
            storage_format: TrajectoryStorageFormat::Vortex,
        })
        .await
        .unwrap();

        judge_async(TrajectoryJudgeRequest {
            storage: storage.clone(),
            agent_id: "agent".into(),
            session_id: "story-a".into(),
            root_session_id: Some("run-1".into()),
            rubric_id: "quality".into(),
            rubric_ids: vec![],
            scope: JudgeScope::Turn,
            method: JudgeMethod::Manual,
            model: None,
            dry_run: false,
            force: false,
            manual_scores: vec![JudgeScoreInput {
                call_id: Some("c1".into()),
                rubric_id: "quality".into(),
                score: 90,
                verdict: "pass".into(),
                rationale: "good".into(),
            }],
            few_shot_limit: 0,
        })
        .await
        .unwrap();

        let out = judge_stats_async(TrajectoryJudgeStatsRequest {
            storage: storage.clone(),
            agent_id: Some("agent".into()),
            session_id: None,
            root_session_id: None,
        })
        .await
        .unwrap();

        assert_eq!(out.judgment_count, 1);
        assert_eq!(out.judged_session_count, 1);
        assert_eq!(out.rubrics.len(), 1);
        assert_eq!(out.rubrics[0].rubric_id, "quality");
        assert!(out
            .sessions
            .iter()
            .any(|s| s.session_id == "story-a" && s.judgment_count == 1));
    }
}
