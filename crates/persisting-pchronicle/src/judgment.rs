//! Normalized judge results stored at `{run}/judgments.lance`.
//!
//! The canonical event schema never evolves when a rubric is added.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use lance::Dataset;

use crate::{story_lance_judgment_path, StoryCoords};

pub const STORY_CALL_ID: &str = "__story__";
pub const MANUAL_RATIONALE_PREFIX: &str = "[manual] ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeRow {
    pub session_id: String,
    pub call_id: String,
    pub rubric_id: String,
    pub score: i64,
    pub verdict: String,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeDialogueUnit {
    pub call_id: String,
    pub user: String,
    pub assistant: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgmentScope {
    Story,
    Turn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationUnit {
    pub call_id: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualJudgmentInput {
    pub call_id: Option<String>,
    pub rubric_id: String,
    pub score: i64,
    pub verdict: String,
    pub rationale: String,
}

/// Build complete user/assistant units from canonical events using pChronicle's
/// dialogue projection policy.
pub fn dialogue_judge_units(records: &[crate::EventRecord]) -> Result<Vec<JudgeDialogueUnit>> {
    let (blocks, _) = crate::event_records_to_markdown_blocks(records)?;
    let mut units: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let mut by_call_id = HashMap::new();

    for block in blocks {
        let Some(role) = block.role() else {
            continue;
        };
        let Some(call_id) = block
            .header
            .fields
            .get("call_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        else {
            continue;
        };
        let index = *by_call_id.entry(call_id.clone()).or_insert_with(|| {
            units.push((call_id, None, None));
            units.len() - 1
        });
        match role {
            "user" => units[index].1 = Some(block.body),
            "assistant" => units[index].2 = Some(block.body),
            _ => {}
        }
    }

    Ok(units
        .into_iter()
        .filter_map(|(call_id, user, assistant)| {
            Some(JudgeDialogueUnit {
                call_id,
                user: user?,
                assistant: assistant?,
            })
        })
        .collect())
}

pub fn story_judge_body(units: &[JudgeDialogueUnit]) -> String {
    let mut body = String::new();
    for (index, unit) in units.iter().enumerate() {
        body.push_str(&format!(
            "### Turn {} (call_id={})\nUser:\n{}\n\nAssistant:\n{}\n\n",
            index + 1,
            unit.call_id,
            unit.user,
            unit.assistant
        ));
    }
    body
}

pub fn evaluation_units(
    records: &[crate::EventRecord],
    scope: JudgmentScope,
) -> Result<Vec<EvaluationUnit>> {
    let dialogue = dialogue_judge_units(records)?;
    Ok(match scope {
        JudgmentScope::Story => {
            let body = story_judge_body(&dialogue);
            if body.trim().is_empty() {
                Vec::new()
            } else {
                vec![EvaluationUnit {
                    call_id: STORY_CALL_ID.into(),
                    body,
                }]
            }
        }
        JudgmentScope::Turn => dialogue
            .into_iter()
            .map(|unit| EvaluationUnit {
                call_id: unit.call_id,
                body: format!("User:\n{}\n\nAssistant:\n{}", unit.user, unit.assistant),
            })
            .collect(),
    })
}

pub fn pending_evaluation_units(
    existing: &[JudgeRow],
    session_id: &str,
    rubric_id: &str,
    units: &[EvaluationUnit],
    force: bool,
) -> (Vec<EvaluationUnit>, usize) {
    if force {
        return (units.to_vec(), 0);
    }
    let pending: Vec<_> = units
        .iter()
        .filter(|unit| !has_judgment(existing, session_id, &unit.call_id, rubric_id))
        .cloned()
        .collect();
    let skipped = units.len().saturating_sub(pending.len());
    (pending, skipped)
}

pub fn manual_judge_rows(
    session_id: &str,
    rubric_id: &str,
    units: &[EvaluationUnit],
    inputs: &[ManualJudgmentInput],
) -> Result<Vec<JudgeRow>> {
    if inputs.is_empty() {
        anyhow::bail!("manual judge requires manual_scores (collect via CLI interactive mode)");
    }
    let mut rows = Vec::new();
    for unit in units {
        let matches: Vec<_> = inputs
            .iter()
            .filter(|input| {
                input.rubric_id == rubric_id
                    && input
                        .call_id
                        .as_deref()
                        .filter(|call_id| !call_id.is_empty())
                        .unwrap_or(STORY_CALL_ID)
                        == unit.call_id
            })
            .collect();
        if matches.is_empty() {
            anyhow::bail!(
                "missing manual score for call_id={} rubric={}",
                unit.call_id,
                rubric_id
            );
        }
        for input in matches {
            rows.push(JudgeRow {
                session_id: session_id.into(),
                call_id: unit.call_id.clone(),
                rubric_id: input.rubric_id.clone(),
                score: input.score.clamp(0, 100),
                verdict: normalize_verdict(&input.verdict),
                rationale: if input.rationale.starts_with(MANUAL_RATIONALE_PREFIX) {
                    input.rationale.clone()
                } else {
                    format!("{MANUAL_RATIONALE_PREFIX}{}", input.rationale)
                },
            });
        }
    }
    Ok(rows)
}

pub fn dry_run_judge_rows(
    session_id: &str,
    rubric_id: &str,
    units: &[EvaluationUnit],
) -> Vec<JudgeRow> {
    units
        .iter()
        .map(|unit| JudgeRow {
            session_id: session_id.into(),
            call_id: unit.call_id.clone(),
            rubric_id: rubric_id.into(),
            score: 100,
            verdict: "pass".into(),
            rationale: "dry-run (no LLM call)".into(),
        })
        .collect()
}

pub fn manual_few_shot_examples(
    existing: &[JudgeRow],
    rubric_id: &str,
    limit: usize,
) -> Vec<JudgeRow> {
    existing
        .iter()
        .filter(|row| {
            row.rubric_id == rubric_id && row.rationale.starts_with(MANUAL_RATIONALE_PREFIX)
        })
        .take(limit)
        .cloned()
        .collect()
}

pub fn build_llm_judge_prompt(
    scope: JudgmentScope,
    rubric_id: &str,
    units: &[EvaluationUnit],
    few_shot: &[JudgeRow],
) -> String {
    let mut examples = String::new();
    if !few_shot.is_empty() {
        examples.push_str("Reference examples (human scores):\n");
        for example in few_shot {
            examples.push_str(&format!(
                "- call_id={} score={} verdict={} rationale={}\n",
                example.call_id, example.score, example.verdict, example.rationale
            ));
        }
        examples.push('\n');
    }
    let trajectory = units
        .iter()
        .enumerate()
        .map(|(index, unit)| match scope {
            JudgmentScope::Story => {
                format!("### Full trajectory\n{}\n", unit.body)
            }
            JudgmentScope::Turn => format!(
                "### Turn {} (call_id={})\n{}\n",
                index + 1,
                unit.call_id,
                unit.body
            ),
        })
        .collect::<String>();
    let task = match scope {
        JudgmentScope::Story => format!(
            "Score the ENTIRE trajectory once (call_id=\"{STORY_CALL_ID}\") on rubric `{rubric_id}`."
        ),
        JudgmentScope::Turn => {
            format!("Score EACH dialogue turn separately on rubric `{rubric_id}`.")
        }
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

#[derive(serde::Deserialize)]
struct LlmJudgment {
    call_id: String,
    #[serde(default)]
    rubric_id: Option<String>,
    score: i64,
    verdict: String,
    rationale: String,
}

#[derive(serde::Deserialize)]
struct LlmJudgeBatch {
    judgments: Vec<LlmJudgment>,
}

pub fn parse_llm_judge_rows(
    session_id: &str,
    rubric_id: &str,
    output: &str,
) -> Result<Vec<JudgeRow>> {
    let trimmed = output.trim();
    let payload = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    let parsed: LlmJudgeBatch = serde_json::from_str(payload)
        .with_context(|| format!("parse judge JSON from model output: {output}"))?;
    Ok(parsed
        .judgments
        .into_iter()
        .filter(|judgment| judgment.rubric_id.as_deref().unwrap_or(rubric_id) == rubric_id)
        .map(|judgment| JudgeRow {
            session_id: session_id.into(),
            call_id: judgment.call_id,
            rubric_id: rubric_id.into(),
            score: judgment.score.clamp(0, 100),
            verdict: normalize_verdict(&judgment.verdict),
            rationale: judgment.rationale,
        })
        .collect())
}

fn normalize_verdict(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pass" | "ok" | "success" => "pass".into(),
        "partial" | "mixed" => "partial".into(),
        "fail" | "failed" | "failure" => "fail".into(),
        other => other.into(),
    }
}

pub fn has_judgment(rows: &[JudgeRow], session_id: &str, call_id: &str, rubric_id: &str) -> bool {
    rows.iter()
        .any(|r| r.session_id == session_id && r.call_id == call_id && r.rubric_id == rubric_id)
}

pub async fn dataset_path(session: &StoryCoords) -> Result<String> {
    Ok(story_lance_judgment_path(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )?
    .to_string_lossy()
    .into_owned())
}

const JUDGMENT_SESSION_COL: &str = "session_id";
const JUDGMENT_CALL_COL: &str = "call_id";
const JUDGMENT_RUBRIC_COL: &str = "rubric_id";
const JUDGMENT_SCORE_COL: &str = "score";
const JUDGMENT_VERDICT_COL: &str = "verdict";
const JUDGMENT_RATIONALE_COL: &str = "rationale";

fn judgment_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        Field::new(JUDGMENT_SESSION_COL, DataType::Utf8, false),
        Field::new(JUDGMENT_CALL_COL, DataType::Utf8, false),
        Field::new(JUDGMENT_RUBRIC_COL, DataType::Utf8, false),
        Field::new(JUDGMENT_SCORE_COL, DataType::Int64, false),
        Field::new(JUDGMENT_VERDICT_COL, DataType::Utf8, false),
        Field::new(JUDGMENT_RATIONALE_COL, DataType::Utf8, false),
    ]))
}

