//! Call-level judge rows persisted as `{run}/layers/judge_{rubric}.vortex`.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema as ArrowSchema};
use vortex::array::arrow::{ArrowSessionExt, FromArrowArray};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::VortexSessionExecute;
use vortex::file::{OpenOptionsSessionExt, WriteOptionsSessionExt};
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

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

fn vortex_session() -> &'static VortexSession {
    static SESSION: OnceLock<VortexSession> = OnceLock::new();
    SESSION.get_or_init(VortexSession::default)
}

fn vortex_err(e: vortex::error::VortexError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
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
            .map(|_| {
                batch
                    .column(idx)
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .unwrap()
            })
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

async fn read_all_rows(path: &Path) -> Result<Vec<JudgeRow>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let file = vortex_session()
        .open_options()
        .open_path(path)
        .await
        .map_err(vortex_err)?;
    let array = file
        .scan()
        .map_err(vortex_err)?
        .into_array_stream()
        .map_err(vortex_err)?
        .read_all()
        .await
        .map_err(vortex_err)?;
    if array.len() == 0 {
        return Ok(Vec::new());
    }
    let schema = judge_schema();
    let mut ctx = vortex_session().create_execution_ctx();
    let target = Field::new(
        "",
        DataType::Struct(schema.fields.clone()),
        array.dtype().is_nullable(),
    );
    let arrow = vortex_session()
        .arrow()
        .execute_arrow(array, Some(&target), &mut ctx)
        .map_err(vortex_err)?;
    let batch = RecordBatch::from(arrow.as_struct());
    rows_from_batch(&batch)
}

pub async fn read_judge_rows(path: &Path) -> Result<Vec<JudgeRow>> {
    read_all_rows(path).await
}

pub async fn write_judge_rows(path: &Path, rows: &[JudgeRow]) -> Result<()> {
    if rows.is_empty() {
        if path.is_file() {
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("remove empty judge sidecar {}", path.display()))?;
        }
        return Ok(());
    }
    let batch = record_batch_from_judge_rows(rows)?;
    let array = vortex::array::ArrayRef::from_arrow(batch, false).map_err(vortex_err)?;
    let stream = array.to_array_stream();
    let tmp = path.with_extension("vortex.tmp");
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("create {}", tmp.display()))?;
    vortex_session()
        .write_options()
        .write(&mut file, stream)
        .await
        .map_err(vortex_err)?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
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
