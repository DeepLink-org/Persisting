//! Interactive manual judge: render markdown, prompt per rubric / per turn.

use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::terminal_markdown::{format_turn_markdown, print_section};
use persisting_capture::engine::TurnKind;
use persisting_capture::engine::{rebuild_session_story, Story};
use persisting_capture::record::CaptureRecord;
use persisting_engine::trajectory::layers::MANUAL_RATIONALE_PREFIX;
use persisting_proto::{JudgeSampleMode, JudgeScope, JudgeScoreInput};

/// Pick up to `limit` sessions from a scan list (`limit == 0` → keep all).
pub fn sample_locations<T: Clone>(
    mut locations: Vec<T>,
    mode: JudgeSampleMode,
    limit: usize,
) -> Vec<T> {
    if locations.is_empty() {
        return locations;
    }
    if mode == JudgeSampleMode::Random {
        let n = locations.len();
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        let mut indices: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let j = (seed as usize) % (i + 1);
            indices.swap(i, j);
        }
        locations = indices.into_iter().map(|i| locations[i].clone()).collect();
    }
    if limit > 0 {
        locations.truncate(limit);
    }
    locations
}

pub fn story_from_replay_json(
    records_json: &[String],
    session_id: &str,
    root: &str,
) -> Result<Story> {
    let records: Vec<CaptureRecord> = records_json
        .iter()
        .enumerate()
        .map(|(i, json)| {
            serde_json::from_str(json)
                .with_context(|| format!("decode replay record[{i}] for manual judge"))
        })
        .collect::<Result<_>>()?;
    Ok(rebuild_session_story(session_id, root, &records))
}

pub fn print_trajectory_markdown(markdown: &str, title: &str) {
    print_section(title, markdown);
}

pub fn format_story_for_display(story: &Story) -> String {
    let mut out = String::new();
    for (i, t) in story
        .turns
        .iter()
        .filter(|t| t.kind == TurnKind::Dialogue)
        .enumerate()
    {
        if let (Some(u), Some(a)) = (&t.user, &t.assistant) {
            let call_id = u
                .call_id
                .as_ref()
                .or(a.call_id.as_ref())
                .map(|c| c.as_str())
                .unwrap_or("?");
            out.push_str(&format!(
                "### Turn {} (`{call_id}`)\n\n**User**\n\n{}\n\n---\n\n**Assistant**\n\n{}\n\n",
                i + 1,
                u.text,
                a.text
            ));
        }
    }
    out
}

pub fn collect_manual_scores(
    story: &Story,
    scope: JudgeScope,
    rubrics: &[String],
) -> Result<Vec<JudgeScoreInput>> {
    match scope {
        JudgeScope::Story => collect_story_scores(story, rubrics),
        JudgeScope::Turn => collect_turn_scores(story, rubrics),
    }
}

/// Non-interactive scores: same value for every rubric (story) or turn×rubric (turn scope).
pub fn fixed_manual_scores(
    story: &Story,
    scope: JudgeScope,
    rubrics: &[String],
    score: i64,
    verdict: Option<&str>,
    rationale: Option<&str>,
) -> Result<Vec<JudgeScoreInput>> {
    if !(0..=100).contains(&score) {
        anyhow::bail!("--score must be an integer 0-100 (got {score})");
    }
    let verdict = verdict
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| verdict_from_score(score));
    let rationale = rationale
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| {
            if r.starts_with(MANUAL_RATIONALE_PREFIX) {
                r.to_string()
            } else {
                format!("{MANUAL_RATIONALE_PREFIX}{r}")
            }
        })
        .unwrap_or_else(|| format!("{MANUAL_RATIONALE_PREFIX}batch score {score}"));

    match scope {
        JudgeScope::Story => {
            if format_story_for_display(story).trim().is_empty() {
                anyhow::bail!("no dialogue turns to score");
            }
            Ok(rubrics
                .iter()
                .map(|rubric| JudgeScoreInput {
                    call_id: None,
                    rubric_id: rubric.clone(),
                    score,
                    verdict: verdict.clone(),
                    rationale: rationale.clone(),
                })
                .collect())
        }
        JudgeScope::Turn => {
            let turns = dialogue_turn_call_ids(story);
            if turns.is_empty() {
                anyhow::bail!("no dialogue turns to score");
            }
            let mut out = Vec::with_capacity(turns.len() * rubrics.len());
            for call_id in turns {
                for rubric in rubrics {
                    out.push(JudgeScoreInput {
                        call_id: Some(call_id.clone()),
                        rubric_id: rubric.clone(),
                        score,
                        verdict: verdict.clone(),
                        rationale: rationale.clone(),
                    });
                }
            }
            Ok(out)
        }
    }
}

fn dialogue_turn_call_ids(story: &Story) -> Vec<String> {
    story
        .turns
        .iter()
        .filter(|t| t.kind == TurnKind::Dialogue)
        .filter_map(|t| {
            let user = t.user.as_ref()?;
            let assistant = t.assistant.as_ref()?;
            Some(
                user.call_id
                    .as_ref()
                    .or(assistant.call_id.as_ref())?
                    .as_str()
                    .to_string(),
            )
        })
        .collect()
}