fn judgment_batch(rows: &[JudgeRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        judgment_schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.session_id.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.call_id.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.rubric_id.as_str()),
            )),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|row| row.score),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.verdict.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.rationale.as_str()),
            )),
        ],
    )
    .context("build normalized judgment batch")
}

fn normalized_rows_from_batch(batch: &RecordBatch) -> Result<Vec<JudgeRow>> {
    (0..batch.num_rows())
        .map(|index| {
            Ok(JudgeRow {
                session_id: utf8_at(batch, JUDGMENT_SESSION_COL, index)?
                    .context("judgment session_id must be non-null")?,
                call_id: utf8_at(batch, JUDGMENT_CALL_COL, index)?
                    .context("judgment call_id must be non-null")?,
                rubric_id: utf8_at(batch, JUDGMENT_RUBRIC_COL, index)?
                    .context("judgment rubric_id must be non-null")?,
                score: i64_at(batch, JUDGMENT_SCORE_COL, index)?
                    .context("judgment score must be non-null")?,
                verdict: utf8_at(batch, JUDGMENT_VERDICT_COL, index)?
                    .context("judgment verdict must be non-null")?,
                rationale: utf8_at(batch, JUDGMENT_RATIONALE_COL, index)?
                    .context("judgment rationale must be non-null")?,
            })
        })
        .collect()
}

