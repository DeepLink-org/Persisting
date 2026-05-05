//! Lance-backed trajectory storage under `{storage}/{name}/` (standard Lance dataset URI).
//!
//! Each append writes rows with monotonic `seq` (0-based global order) and UTF-8 `record_ron`
//! (one wire RON value per row). Replay applies `order_by(seq)`, then SQL-style `limit`/`offset`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use lance::Dataset;
use lance::Error as LanceError;
use lance::dataset::scanner::ColumnOrdering;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
pub use persisting_proto::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryReplayRequest,
    TrajectoryReplayResponse, TrajectoryStatsRequest, TrajectoryStatsResponse,
};

/// Arrow / Lance column storing one trajectory step as RON text.
pub const TRAJECTORY_SEQ_COL: &str = "seq";
pub const TRAJECTORY_RECORD_COL: &str = "record_ron";

const APPEND_CHUNK_ROWS: usize = 8192;

fn trajectory_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        Field::new(TRAJECTORY_SEQ_COL, DataType::Int64, false),
        Field::new(TRAJECTORY_RECORD_COL, DataType::Utf8, false),
    ]))
}

fn validate_namespace(storage: &str, name: &str) -> Result<()> {
    if storage.trim().is_empty() {
        anyhow::bail!("storage path must not be empty");
    }
    if name.trim().is_empty() {
        anyhow::bail!("trajectory name must not be empty");
    }
    Ok(())
}

/// Directory passed to [`Dataset::open`] (`…/storage/name`, contains `data.lance/`).
pub fn trajectory_dataset_dir(storage: &str, name: &str) -> PathBuf {
    Path::new(storage).join(name)
}

pub fn dataset_uri_display(storage: &str, name: &str) -> String {
    trajectory_dataset_dir(storage, name)
        .to_string_lossy()
        .into_owned()
}

async fn open_trajectory(uri: &str) -> Result<Option<Dataset>> {
    match Dataset::open(uri).await {
        Ok(ds) => Ok(Some(ds)),
        Err(e) if matches!(e, LanceError::DatasetNotFound { .. }) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

fn parse_ronl_records(records_ronl: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (line_number, line) in records_ronl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _: ron::value::Value = ron::from_str(line).map_err(|err| {
            anyhow::anyhow!("invalid RON at line {}: {}", line_number + 1, err)
        })?;
        out.push(line.to_string());
    }
    Ok(out)
}

fn record_batches_for_append(
    schema: Arc<ArrowSchema>,
    mut start_seq: i64,
    lines: &[String],
) -> Result<Vec<RecordBatch>> {
    let mut batches = Vec::new();
    for chunk in lines.chunks(APPEND_CHUNK_ROWS) {
        let n = chunk.len();
        let seqs: Vec<i64> = (0..n).map(|i| start_seq + i as i64).collect();
        start_seq += n as i64;
        let seq_arr = Int64Array::from(seqs);
        let record_arr = StringArray::from_iter_values(chunk.iter().map(|s| s.as_str()));
        batches.push(
            RecordBatch::try_new(schema.clone(), vec![Arc::new(seq_arr), Arc::new(record_arr)])
                .context("build trajectory RecordBatch")?,
        );
    }
    Ok(batches)
}

pub async fn append_async(request: TrajectoryAppendRequest) -> Result<TrajectoryAppendResponse> {
    validate_namespace(&request.storage, &request.name)?;
    let lines = parse_ronl_records(&request.records_ronl)?;
    let accepted = lines.len();
    let root = trajectory_dataset_dir(&request.storage, &request.name);
    let uri = root.to_string_lossy().to_string();

    if accepted == 0 {
        return Ok(TrajectoryAppendResponse {
            dataset: uri.clone(),
            storage: request.storage,
            name: request.name,
            accepted_records: 0,
            status: "ok".to_string(),
            note: "No non-empty RON lines; dataset unchanged.".to_string(),
        });
    }

    tokio::fs::create_dir_all(&request.storage)
        .await
        .with_context(|| format!("create_dir_all {}", request.storage))?;

    let schema = trajectory_schema();

    if let Some(ds) = open_trajectory(uri.as_str()).await? {
        let ds = Arc::new(ds);
        let base_seq = ds
            .count_rows(None)
            .await
            .context("count_rows before append")? as i64;
        let batches = record_batches_for_append(schema, base_seq, &lines)?;
        InsertBuilder::new(ds)
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute(batches)
            .await
            .context("append trajectory batches")?;
    } else {
        let batches = record_batches_for_append(schema, 0, &lines)?;
        InsertBuilder::new(uri.as_str())
            .execute(batches)
            .await
            .context("create trajectory dataset with first batches")?;
    }

    Ok(TrajectoryAppendResponse {
        dataset: uri.clone(),
        storage: request.storage,
        name: request.name,
        accepted_records: accepted,
        status: "ok".to_string(),
        note: format!(
            "Appended {} row(s) to Lance dataset at {} (columns {}, {}).",
            accepted, uri, TRAJECTORY_SEQ_COL, TRAJECTORY_RECORD_COL
        ),
    })
}

pub async fn replay_async(request: TrajectoryReplayRequest) -> Result<TrajectoryReplayResponse> {
    validate_namespace(&request.storage, &request.name)?;
    let root = trajectory_dataset_dir(&request.storage, &request.name);
    let uri = root.to_string_lossy().to_string();

    let Some(ds) = open_trajectory(uri.as_str()).await? else {
        anyhow::bail!("trajectory dataset does not exist at {}", uri);
    };

    let note_tid = if request.trajectory_id.is_some() {
        " trajectory_id filter is reserved (not stored per row yet)."
    } else {
        ""
    };

    // Full-schema scan: `project` + `order_by` Together triggers TakeExec row-address requirements;
    // we sort by `seq` on all columns then extract `record_ron` in-process.
    let mut scan = ds.scan();
    scan.order_by(Some(vec![ColumnOrdering::asc_nulls_first(
        TRAJECTORY_SEQ_COL.to_string(),
    )]))
    .context("order_by seq")?;
    scan.limit(
        request.limit.map(|x| x as i64),
        Some(request.offset as i64),
    )
    .context("limit/offset")?;

    let batch = scan
        .try_into_batch()
        .await
        .context("replay scan")?;

    let col_idx = batch
        .schema()
        .fields()
        .iter()
        .position(|f| f.name() == TRAJECTORY_RECORD_COL)
        .ok_or_else(|| anyhow::anyhow!("batch schema missing column '{}'", TRAJECTORY_RECORD_COL))?;

    let mut records = Vec::with_capacity(batch.num_rows());
    let col = batch.column(col_idx);
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        for i in 0..batch.num_rows() {
            records.push(a.value(i).to_string());
        }
    } else {
        anyhow::bail!("expected Utf8 column {}", TRAJECTORY_RECORD_COL);
    }

    Ok(TrajectoryReplayResponse {
        storage: request.storage,
        name: request.name,
        records,
        status: "ok".to_string(),
        note: format!(
            "Ordered scan on '{}' with limit/offset (offset={}, limit={:?}).{}",
            TRAJECTORY_SEQ_COL, request.offset, request.limit, note_tid
        ),
    })
}

