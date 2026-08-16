//! Protocol-independent trajectory judgment orchestration.

use anyhow::{Context, Result};

use crate::{
    build_llm_judge_prompt, dry_run_judge_rows, evaluation_units, judgment_dataset_path,
    manual_few_shot_examples, manual_judge_rows, parse_llm_judge_rows, pending_evaluation_units,
    read_judge_rows, write_judge_rows, JudgeRow, JudgmentScope, ManualJudgmentInput,
    RawEventLanceStore, StoryCoords,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgingMethod {
    Manual,
    Llm,
}

#[derive(Debug, Clone)]
pub struct JudgeTrajectoryRequest {
    pub session: StoryCoords,
    pub rubric_id: String,
    pub rubric_ids: Vec<String>,
    pub scope: JudgmentScope,
    pub method: JudgingMethod,
    pub force: bool,
    pub dry_run: bool,
    pub model: Option<String>,
    pub few_shot_limit: usize,
    pub manual_scores: Vec<ManualJudgmentInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeTrajectoryOutcome {
    pub primary_rubric: String,
    pub rubric_ids: Vec<String>,
    pub dataset: String,
    pub judged_units: usize,
    pub skipped_units: usize,
    pub status: String,
    pub note: String,
}

pub async fn judge_trajectory(request: JudgeTrajectoryRequest) -> Result<JudgeTrajectoryOutcome> {
    let rubric_ids = resolve_rubric_ids(&request.rubric_id, &request.rubric_ids);
    let primary_rubric = rubric_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "default".into());

    let store = RawEventLanceStore;
    if !store.exists(&request.session).await? {
        anyhow::bail!(
            "Lance event log missing for session {}; judge requires events.lance",
            request.session.session_id
        );
    }
    let records = store.read_events(&request.session, 0, None).await?;
    let units = evaluation_units(&records, request.scope)?;
    if units.is_empty() {
        anyhow::bail!(
            "no judge units for session {} ({:?})",
            request.session.session_id,
            request.scope
        );
    }

    let existing = read_judge_rows(&request.session).await?;
    let mut judged_units = 0;
    let mut skipped_units = 0;
    let mut incoming = Vec::new();

    for rubric_id in &rubric_ids {
        let (pending, skipped) = pending_evaluation_units(
            &existing,
            &request.session.session_id,
            rubric_id,
            &units,
            request.force,
        );
        skipped_units += skipped;
        if pending.is_empty() {
            continue;
        }

        let rows = match request.method {
            JudgingMethod::Manual => manual_judge_rows(
                &request.session.session_id,
                rubric_id,
                &pending,
                &request.manual_scores,
            )?,
            JudgingMethod::Llm if request.dry_run => {
                dry_run_judge_rows(&request.session.session_id, rubric_id, &pending)
            }
            JudgingMethod::Llm => {
                let examples =
                    manual_few_shot_examples(&existing, rubric_id, request.few_shot_limit);
                llm_judge_rows(
                    &request.session,
                    request.scope,
                    rubric_id,
                    request.model.as_deref(),
                    &pending,
                    &examples,
                )
                .await?
            }
        };
        judged_units += pending.len();
        incoming.extend(rows);
    }

    let dataset = if incoming.is_empty() {
        judgment_dataset_path(&request.session).await?
    } else {
        write_judge_rows(&request.session, &incoming).await?
    };
    Ok(JudgeTrajectoryOutcome {
        primary_rubric,
        rubric_ids: rubric_ids.clone(),
        dataset: dataset.clone(),
        judged_units,
        skipped_units,
        status: "ok".into(),
        note: format!(
            "Judge {:?}/{:?}: {} rubric(s), {} unit(s) scored, {} skipped. Judgments stored in {}.",
            request.method,
            request.scope,
            rubric_ids.len(),
            judged_units,
            skipped_units,
            dataset
        ),
    })
}

fn resolve_rubric_ids(single: &str, multiple: &[String]) -> Vec<String> {
    if !multiple.is_empty() {
        return multiple
            .iter()
            .map(|rubric| {
                if rubric.trim().is_empty() {
                    "default".into()
                } else {
                    rubric.trim().into()
                }
            })
            .collect();
    }
    vec![if single.trim().is_empty() {
        "default".into()
    } else {
        single.into()
    }]
}

async fn llm_judge_rows(
    session: &StoryCoords,
    scope: JudgmentScope,
    rubric_id: &str,
    model: Option<&str>,
    units: &[crate::EvaluationUnit],
    few_shot: &[JudgeRow],
) -> Result<Vec<JudgeRow>> {
    let model = model
        .map(str::to_string)
        .or_else(|| std::env::var("PERSISTING_JUDGE_MODEL").ok())
        .unwrap_or_else(|| "gpt-4o-mini".into());
    let prompt = build_llm_judge_prompt(scope, rubric_id, units, few_shot);
    let output = call_openai_chat(&model, &prompt).await?;
    parse_llm_judge_rows(&session.session_id, rubric_id, &output)
}

async fn call_openai_chat(model: &str, user_prompt: &str) -> Result<String> {
    let base = std::env::var("OPENAI_BASE_URL")
        .or_else(|_| std::env::var("PERSISTING_JUDGE_BASE_URL"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("PERSISTING_JUDGE_API_KEY"))
        .context("OPENAI_API_KEY (or PERSISTING_JUDGE_API_KEY) required for judge")?;
    let response = reqwest::Client::builder()
        .build()
        .context("build reqwest client for judge")?
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                {"role": "system", "content": "You output strict JSON only."},
                {"role": "user", "content": user_prompt}
            ]
        }))
        .send()
        .await
        .context("judge LLM HTTP request")?;
    let status = response.status();
    let text = response.text().await.context("read judge LLM response")?;
    if !status.is_success() {
        anyhow::bail!("judge LLM HTTP {status}: {text}");
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).context("parse judge LLM envelope")?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("judge LLM response missing message content"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventRecord, RawEventLanceStore};

    fn event(kind: &str, content_key: &str, content: &str) -> EventRecord {
        EventRecord {
            identity: crate::EventIdentity::default(),
            seq: 0,
            source: "test".into(),
            kind: kind.into(),
            timestamp: None,
            session_id: Some("session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({(content_key): content}),
        }
    }

    #[tokio::test]
    async fn manual_judge_isolated_from_append_only_events() {
        let dir = tempfile::tempdir().unwrap();
        let session = StoryCoords::new(dir.path().to_string_lossy(), "agent", "session", None);
        RawEventLanceStore
            .append_events(
                &session,
                &[
                    event("llm.request", "user_content", "hello"),
                    event("llm.response", "assistant_content", "world"),
                ],
            )
            .await
            .unwrap();

        let outcome = judge_trajectory(JudgeTrajectoryRequest {
            session: session.clone(),
            rubric_id: "quality".into(),
            rubric_ids: Vec::new(),
            scope: JudgmentScope::Story,
            method: JudgingMethod::Manual,
            force: false,
            dry_run: false,
            model: None,
            few_shot_limit: 0,
            manual_scores: vec![ManualJudgmentInput {
                call_id: Some(crate::STORY_CALL_ID.into()),
                rubric_id: "quality".into(),
                score: 90,
                verdict: "pass".into(),
                rationale: "good".into(),
            }],
        })
        .await
        .unwrap();

        assert_eq!(outcome.judged_units, 1);
        let rows = crate::read_judge_rows(&session).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].score, 90);
        assert!(rows[0]
            .rationale
            .starts_with(crate::MANUAL_RATIONALE_PREFIX));

        let event_layout = RawEventLanceStore.layout_stats(&session).await.unwrap();
        assert_eq!(event_layout.visible_rows, 2);
        assert!(crate::raw_event_arrow_schema()
            .fields
            .iter()
            .all(|field| !field.name().starts_with("judge_")));
        assert!(crate::judgment_dataset_path(&session)
            .await
            .unwrap()
            .ends_with("judgments.lance"));

        RawEventLanceStore
            .append_events(&session, &[event("note", "content", "after judgment")])
            .await
            .unwrap();
        RawEventLanceStore
            .append_events(
                &session,
                &[
                    event("llm.request", "user_content", "hello again"),
                    event("llm.response", "assistant_content", "world again"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(crate::read_judge_rows(&session).await.unwrap().len(), 1);
    }
}
