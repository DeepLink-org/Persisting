//! Judge results as native Lance columns on `{run}/events.lance`.
//!
//! Per rubric `R` (sanitized), columns:
//! - `judge_{R}_score` (Int64, nullable)
//! - `judge_{R}_verdict` (Utf8, nullable)
//! - `judge_{R}_rationale` (Utf8, nullable)
//! - `judge_{R}_unit` (Utf8, nullable) — `__story__` or turn `call_id`
//!
//! Written via schema evolution (`add_columns` AllNulls) + `MergeInsert` on `seq`
//! (updates only new column files; existing event payloads are not rewritten).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{MergeInsertBuilder, NewColumnTransform, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use lance::Dataset;

use crate::{
    raw_event_lance_path, TrajectorySession, TRAJECTORY_CALL_ID_COL, TRAJECTORY_SEQ_COL,
    TRAJECTORY_SESSION_ID_COL,
};

pub const STORY_CALL_ID: &str = "__story__";
pub const MANUAL_RATIONALE_PREFIX: &str = "[manual] ";

const SCORE_SUFFIX: &str = "_score";
const VERDICT_SUFFIX: &str = "_verdict";
const RATIONALE_SUFFIX: &str = "_rationale";
const UNIT_SUFFIX: &str = "_unit";

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

pub fn layer_field_name(rubric_id: &str) -> String {
    format!("judge_{}", sanitize_layer_token(rubric_id))
}

fn sanitize_layer_token(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

fn score_col(prefix: &str) -> String {
    format!("{prefix}{SCORE_SUFFIX}")
}
fn verdict_col(prefix: &str) -> String {
    format!("{prefix}{VERDICT_SUFFIX}")
}
fn rationale_col(prefix: &str) -> String {
    format!("{prefix}{RATIONALE_SUFFIX}")
}
fn unit_col(prefix: &str) -> String {
    format!("{prefix}{UNIT_SUFFIX}")
}

fn rubric_from_score_column(name: &str) -> Option<String> {
    let rest = name.strip_prefix("judge_")?.strip_suffix(SCORE_SUFFIX)?;
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

pub fn has_judgment(rows: &[JudgeRow], session_id: &str, call_id: &str, rubric_id: &str) -> bool {
    let prefix = layer_field_name(rubric_id);
    let rubric_token = prefix.strip_prefix("judge_").unwrap_or(rubric_id);
    rows.iter().any(|r| {
        r.session_id == session_id
            && r.call_id == call_id
            && (r.rubric_id == rubric_id || r.rubric_id == rubric_token)
    })
}

pub async fn dataset_path(session: &TrajectorySession) -> Result<String> {
    Ok(raw_event_lance_path(session)?
        .to_string_lossy()
        .into_owned())
}

/// Read distinct judge rows from `events.lance` (deduped by session/unit/rubric).
pub async fn read_judge_rows(session: &TrajectorySession) -> Result<Vec<JudgeRow>> {
    let path = raw_event_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let ds = match Dataset::open(&uri).await {
        Ok(ds) => ds,
        Err(lance::Error::DatasetNotFound { .. }) => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::anyhow!("{:#}", e)).context("open events.lance for judge"),
    };

    let prefixes = discover_judge_prefixes(ds.schema());
    if prefixes.is_empty() {
        return Ok(Vec::new());
    }

    let mut project = vec![
        TRAJECTORY_SEQ_COL.to_string(),
        TRAJECTORY_SESSION_ID_COL.to_string(),
        TRAJECTORY_CALL_ID_COL.to_string(),
    ];
    for p in &prefixes {
        project.push(score_col(p));
        project.push(verdict_col(p));
        project.push(rationale_col(p));
        project.push(unit_col(p));
    }

    let mut scan = ds.scan();
    scan.project(&project).context("project judge columns")?;
    let stream = scan
        .try_into_stream()
        .await
        .context("scan events.lance for judge columns")?;
    let batches: Vec<RecordBatch> = stream.try_collect().await.context("collect judge scan")?;

    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for batch in &batches {
        for prefix in &prefixes {
            let rubric_id = prefix
                .strip_prefix("judge_")
                .unwrap_or(prefix.as_str())
                .to_string();
            extract_rows_from_batch(batch, prefix, &rubric_id, &mut seen, &mut rows)?;
        }
    }
    rows.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.rubric_id.cmp(&b.rubric_id))
            .then_with(|| a.call_id.cmp(&b.call_id))
    });
    Ok(rows)
}

