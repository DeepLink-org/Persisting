//! Call-level judge rows persisted as `{run}/layers/judge_{rubric}.lance`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use lance::Dataset;
use lance::Error as LanceError;

pub const JUDGE_SESSION_ID_COL: &str = "session_id";
pub const JUDGE_CALL_ID_COL: &str = "call_id";
pub const JUDGE_RUBRIC_ID_COL: &str = "rubric_id";
pub const JUDGE_SCORE_COL: &str = "score";
pub const JUDGE_VERDICT_COL: &str = "verdict";
pub const JUDGE_RATIONALE_COL: &str = "rationale";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeRow {
    pub session_id: String,
    pub call_id: String,
    pub rubric_id: String,
    pub score: i64,
    pub verdict: String,
    pub rationale: String,
}

pub fn judge_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        Field::new(JUDGE_SESSION_ID_COL, DataType::Utf8, false),
        Field::new(JUDGE_CALL_ID_COL, DataType::Utf8, false),
        Field::new(JUDGE_RUBRIC_ID_COL, DataType::Utf8, false),
        Field::new(JUDGE_SCORE_COL, DataType::Int64, false),
        Field::new(JUDGE_VERDICT_COL, DataType::Utf8, false),
        Field::new(JUDGE_RATIONALE_COL, DataType::Utf8, false),
    ]))
}

pub fn record_batch_from_judge_rows(rows: &[JudgeRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        judge_schema(),
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.call_id.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.rubric_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.score).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.verdict.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.rationale.as_str())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .context("build judge RecordBatch")
}

fn rows_from_batch(batch: &RecordBatch) -> Result<Vec<JudgeRow>> {
    let col = |name: &str| -> Result<&StringArray> {
        let idx = batch
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == name)
            .ok_or_else(|| anyhow::anyhow!("judge batch missing column {name}"))?;
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow::anyhow!("expected Utf8 column {name}"))
    };
    let session = col(JUDGE_SESSION_ID_COL)?;
    let call = col(JUDGE_CALL_ID_COL)?;
    let rubric = col(JUDGE_RUBRIC_ID_COL)?;
    let score_col = {
        let idx = batch
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == JUDGE_SCORE_COL)
            .ok_or_else(|| anyhow::anyhow!("judge batch missing score"))?;
        batch
            .column(idx)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| anyhow::anyhow!("expected Int64 score"))?
    };
    let verdict = col(JUDGE_VERDICT_COL)?;
    let rationale = col(JUDGE_RATIONALE_COL)?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        rows.push(JudgeRow {
            session_id: session.value(i).to_string(),
            call_id: call.value(i).to_string(),
            rubric_id: rubric.value(i).to_string(),
            score: score_col.value(i),
            verdict: verdict.value(i).to_string(),
            rationale: rationale.value(i).to_string(),
        });
    }
    Ok(rows)
}

async fn dataset_exists(uri: &str) -> Result<bool> {
    match Dataset::open(uri).await {
        Ok(_) => Ok(true),
        Err(e) if matches!(e, LanceError::DatasetNotFound { .. }) => Ok(false),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

async fn read_all_rows(path: &Path) -> Result<Vec<JudgeRow>> {
    let uri = path.to_string_lossy();
    if !dataset_exists(&uri).await? {
        return Ok(Vec::new());
    }
    let ds = Dataset::open(uri.as_ref())
        .await
        .with_context(|| format!("open judge Lance dataset {}", path.display()))?;
    let stream = ds
        .scan()
        .try_into_stream()
        .await
        .with_context(|| format!("scan judge Lance dataset {}", path.display()))?;
    let batches: Vec<RecordBatch> = stream
        .try_collect()
        .await
        .with_context(|| format!("collect judge Lance batches {}", path.display()))?;
    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(rows_from_batch(batch)?);
    }
    Ok(rows)
}

pub async fn read_judge_rows(path: &Path) -> Result<Vec<JudgeRow>> {
    read_all_rows(path).await
}

pub async fn write_judge_rows(path: &Path, rows: &[JudgeRow]) -> Result<()> {
    let uri = path.to_string_lossy().into_owned();
    if rows.is_empty() {
        if path.exists() {
            tokio::fs::remove_dir_all(path)
                .await
                .with_context(|| format!("remove empty judge sidecar {}", path.display()))?;
        }
        return Ok(());
    }
    let batch = record_batch_from_judge_rows(rows)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let mode = if dataset_exists(&uri).await? {
        WriteMode::Overwrite
    } else {
        WriteMode::Create
    };
    InsertBuilder::new(uri.as_str())
        .with_params(&WriteParams {
            mode,
            ..Default::default()
        })
        .execute(vec![batch])
        .await
        .with_context(|| format!("write judge Lance dataset {}", path.display()))?;
    Ok(())
}

pub fn merge_judge_rows(
    existing: Vec<JudgeRow>,
    incoming: Vec<JudgeRow>,
    session_id: &str,
    rubric_id: &str,
) -> Vec<JudgeRow> {
    let mut kept: Vec<JudgeRow> = existing
        .into_iter()
        .filter(|r| r.session_id != session_id || r.rubric_id != rubric_id)
        .collect();
    kept.extend(incoming);
    kept.sort_by(|a, b| {
        a.session_id
            .cmp(&b.session_id)
            .then_with(|| a.call_id.cmp(&b.call_id))
    });
    kept
}

pub fn has_judgment(rows: &[JudgeRow], session_id: &str, call_id: &str, rubric_id: &str) -> bool {
    rows.iter()
        .any(|r| r.session_id == session_id && r.call_id == call_id && r.rubric_id == rubric_id)
}
