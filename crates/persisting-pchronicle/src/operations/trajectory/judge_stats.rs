//! Typed adapter for pChronicle-owned judgment aggregation.

use crate::{
    aggregate_judgments, session_judgment_summary, JudgmentRubricSummary, JudgmentSessionSummary,
    StoryCoords,
};
use crate::{
    JudgeRubricSummary, JudgeStatsSession, SessionJudgeStats, TrajectoryJudgeStatsRequest,
    TrajectoryJudgeStatsResponse,
};
use anyhow::Result;

fn session_to_proto(summary: JudgmentSessionSummary) -> JudgeStatsSession {
    JudgeStatsSession {
        storage: summary.storage,
        agent_id: summary.agent_id,
        session_id: summary.session_id,
        root_session_id: summary.root_session_id,
        judgment_count: summary.judgment_count,
        turn_judgments: summary.turn_judgments,
        story_judgments: summary.story_judgments,
        rubric_ids: summary.rubric_ids,
        avg_score: summary.avg_score,
        verdict_pass: summary.verdict_pass,
        verdict_partial: summary.verdict_partial,
        verdict_fail: summary.verdict_fail,
        manual_count: summary.manual_count,
        judgments_path: summary.judgments_path,
        status: summary.status,
    }
}

fn rubric_to_proto(summary: JudgmentRubricSummary) -> JudgeRubricSummary {
    JudgeRubricSummary {
        rubric_id: summary.rubric_id,
        judgment_count: summary.judgment_count,
        avg_score: summary.avg_score,
        verdict_pass: summary.verdict_pass,
        verdict_partial: summary.verdict_partial,
        verdict_fail: summary.verdict_fail,
        manual_count: summary.manual_count,
    }
}

pub async fn judge_stats_async(
    request: TrajectoryJudgeStatsRequest,
) -> Result<TrajectoryJudgeStatsResponse> {
    let summary = aggregate_judgments(
        request.storage,
        request.agent_id,
        request.session_id,
        request.root_session_id,
    )
    .await?;
    Ok(TrajectoryJudgeStatsResponse {
        storage: summary.storage,
        session_count: summary.session_count,
        judged_session_count: summary.judged_session_count,
        judgment_count: summary.judgment_count,
        rubric_count: summary.rubric_count,
        sessions: summary.sessions.into_iter().map(session_to_proto).collect(),
        rubrics: summary.rubrics.into_iter().map(rubric_to_proto).collect(),
        status: summary.status,
        note: summary.note,
    })
}

pub async fn session_judge_stats(location: &StoryCoords) -> SessionJudgeStats {
    session_to_proto(session_judgment_summary(location).await).into()
}