fn discover_judge_prefixes(schema: &lance::datatypes::Schema) -> Vec<String> {
    let mut out = Vec::new();
    for field in &schema.fields {
        if let Some(rubric) = rubric_from_score_column(&field.name) {
            out.push(format!("judge_{rubric}"));
        }
    }
    out.sort();
    out.dedup();
    out
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

fn extract_rows_from_batch(
    batch: &RecordBatch,
    prefix: &str,
    rubric_id: &str,
    seen: &mut HashSet<(String, String, String)>,
    out: &mut Vec<JudgeRow>,
) -> Result<()> {
    let score_name = score_col(prefix);
    if batch.schema().index_of(&score_name).is_err() {
        return Ok(());
    }
    for i in 0..batch.num_rows() {
        let Some(score) = i64_at(batch, &score_name, i)? else {
            continue;
        };
        let session_id = utf8_at(batch, TRAJECTORY_SESSION_ID_COL, i)?.unwrap_or_default();
        if session_id.is_empty() {
            continue;
        }
        let unit = utf8_at(batch, &unit_col(prefix), i)?
            .or_else(|| utf8_at(batch, TRAJECTORY_CALL_ID_COL, i).ok().flatten())
            .unwrap_or_else(|| STORY_CALL_ID.to_string());
        let key = (session_id.clone(), unit.clone(), rubric_id.to_string());
        if !seen.insert(key) {
            continue;
        }
        out.push(JudgeRow {
            session_id,
            call_id: unit,
            rubric_id: rubric_id.to_string(),
            score,
            verdict: utf8_at(batch, &verdict_col(prefix), i)?.unwrap_or_default(),
            rationale: utf8_at(batch, &rationale_col(prefix), i)?.unwrap_or_default(),
        });
    }
    Ok(())
}

/// Ensure judge columns exist, then merge-insert values onto matching event rows by `seq`.
pub async fn write_judge_rows(session: &TrajectorySession, rows: &[JudgeRow]) -> Result<String> {
    if rows.is_empty() {
        return dataset_path(session).await;
    }
    let path = raw_event_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let mut ds = Dataset::open(&uri)
        .await
        .with_context(|| format!("open events.lance for judge write at {uri}"))?;

    let mut by_rubric: HashMap<String, Vec<&JudgeRow>> = HashMap::new();
    for row in rows {
        by_rubric
            .entry(row.rubric_id.clone())
            .or_default()
            .push(row);
    }

    for (rubric_id, rubric_rows) in by_rubric {
        let prefix = ensure_judge_columns(&mut ds, &rubric_id).await?;
        apply_rubric_rows(&mut ds, &prefix, &rubric_rows).await?;
    }

    Ok(uri)
}

async fn ensure_judge_columns(ds: &mut Dataset, rubric_id: &str) -> Result<String> {
    let prefix = layer_field_name(rubric_id);
    let existing: HashSet<String> = ds.schema().fields.iter().map(|f| f.name.clone()).collect();
    let candidates = [
        (score_col(&prefix), DataType::Int64),
        (verdict_col(&prefix), DataType::Utf8),
        (rationale_col(&prefix), DataType::Utf8),
        (unit_col(&prefix), DataType::Utf8),
    ];
    let missing: Vec<Field> = candidates
        .iter()
        .filter(|(name, _)| !existing.contains(name))
        .map(|(name, dt)| Field::new(name, dt.clone(), true))
        .collect();
    if !missing.is_empty() {
        ds.add_columns(
            NewColumnTransform::AllNulls(Arc::new(ArrowSchema::new(missing))),
            None,
            None,
        )
        .await
        .with_context(|| format!("add_columns for judge prefix {prefix}"))?;
    }
    Ok(prefix)
}

async fn apply_rubric_rows(ds: &mut Dataset, prefix: &str, rows: &[&JudgeRow]) -> Result<()> {
    // Map judgment unit → values. Story judgments apply to every row of that session.
    let mut story_by_session: HashMap<&str, &&JudgeRow> = HashMap::new();
    let mut turn_by_key: HashMap<(&str, &str), &&JudgeRow> = HashMap::new();
    for row in rows {
        if row.call_id == STORY_CALL_ID {
            story_by_session.insert(row.session_id.as_str(), row);
        } else {
            turn_by_key.insert((row.session_id.as_str(), row.call_id.as_str()), row);
        }
    }

    let project = [
        TRAJECTORY_SEQ_COL,
        TRAJECTORY_SESSION_ID_COL,
        TRAJECTORY_CALL_ID_COL,
    ];
    let mut scan = ds.scan();
    scan.project(&project).context("project seq/session/call")?;
    let stream = scan
        .try_into_stream()
        .await
        .context("scan rows for judge merge")?;
    let batches: Vec<RecordBatch> = stream.try_collect().await.context("collect seq scan")?;

    let mut seqs = Vec::new();
    let mut scores: Vec<Option<i64>> = Vec::new();
    let mut verdicts: Vec<Option<String>> = Vec::new();
    let mut rationales: Vec<Option<String>> = Vec::new();
    let mut units: Vec<Option<String>> = Vec::new();

    for batch in &batches {
        for i in 0..batch.num_rows() {
            let seq = i64_at(batch, TRAJECTORY_SEQ_COL, i)?
                .ok_or_else(|| anyhow::anyhow!("seq must be non-null"))?;
            let session_id = utf8_at(batch, TRAJECTORY_SESSION_ID_COL, i)?.unwrap_or_default();
            let call_id = utf8_at(batch, TRAJECTORY_CALL_ID_COL, i)?;

            let matched = turn_by_key
                .get(&(session_id.as_str(), call_id.as_deref().unwrap_or("")))
                .copied()
                .or_else(|| story_by_session.get(session_id.as_str()).copied());

            let Some(j) = matched else {
                continue;
            };
            seqs.push(seq);
            scores.push(Some(j.score));
            verdicts.push(Some(j.verdict.clone()));
            rationales.push(Some(j.rationale.clone()));
            units.push(Some(j.call_id.clone()));
        }
    }

    if seqs.is_empty() {
        anyhow::bail!(
            "judge merge matched 0 event rows for prefix {prefix}; \
             check session_id/call_id alignment with events.lance"
        );
    }

    let schema = Arc::new(ArrowSchema::new(vec![
        Field::new(TRAJECTORY_SEQ_COL, DataType::Int64, false),
        Field::new(score_col(prefix), DataType::Int64, true),
        Field::new(verdict_col(prefix), DataType::Utf8, true),
        Field::new(rationale_col(prefix), DataType::Utf8, true),
        Field::new(unit_col(prefix), DataType::Utf8, true),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(seqs)),
            Arc::new(Int64Array::from(scores)),
            Arc::new(StringArray::from(
                verdicts.iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rationales.iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                units.iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
            )),
        ],
    )
    .context("build judge merge RecordBatch")?;

    let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    let (updated, _stats) =
        MergeInsertBuilder::try_new(Arc::new(ds.clone()), vec![TRAJECTORY_SEQ_COL.to_string()])
            .context("MergeInsertBuilder")?
            .when_matched(WhenMatched::UpdateAll)
            .when_not_matched(WhenNotMatched::DoNothing)
            .try_build()
            .context("build merge insert job")?
            .execute_reader(reader)
            .await
            .context("merge insert judge columns")?;

    *ds = Arc::try_unwrap(updated).unwrap_or_else(|arc| (*arc).clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rubric_for_column_prefix() {
        assert_eq!(layer_field_name("default"), "judge_default");
        assert_eq!(layer_field_name("task/success"), "judge_task_success");
        assert_eq!(
            rubric_from_score_column("judge_default_score").as_deref(),
            Some("default")
        );
    }

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
