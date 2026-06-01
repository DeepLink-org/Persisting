//! Judge egress: read canonical Vortex, write `{run}/layers/judge_*.vortex` sidecars.
//!
//! Modes (scope × method):
//! - **story + llm**: one LLM call scores the full trajectory per rubric dimension
//! - **story + manual**: CLI renders markdown, human scores each rubric once
//! - **turn + llm**: LLM scores each dialogue turn (RL-style reward model)
//! - **turn + manual**: CLI walks turns, human scores each rubric per turn

use anyhow::{Context, Result};
use persisting_capture::engine::{rebuild_session_story, Story, TurnKind};
use persisting_capture::record::CaptureRecord;
use persisting_proto::{
    JudgeMethod, JudgeScope, JudgeScoreInput, TrajectoryJudgeRequest, TrajectoryJudgeResponse,
};
use serde::Deserialize;

use super::layers::{
    ensure_layers_dir, has_judgment, layer_file_name, load_manifest, merge_judge_rows,
    read_judge_rows, register_layer, save_manifest, sidecar_path, write_judge_rows, JudgeRow,
    MANUAL_RATIONALE_PREFIX, STORY_CALL_ID,
};
use super::store::{TrajectoryStore, VortexTrajectoryStore};
use super::TrajectorySession;

#[derive(Debug, Deserialize)]
struct LlmJudgment {
    call_id: String,
    #[serde(default)]
    rubric_id: Option<String>,
    score: i64,
    verdict: String,
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct LlmJudgeBatch {
    judgments: Vec<LlmJudgment>,
}

#[derive(Debug, Clone)]
struct JudgeUnit {
    call_id: String,
    body: String,
}

pub async fn judge_async(request: TrajectoryJudgeRequest) -> Result<TrajectoryJudgeResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = super::session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let rubric_ids = resolve_rubric_ids(&request);
    let primary_rubric = rubric_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "default".into());

    let vortex = VortexTrajectoryStore;
    if !vortex.exists(&session).await? {
        anyhow::bail!(
            "Vortex event log missing for session {}; judge requires events.vortex",
            session.session_id
        );
    }

    let replay = vortex.replay(&session, 0, None).await?;
    let records: Vec<CaptureRecord> = replay
        .records
        .iter()
        .enumerate()
        .map(|(i, json)| {
            serde_json::from_str(json)
                .with_context(|| format!("decode replay record[{i}] for judge"))
        })
        .collect::<Result<_>>()?;

    let root = session
        .root_session_id
        .as_deref()
        .unwrap_or(session.session_id.as_str());
    let story = rebuild_session_story(&session.session_id, root, &records);
    let units = units_for_scope(&story, request.scope);
    if units.is_empty() {
        anyhow::bail!(
            "no judge units for session {} ({:?})",
            session.session_id,
            request.scope
        );
    }

    ensure_layers_dir(&session).await?;

    let mut total_judged = 0usize;
    let mut total_skipped = 0usize;
    let mut last_sidecar = String::new();
    let mut last_layer = String::new();

    for rubric_id in &rubric_ids {
        let sidecar = sidecar_path(&session, &layer_file_name(rubric_id))?;
        let existing = read_judge_rows(&sidecar).await?;

        let (to_judge, skipped) = pending_units(
            &existing,
            &session.session_id,
            rubric_id,
            &units,
            request.force,
        );
        total_skipped += skipped;

        if to_judge.is_empty() {
            continue;
        }

        let incoming = match request.method {
            JudgeMethod::Manual => manual_rows(
                &session.session_id,
                rubric_id,
                &to_judge,
                &request.manual_scores,
            )?,
            JudgeMethod::Llm if request.dry_run => {
                dry_run_rows(&session.session_id, rubric_id, &to_judge)
            }
            JudgeMethod::Llm => {
                let few_shot = few_shot_examples(&existing, rubric_id, request.few_shot_limit);
                llm_judge_rows(
                    &session,
                    request.scope,
                    rubric_id,
                    request.model.as_deref(),
                    &to_judge,
                    &few_shot,
                )
                .await?
            }
        };

        total_judged += to_judge.len();
        let merged = merge_judge_rows(existing, incoming, &session.session_id, rubric_id);
        write_judge_rows(&sidecar, &merged).await?;
        last_sidecar = sidecar.display().to_string();

        let mut manifest = load_manifest(&session)?;
        last_layer = register_layer(&mut manifest, rubric_id);
        save_manifest(&session, &manifest)?;
    }

    Ok(TrajectoryJudgeResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        rubric_id: primary_rubric,
        rubric_ids: rubric_ids.clone(),
        scope: request.scope,
        method: request.method,
        layer_name: last_layer,
        sidecar_path: last_sidecar,
        judged_calls: total_judged,
        skipped_calls: total_skipped,
        status: "ok".to_string(),
        note: format!(
            "Judge {:?}/{:?}: {} rubric(s), {} unit(s) scored, {} skipped. Sidecar updated.",
            request.method,
            request.scope,
            rubric_ids.len(),
            total_judged,
            total_skipped
        ),
    })
}