/// Read normalized judgments ordered by session, rubric, then unit.
pub async fn read_judge_rows(session: &StoryCoords) -> Result<Vec<JudgeRow>> {
    let uri = dataset_path(session).await?;
    let ds = match Dataset::open(&uri).await {
        Ok(ds) => ds,
        Err(lance::Error::DatasetNotFound { .. }) => return Ok(Vec::new()),
        Err(error) => return Err(anyhow::anyhow!("{error:#}")).context("open judgments.lance"),
    };
    let batches: Vec<RecordBatch> = ds
        .scan()
        .try_into_stream()
        .await
        .context("scan judgments.lance")?
        .try_collect()
        .await
        .context("collect judgments.lance")?;
    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(normalized_rows_from_batch(batch)?);
    }
    rows.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.rubric_id.cmp(&b.rubric_id))
            .then_with(|| a.call_id.cmp(&b.call_id))
    });
    Ok(rows)
}

fn utf8_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>> {
    let Some(idx) = batch.schema().index_of(name).ok() else {
        return Ok(None);
    };
    let col = batch.column(idx);
    let Some(a) = col.as_any().downcast_ref::<StringArray>() else {
        anyhow::bail!("expected Utf8 column {name}");
    };
    if a.is_null(row) {
        Ok(None)
    } else {
        Ok(Some(a.value(row).to_string()))
    }
}