fn collect_story_scores(story: &Story, rubrics: &[String]) -> Result<Vec<JudgeScoreInput>> {
    let display = format_story_for_display(story);
    if display.trim().is_empty() {
        anyhow::bail!("no dialogue turns to score");
    }
    print_trajectory_markdown(&display, "Score the full trajectory (story scope)");
    let mut out = Vec::with_capacity(rubrics.len());
    for rubric in rubrics {
        let (score, verdict, rationale) = prompt_score(rubric, "full story")?;
        out.push(JudgeScoreInput {
            call_id: None,
            rubric_id: rubric.clone(),
            score,
            verdict,
            rationale,
        });
    }
    Ok(out)
}

fn collect_turn_scores(story: &Story, rubrics: &[String]) -> Result<Vec<JudgeScoreInput>> {
    let turns: Vec<_> = story
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
            Some((call_id, user.text.clone(), assistant.text.clone()))
        })
        .collect();

    if turns.is_empty() {
        anyhow::bail!("no dialogue turns to score");
    }

    let mut out = Vec::new();
    let turn_total = turns.len();
    for (idx, (call_id, user, assistant)) in turns.iter().enumerate() {
        let md = format_turn_markdown(idx + 1, turn_total, call_id, user, assistant);
        print_section(
            &format!("Turn {}/{} · call_id={call_id}", idx + 1, turn_total),
            &md,
        );

        for rubric in rubrics {
            let (score, verdict, rationale) =
                prompt_score(rubric, &format!("turn {} ({call_id})", idx + 1))?;
            out.push(JudgeScoreInput {
                call_id: Some(call_id.clone()),
                rubric_id: rubric.clone(),
                score,
                verdict,
                rationale,
            });
        }
    }
    Ok(out)
}

fn prompt_score(rubric: &str, context: &str) -> Result<(i64, String, String)> {
    let score = loop {
        eprint!("[{rubric}] score 0-100 for {context}: ");
        io::stderr().flush().ok();
        let line = read_line()?;
        match line.trim().parse::<i64>() {
            Ok(v) if (0..=100).contains(&v) => break v,
            _ => eprintln!("  enter an integer 0-100"),
        }
    };

    eprint!("[{rubric}] verdict (pass/partial/fail) [auto]: ");
    io::stderr().flush().ok();
    let verdict_raw = read_line()?;
    let verdict = if verdict_raw.trim().is_empty() {
        verdict_from_score(score)
    } else {
        verdict_raw.trim().to_string()
    };

    eprint!("[{rubric}] rationale (optional): ");
    io::stderr().flush().ok();
    let rationale_raw = read_line()?;
    let rationale = if rationale_raw.trim().is_empty() {
        format!("{MANUAL_RATIONALE_PREFIX}human score {score}")
    } else if rationale_raw.starts_with(MANUAL_RATIONALE_PREFIX) {
        rationale_raw
    } else {
        format!("{MANUAL_RATIONALE_PREFIX}{rationale_raw}")
    };

    Ok((score, verdict, rationale))
}

fn verdict_from_score(score: i64) -> String {
    if score >= 80 {
        "pass".into()
    } else if score >= 50 {
        "partial".into()
    } else {
        "fail".into()
    }
}

fn read_line() -> Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .context("read stdin for manual judge")?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_turn_markdown_includes_call_id() {
        let md = format_turn_markdown(1, 3, "c1", "hello", "world");
        assert!(md.contains("Turn 1/3"));
        assert!(md.contains("`c1`"));
        assert!(md.contains("hello"));
        assert!(md.contains("world"));
    }

    #[test]
    fn sample_truncates_sequential() {
        let locs = vec!["a", "b", "c", "d"];
        let got = sample_locations(locs, JudgeSampleMode::Sequential, 2);
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn sample_limit_zero_keeps_all() {
        let locs = vec!["a", "b", "c"];
        let got = sample_locations(locs, JudgeSampleMode::Sequential, 0);
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn fixed_manual_scores_story_scope() {
        use persisting_capture::config::CaptureLevel;
        use persisting_capture::engine::Call;
        use persisting_capture::sink::{llm_request_summary_record, llm_response_record_with_content};

        let call = Call {
            call_id: "c1".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("s".into()),
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
            Some("s".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("ok".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let records = [
            serde_json::to_string(&req).unwrap(),
            serde_json::to_string(&resp).unwrap(),
        ];
        let story = story_from_replay_json(&records, "s", "s").unwrap();
        let scores =
            fixed_manual_scores(&story, JudgeScope::Story, &["default".into()], 100, None, None)
                .unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].score, 100);
        assert_eq!(scores[0].verdict, "pass");
        assert!(scores[0].call_id.is_none());
    }

    #[test]
    fn fixed_manual_scores_rejects_out_of_range() {
        use persisting_capture::config::CaptureLevel;
        use persisting_capture::engine::Call;
        use persisting_capture::sink::{llm_request_summary_record, llm_response_record_with_content};

        let call = Call {
            call_id: "c1".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let req = llm_request_summary_record(
            Some("s".into()),
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
            Some("s".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("ok".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let records = [
            serde_json::to_string(&req).unwrap(),
            serde_json::to_string(&resp).unwrap(),
        ];
        let story = story_from_replay_json(&records, "s", "s").unwrap();
        assert!(fixed_manual_scores(&story, JudgeScope::Story, &["default".into()], 101, None, None).is_err());
    }
}