fn resolve_rubric_ids(request: &TrajectoryJudgeRequest) -> Vec<String> {
    if !request.rubric_ids.is_empty() {
        return request
            .rubric_ids
            .iter()
            .map(|r| {
                if r.trim().is_empty() {
                    "default".to_string()
                } else {
                    r.trim().to_string()
                }
            })
            .collect();
    }
    let single = if request.rubric_id.trim().is_empty() {
        "default".to_string()
    } else {
        request.rubric_id.clone()
    };
    vec![single]
}

fn units_for_scope(story: &Story, scope: JudgeScope) -> Vec<JudgeUnit> {
    match scope {
        JudgeScope::Story => {
            let body = story_body(story);
            if body.trim().is_empty() {
                Vec::new()
            } else {
                vec![JudgeUnit {
                    call_id: STORY_CALL_ID.to_string(),
                    body,
                }]
            }
        }
        JudgeScope::Turn => story
            .turns
            .iter()
            .filter(|t| t.kind == TurnKind::Dialogue)
            .filter_map(|t| {
                let user = t.user.as_ref()?;
                let assistant = t.assistant.as_ref()?;
                let call_id = user
                    .call_id
                    .as_ref()
                    .or(assistant.call_id.as_ref())?
                    .as_str()
                    .to_string();
                Some(JudgeUnit {
                    call_id,
                    body: format!("User:\n{}\n\nAssistant:\n{}", user.text, assistant.text),
                })
            })
            .collect(),
    }
}

fn story_body(story: &Story) -> String {
    let mut out = String::new();
    for (i, t) in story
        .turns
        .iter()
        .filter(|t| t.kind == TurnKind::Dialogue)
        .enumerate()
    {
        if let (Some(u), Some(a)) = (&t.user, &t.assistant) {
            out.push_str(&format!(
                "### Turn {} (call_id={})\nUser:\n{}\n\nAssistant:\n{}\n\n",
                i + 1,
                u.call_id
                    .as_ref()
                    .or(a.call_id.as_ref())
                    .map(|c| c.as_str())
                    .unwrap_or("?"),
                u.text,
                a.text
            ));
        }
    }
    out
}

fn pending_units(
    existing: &[JudgeRow],
    session_id: &str,
    rubric_id: &str,
    units: &[JudgeUnit],
    force: bool,
) -> (Vec<JudgeUnit>, usize) {
    if force {
        return (units.to_vec(), 0);
    }
    let pending: Vec<_> = units
        .iter()
        .filter(|u| !has_judgment(existing, session_id, &u.call_id, rubric_id))
        .cloned()
        .collect();
    let skipped = units.len().saturating_sub(pending.len());
    (pending, skipped)
}

fn manual_rows(
    session_id: &str,
    rubric_id: &str,
    units: &[JudgeUnit],
    inputs: &[JudgeScoreInput],
) -> Result<Vec<JudgeRow>> {
    if inputs.is_empty() {
        anyhow::bail!("manual judge requires manual_scores (collect via CLI interactive mode)");
    }
    let mut rows = Vec::new();
    for unit in units {
        let matches: Vec<_> = inputs
            .iter()
            .filter(|s| s.rubric_id == rubric_id && score_call_id(s) == unit.call_id)
            .collect();
        if matches.is_empty() {
            anyhow::bail!(
                "missing manual score for call_id={} rubric={}",
                unit.call_id,
                rubric_id
            );
        }
        for s in matches {
            rows.push(score_input_to_row(session_id, s, &unit.call_id));
        }
    }
    Ok(rows)
}

fn score_call_id(s: &JudgeScoreInput) -> String {
    s.call_id
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| STORY_CALL_ID.to_string())
}

fn score_input_to_row(session_id: &str, s: &JudgeScoreInput, call_id: &str) -> JudgeRow {
    let rationale = if s.rationale.starts_with(MANUAL_RATIONALE_PREFIX) {
        s.rationale.clone()
    } else {
        format!("{MANUAL_RATIONALE_PREFIX}{}", s.rationale)
    };
    JudgeRow {
        session_id: session_id.to_string(),
        call_id: call_id.to_string(),
        rubric_id: s.rubric_id.clone(),
        score: s.score.clamp(0, 100),
        verdict: normalize_verdict(&s.verdict),
        rationale,
    }
}

