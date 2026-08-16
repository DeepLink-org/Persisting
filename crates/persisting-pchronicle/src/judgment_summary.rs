//! Protocol-independent aggregation of persisted judgment columns.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::{
    drop_lifecycle_run_partitions, expand_story_locations, judgment_dataset_path,
    list_story_read_locations, read_judge_rows, JudgeRow, StoryCoords, MANUAL_RATIONALE_PREFIX,
    STORY_CALL_ID,
};

#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentSessionSummary {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub root_session_id: Option<String>,
    pub judgment_count: usize,
    pub turn_judgments: usize,
    pub story_judgments: usize,
    pub rubric_ids: Vec<String>,
    pub avg_score: Option<f64>,
    pub verdict_pass: usize,
    pub verdict_partial: usize,
    pub verdict_fail: usize,
    pub manual_count: usize,
    pub judgments_path: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentRubricSummary {
    pub rubric_id: String,
    pub judgment_count: usize,
    pub avg_score: f64,
    pub verdict_pass: usize,
    pub verdict_partial: usize,
    pub verdict_fail: usize,
    pub manual_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentAggregate {
    pub storage: String,
    pub session_count: usize,
    pub judged_session_count: usize,
    pub judgment_count: usize,
    pub rubric_count: usize,
    pub sessions: Vec<JudgmentSessionSummary>,
    pub rubrics: Vec<JudgmentRubricSummary>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RunKey {
    storage: String,
    agent_id: String,
    root: String,
}

fn run_bucket(location: &StoryCoords) -> RunKey {
    RunKey {
        storage: location.storage.clone(),
        agent_id: location.agent_id.clone(),
        root: location
            .root_session_id
            .clone()
            .unwrap_or_else(|| location.session_id.clone()),
    }
}

fn run_coords(key: &RunKey) -> StoryCoords {
    StoryCoords::new(
        &key.storage,
        &key.agent_id,
        &key.root,
        Some(key.root.clone()),
    )
}

async fn rows_for_run(key: &RunKey) -> Result<Vec<JudgeRow>> {
    read_judge_rows(&run_coords(key)).await
}

pub async fn session_judgment_summary(location: &StoryCoords) -> JudgmentSessionSummary {
    let run = run_bucket(location);
    let rows = rows_for_run(&run).await.unwrap_or_default();
    let scoped: Vec<_> = rows
        .into_iter()
        .filter(|row| row.session_id == location.session_id)
        .collect();
    session_entry(location, &run, &scoped).await
}

pub async fn aggregate_judgments(
    storage: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> Result<JudgmentAggregate> {
    let mut locations = list_story_read_locations(
        storage.clone(),
        agent_id,
        session_id.clone(),
        root_session_id,
    )?;
    if session_id.is_none() {
        locations = expand_story_locations(locations).await?;
        locations = drop_lifecycle_run_partitions(locations);
    }
    if locations.is_empty() {
        anyhow::bail!("judge stats: no sessions found under {storage}");
    }

    let mut rows_by_run: HashMap<RunKey, Vec<JudgeRow>> = HashMap::new();
    for location in &locations {
        rows_by_run.entry(run_bucket(location)).or_default();
    }
    for (run, rows) in &mut rows_by_run {
        *rows = rows_for_run(run).await?;
    }

    let mut all_rows = Vec::new();
    let mut seen = HashSet::new();
    for rows in rows_by_run.values() {
        for row in rows {
            let key = (
                row.session_id.clone(),
                row.call_id.clone(),
                row.rubric_id.clone(),
            );
            if seen.insert(key) {
                all_rows.push(row.clone());
            }
        }
    }

    let mut sessions = Vec::with_capacity(locations.len());
    for location in &locations {
        let run = run_bucket(location);
        let scoped: Vec<_> = rows_by_run
            .get(&run)
            .into_iter()
            .flatten()
            .filter(|row| row.session_id == location.session_id)
            .cloned()
            .collect();
        sessions.push(session_entry(location, &run, &scoped).await);
    }

    let rubrics = rubric_summaries(&all_rows);
    let judged_session_count = sessions
        .iter()
        .filter(|summary| summary.judgment_count > 0)
        .count();
    let session_count = sessions.len();
    let judgment_count = all_rows.len();
    let rubric_count = rubrics.len();
    Ok(JudgmentAggregate {
        storage,
        session_count,
        judged_session_count,
        judgment_count,
        rubric_count,
        sessions,
        rubrics,
        status: if judgment_count > 0 { "ok" } else { "empty" }.into(),
        note: format!(
            "Judge stats: {judged_session_count}/{session_count} session(s) with judgments, \
             {judgment_count} judgment(s), {rubric_count} rubric(s)"
        ),
    })
}

async fn session_entry(
    location: &StoryCoords,
    run: &RunKey,
    rows: &[JudgeRow],
) -> JudgmentSessionSummary {
    let (verdict_pass, verdict_partial, verdict_fail) = verdict_counts(rows.iter());
    let manual_count = rows
        .iter()
        .filter(|row| row.rationale.starts_with(MANUAL_RATIONALE_PREFIX))
        .count();
    let turn_judgments = rows
        .iter()
        .filter(|row| row.call_id != STORY_CALL_ID)
        .count();
    JudgmentSessionSummary {
        storage: location.storage.clone(),
        agent_id: location.agent_id.clone(),
        session_id: location.session_id.clone(),
        root_session_id: location.root_session_id.clone(),
        judgment_count: rows.len(),
        turn_judgments,
        story_judgments: rows.len().saturating_sub(turn_judgments),
        rubric_ids: rubric_ids(rows),
        avg_score: average_score(rows.iter()),
        verdict_pass,
        verdict_partial,
        verdict_fail,
        manual_count,
        judgments_path: judgment_dataset_path(&run_coords(run))
            .await
            .unwrap_or_default(),
        status: if rows.is_empty() { "empty" } else { "ok" }.into(),
    }
}

fn rubric_summaries(rows: &[JudgeRow]) -> Vec<JudgmentRubricSummary> {
    rubric_ids(rows)
        .into_iter()
        .map(|rubric_id| {
            let scoped: Vec<_> = rows
                .iter()
                .filter(|row| row.rubric_id == rubric_id)
                .collect();
            let (verdict_pass, verdict_partial, verdict_fail) =
                verdict_counts(scoped.iter().copied());
            JudgmentRubricSummary {
                rubric_id,
                judgment_count: scoped.len(),
                avg_score: average_score(scoped.iter().copied()).unwrap_or(0.0),
                verdict_pass,
                verdict_partial,
                verdict_fail,
                manual_count: scoped
                    .iter()
                    .filter(|row| row.rationale.starts_with(MANUAL_RATIONALE_PREFIX))
                    .count(),
            }
        })
        .collect()
}

fn rubric_ids(rows: &[JudgeRow]) -> Vec<String> {
    let mut ids: Vec<_> = rows
        .iter()
        .map(|row| row.rubric_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    ids.sort();
    ids
}

fn verdict_counts<'a>(rows: impl Iterator<Item = &'a JudgeRow>) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for row in rows {
        match row.verdict.as_str() {
            "pass" => counts.0 += 1,
            "partial" => counts.1 += 1,
            "fail" => counts.2 += 1,
            _ => {}
        }
    }
    counts
}

fn average_score<'a>(rows: impl Iterator<Item = &'a JudgeRow>) -> Option<f64> {
    let scores: Vec<_> = rows.map(|row| row.score).collect();
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<i64>() as f64 / scores.len() as f64)
    }
}
