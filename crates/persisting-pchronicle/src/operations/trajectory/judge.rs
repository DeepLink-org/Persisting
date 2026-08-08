//! Typed adapter for pChronicle-owned trajectory judgment.

use crate::{
    judge_trajectory, JudgeTrajectoryRequest, JudgingMethod, JudgmentScope, ManualJudgmentInput,
};
use crate::{JudgeMethod, JudgeScope, TrajectoryJudgeRequest, TrajectoryJudgeResponse};
use anyhow::Result;

pub async fn judge_async(request: TrajectoryJudgeRequest) -> Result<TrajectoryJudgeResponse> {
    let session = super::session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        request.root_session_id.as_deref(),
    );
    let scope = match request.scope {
        JudgeScope::Story => JudgmentScope::Story,
        JudgeScope::Turn => JudgmentScope::Turn,
    };
    let method = match request.method {
        JudgeMethod::Manual => JudgingMethod::Manual,
        JudgeMethod::Llm => JudgingMethod::Llm,
    };
    let outcome = judge_trajectory(JudgeTrajectoryRequest {
        session,
        rubric_id: request.rubric_id,
        rubric_ids: request.rubric_ids,
        scope,
        method,
        force: request.force,
        dry_run: request.dry_run,
        model: request.model,
        few_shot_limit: request.few_shot_limit,
        manual_scores: request
            .manual_scores
            .into_iter()
            .map(|score| ManualJudgmentInput {
                call_id: score.call_id,
                rubric_id: score.rubric_id,
                score: score.score,
                verdict: score.verdict,
                rationale: score.rationale,
            })
            .collect(),
    })
    .await?;

    Ok(TrajectoryJudgeResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        rubric_id: outcome.primary_rubric,
        rubric_ids: outcome.rubric_ids,
        scope: request.scope,
        method: request.method,
        judgments_path: outcome.dataset,
        judged_calls: outcome.judged_units,
        skipped_calls: outcome.skipped_units,
        status: outcome.status,
        note: outcome.note,
    })
}