fn dry_run_rows(session_id: &str, rubric_id: &str, units: &[JudgeUnit]) -> Vec<JudgeRow> {
    units
        .iter()
        .map(|u| JudgeRow {
            session_id: session_id.to_string(),
            call_id: u.call_id.clone(),
            rubric_id: rubric_id.to_string(),
            score: 100,
            verdict: "pass".to_string(),
            rationale: "dry-run (no LLM call)".to_string(),
        })
        .collect()
}

fn few_shot_examples(existing: &[JudgeRow], rubric_id: &str, limit: usize) -> Vec<JudgeRow> {
    if limit == 0 {
        return Vec::new();
    }
    existing
        .iter()
        .filter(|r| r.rubric_id == rubric_id && r.rationale.starts_with(MANUAL_RATIONALE_PREFIX))
        .take(limit)
        .cloned()
        .collect()
}

async fn llm_judge_rows(
    session: &TrajectorySession,
    scope: JudgeScope,
    rubric_id: &str,
    model: Option<&str>,
    units: &[JudgeUnit],
    few_shot: &[JudgeRow],
) -> Result<Vec<JudgeRow>> {
    if units.is_empty() {
        return Ok(Vec::new());
    }
    let model = model
        .map(str::to_string)
        .or_else(|| std::env::var("PERSISTING_JUDGE_MODEL").ok())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    let prompt = build_llm_prompt(scope, rubric_id, units, few_shot);
    let raw = call_openai_chat(&model, &prompt).await?;
    let parsed: LlmJudgeBatch = serde_json::from_str(extract_json_payload(&raw))
        .with_context(|| format!("parse judge JSON from model output: {raw}"))?;

    let mut rows = Vec::with_capacity(parsed.judgments.len());
    for j in parsed.judgments {
        let rid = j.rubric_id.as_deref().unwrap_or(rubric_id);
        if rid != rubric_id {
            continue;
        }
        rows.push(JudgeRow {
            session_id: session.session_id.clone(),
            call_id: j.call_id,
            rubric_id: rubric_id.to_string(),
            score: j.score.clamp(0, 100),
            verdict: normalize_verdict(&j.verdict),
            rationale: j.rationale,
        });
    }
    Ok(rows)
}

fn build_llm_prompt(
    scope: JudgeScope,
    rubric_id: &str,
    units: &[JudgeUnit],
    few_shot: &[JudgeRow],
) -> String {
    let mut examples = String::new();
    if !few_shot.is_empty() {
        examples.push_str("Reference examples (human scores):\n");
        for ex in few_shot {
            examples.push_str(&format!(
                "- call_id={} score={} verdict={} rationale={}\n",
                ex.call_id, ex.score, ex.verdict, ex.rationale
            ));
        }
        examples.push('\n');
    }

    let trajectory = units
        .iter()
        .enumerate()
        .map(|(i, u)| {
            if scope == JudgeScope::Story {
                format!("### Full trajectory\n{}\n", u.body)
            } else {
                format!("### Turn {} (call_id={})\n{}\n", i + 1, u.call_id, u.body)
            }
        })
        .collect::<String>();

    let task = match scope {
        JudgeScope::Story => format!(
            "Score the ENTIRE trajectory once (call_id=\"{STORY_CALL_ID}\") on rubric `{rubric_id}`."
        ),
        JudgeScope::Turn => format!(
            "Score EACH dialogue turn separately on rubric `{rubric_id}`."
        ),
    };

    format!(
        r#"You are an evaluator (LLM-as-judge) for agent trajectories.
{task}
Score 0-100. Verdict: pass, partial, or fail.
Return ONLY valid JSON (no markdown fences):
{{"judgments":[{{"call_id":"...","rubric_id":"{rubric_id}","score":85,"verdict":"pass","rationale":"..."}}]}}

{examples}Trajectory:
{trajectory}"#
    )
}

fn normalize_verdict(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pass" | "ok" | "success" => "pass".to_string(),
        "partial" | "mixed" => "partial".to_string(),
        "fail" | "failed" | "failure" => "fail".to_string(),
        other => other.to_string(),
    }
}

fn extract_json_payload(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    }
}

