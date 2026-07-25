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
use lance::dataset::{
    MergeInsertBuilder, NewColumnTransform, WhenMatched, WhenNotMatched,
};
use lance::deps::arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use lance::Dataset;

use super::store::session_lance_path;
use super::{
    TrajectorySession, TRAJECTORY_CALL_ID_COL, TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL,
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
    Ok(session_lance_path(session)?.to_string_lossy().into_owned())
}

/// Read distinct judge rows from `events.lance` (deduped by session/unit/rubric).
pub async fn read_judge_rows(session: &TrajectorySession) -> Result<Vec<JudgeRow>> {
    let path = session_lance_path(session)?;
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
    scan.project(&project)
        .context("project judge columns")?;
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
    let path = session_lance_path(session)?;
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
                verdicts
                    .iter()
                    .map(|v| v.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rationales
                    .iter()
                    .map(|v| v.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                units.iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
            )),
        ],
    )
    .context("build judge merge RecordBatch")?;

    let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
    let (updated, _stats) = MergeInsertBuilder::try_new(Arc::new(ds.clone()), vec![
        TRAJECTORY_SEQ_COL.to_string(),
    ])
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
        assert_eq!(rubric_from_score_column("judge_default_score").as_deref(), Some("default"));
    }
}