fn i64_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<i64>> {
    let Some(idx) = batch.schema().index_of(name).ok() else {
        return Ok(None);
    };
    let col = batch.column(idx);
    let Some(a) = col.as_any().downcast_ref::<Int64Array>() else {
        anyhow::bail!("expected Int64 column {name}");
    };
    if a.is_null(row) {
        Ok(None)
    } else {
        Ok(Some(a.value(row)))
    }
}

/// Upsert normalized judgments by `(session_id, call_id, rubric_id)`.
pub async fn write_judge_rows(session: &StoryCoords, rows: &[JudgeRow]) -> Result<String> {
    if rows.is_empty() {
        return dataset_path(session).await;
    }
    let uri = dataset_path(session).await?;
    let _guard = crate::store::dataset_write_lock::acquire(&uri).await?;

    let mut keys = HashSet::new();
    for row in rows {
        anyhow::ensure!(
            keys.insert((&row.session_id, &row.call_id, &row.rubric_id)),
            "duplicate judgment key ({}, {}, {}) in one write",
            row.session_id,
            row.call_id,
            row.rubric_id
        );
    }
    let batch = judgment_batch(rows)?;

    match Dataset::open(&uri).await {
        Ok(ds) => {
            let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], judgment_schema()));
            MergeInsertBuilder::try_new(
                Arc::new(ds),
                vec![
                    JUDGMENT_SESSION_COL.to_string(),
                    JUDGMENT_CALL_COL.to_string(),
                    JUDGMENT_RUBRIC_COL.to_string(),
                ],
            )
            .context("build judgment upsert")?
            .when_matched(WhenMatched::UpdateAll)
            .when_not_matched(WhenNotMatched::InsertAll)
            .try_build()
            .context("build normalized judgment merge job")?
            .execute_reader(reader)
            .await
            .context("upsert judgments.lance")?;
        }
        Err(lance::Error::DatasetNotFound { .. }) => {
            if !uri.contains("://") {
                if let Some(parent) = std::path::Path::new(&uri).parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("create judgment root {}", parent.display()))?;
                }
            }
            InsertBuilder::new(&uri)
                .execute(vec![batch])
                .await
                .with_context(|| format!("create judgments.lance at {uri}"))?;
        }
        Err(error) => return Err(anyhow::anyhow!("{error:#}")).context("open judgments.lance"),
    }

    Ok(uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialogue_units_pair_canonical_events_by_call_id() {
        let records = vec![
            crate::EventRecord {
                identity: crate::EventIdentity::default(),
                seq: 0,
                source: "test".into(),
                kind: "llm.request".into(),
                timestamp: None,
                session_id: Some("session".into()),
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: Some("call-1".into()),
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"user_content": "hello"}),
            },
            crate::EventRecord {
                identity: crate::EventIdentity::default(),
                seq: 1,
                source: "test".into(),
                kind: "llm.response".into(),
                timestamp: None,
                session_id: Some("session".into()),
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: Some("call-1".into()),
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"assistant_content": "world"}),
            },
        ];
        let units = dialogue_judge_units(&records).unwrap();
        assert_eq!(
            units,
            vec![JudgeDialogueUnit {
                call_id: "call-1".into(),
                user: "hello".into(),
                assistant: "world".into(),
            }]
        );
    }
}