async fn call_openai_chat(model: &str, user_prompt: &str) -> Result<String> {
    let base = std::env::var("OPENAI_BASE_URL")
        .or_else(|_| std::env::var("PERSISTING_JUDGE_BASE_URL"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let api_key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("PERSISTING_JUDGE_API_KEY"))
        .context("OPENAI_API_KEY (or PERSISTING_JUDGE_API_KEY) required for judge")?;
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            {"role": "system", "content": "You output strict JSON only."},
            {"role": "user", "content": user_prompt}
        ]
    });

    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client for judge")?;
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .context("judge LLM HTTP request")?;
    let status = resp.status();
    let text = resp.text().await.context("read judge LLM response")?;
    if !status.is_success() {
        anyhow::bail!("judge LLM HTTP {status}: {text}");
    }
    let v: serde_json::Value = serde_json::from_str(&text).context("parse judge LLM envelope")?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("judge LLM response missing message content"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_capture::config::CaptureLevel;
    use persisting_capture::engine::Call;
    use persisting_capture::sink::{llm_request_summary_record, llm_response_record_with_content};
    use persisting_proto::{TrajectoryAppendRequest, TrajectoryStorageFormat};

    use crate::trajectory::append_async;

    fn sample_call(id: &str) -> Call {
        Call {
            call_id: id.into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn judge_dry_run_turn_llm_writes_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().into_owned();
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("run".into()),
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
            Some("run".into()),
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
            session_id: "run".into(),
            root_session_id: None,
            records_ronl: lines.join("\n") + "\n",
            storage_format: TrajectoryStorageFormat::Vortex,
        })
        .await
        .unwrap();

        let out = judge_async(TrajectoryJudgeRequest {
            storage,
            agent_id: "agent".into(),
            session_id: "run".into(),
            root_session_id: None,
            rubric_id: "default".into(),
            rubric_ids: vec![],
            scope: JudgeScope::Turn,
            method: JudgeMethod::Llm,
            model: None,
            dry_run: true,
            force: false,
            manual_scores: vec![],
            few_shot_limit: 0,
        })
        .await
        .unwrap();

        assert_eq!(out.judged_calls, 1);
        assert!(std::path::Path::new(&out.sidecar_path).is_file());
    }

    #[tokio::test]
    async fn judge_manual_story_accepts_scores() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().into_owned();
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("run".into()),
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
            Some("run".into()),
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
            session_id: "run".into(),
            root_session_id: None,
            records_ronl: lines.join("\n") + "\n",
            storage_format: TrajectoryStorageFormat::Vortex,
        })
        .await
        .unwrap();

        let out = judge_async(TrajectoryJudgeRequest {
            storage,
            agent_id: "agent".into(),
            session_id: "run".into(),
            root_session_id: None,
            rubric_id: "quality".into(),
            rubric_ids: vec![],
            scope: JudgeScope::Story,
            method: JudgeMethod::Manual,
            model: None,
            dry_run: false,
            force: false,
            manual_scores: vec![JudgeScoreInput {
                call_id: None,
                rubric_id: "quality".into(),
                score: 90,
                verdict: "pass".into(),
                rationale: "good overall".into(),
            }],
            few_shot_limit: 0,
        })
        .await
        .unwrap();

        assert_eq!(out.judged_calls, 1);
        let rows = read_judge_rows(std::path::Path::new(&out.sidecar_path))
            .await
            .unwrap();
        assert_eq!(rows[0].call_id, STORY_CALL_ID);
        assert!(rows[0].rationale.starts_with(MANUAL_RATIONALE_PREFIX));
    }

    #[tokio::test]
    async fn judge_rejudge_without_force_preserves_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_string_lossy().into_owned();
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("run".into()),
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
            Some("run".into()),
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
            session_id: "run".into(),
            root_session_id: None,
            records_ronl: lines.join("\n") + "\n",
            storage_format: TrajectoryStorageFormat::Vortex,
        })
        .await
        .unwrap();

        let first = judge_async(TrajectoryJudgeRequest {
            storage: storage.clone(),
            agent_id: "agent".into(),
            session_id: "run".into(),
            root_session_id: None,
            rubric_id: "default".into(),
            rubric_ids: vec![],
            scope: JudgeScope::Story,
            method: JudgeMethod::Manual,
            model: None,
            dry_run: false,
            force: false,
            manual_scores: vec![JudgeScoreInput {
                call_id: None,
                rubric_id: "default".into(),
                score: 88,
                verdict: "pass".into(),
                rationale: "first pass".into(),
            }],
            few_shot_limit: 0,
        })
        .await
        .unwrap();
        assert_eq!(first.judged_calls, 1);
        let sidecar = first.sidecar_path.clone();
        assert!(std::path::Path::new(&sidecar).is_file());

        let second = judge_async(TrajectoryJudgeRequest {
            storage,
            agent_id: "agent".into(),
            session_id: "run".into(),
            root_session_id: None,
            rubric_id: "default".into(),
            rubric_ids: vec![],
            scope: JudgeScope::Story,
            method: JudgeMethod::Manual,
            model: None,
            dry_run: false,
            force: false,
            manual_scores: vec![JudgeScoreInput {
                call_id: None,
                rubric_id: "default".into(),
                score: 99,
                verdict: "pass".into(),
                rationale: "ignored".into(),
            }],
            few_shot_limit: 0,
        })
        .await
        .unwrap();
        assert_eq!(second.judged_calls, 0);
        assert_eq!(second.skipped_calls, 1);
        assert!(std::path::Path::new(&sidecar).is_file());
        let rows = read_judge_rows(std::path::Path::new(&sidecar))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].score, 88);
    }
}