pub async fn stats_async(request: TrajectoryStatsRequest) -> Result<TrajectoryStatsResponse> {
    validate_namespace(&request.storage, &request.name)?;
    let root = trajectory_dataset_dir(&request.storage, &request.name);
    let uri = root.to_string_lossy().to_string();

    let Some(ds) = open_trajectory(uri.as_str()).await? else {
        return Ok(TrajectoryStatsResponse {
            dataset: uri,
            storage: request.storage,
            name: request.name,
            status: "missing".to_string(),
            note: "No Lance dataset at this path yet; use trajectory add first.".to_string(),
        });
    };
    let rows = ds.count_rows(None).await.context("count_rows")?;
    let ver = ds.version().version;

    Ok(TrajectoryStatsResponse {
        dataset: uri,
        storage: request.storage,
        name: request.name,
        status: "ok".to_string(),
        note: format!(
            "rows={}, manifest_version={}, columns ['{}','{}']",
            rows, ver, TRAJECTORY_SEQ_COL, TRAJECTORY_RECORD_COL
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ronl_counts() {
        let v = parse_ronl_records("(a:1)\n\n(b:2)\n").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[tokio::test]
    async fn append_replay_stats_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            name: "run_a".into(),
            records_ronl: "(step:1)\n(step:2)\n".into(),
        })
        .await
        .unwrap();
        assert_eq!(append.accepted_records, 2);
        assert_eq!(append.status, "ok");

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            name: "run_a".into(),
            trajectory_id: None,
            offset: 0,
            limit: Some(10),
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 2);
        assert!(replay.records[0].contains("step:1"));

        let replay_page = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            name: "run_a".into(),
            trajectory_id: None,
            offset: 1,
            limit: Some(1),
        })
        .await
        .unwrap();
        assert_eq!(replay_page.records.len(), 1);
        assert!(replay_page.records[0].contains("step:2"));

        let st = stats_async(TrajectoryStatsRequest {
            storage: storage_s,
            name: "run_a".into(),
        })
        .await
        .unwrap();
        assert_eq!(st.status, "ok");
        assert!(st.note.contains("rows=2"));
    }
}
