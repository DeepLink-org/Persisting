//! Queryable lineage catalog for derived trajectory artifacts.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use lance::Dataset;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StoryCoords;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionRow {
    pub revision_id: String,
    #[serde(default)]
    pub parent_revision_ids: Vec<String>,
    pub kind: String,
    pub canonical_snapshot: String,
    pub recipe: Value,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub output_refs: Vec<String>,
}

pub fn revision_dataset_path(session: &StoryCoords) -> Result<String> {
    Ok(session
        .run_dir()?
        .join("revisions.lance")
        .to_string_lossy()
        .into_owned())
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(
        [
            "revision_id",
            "parent_revision_ids_json",
            "kind",
            "canonical_snapshot",
            "recipe_json",
            "status",
            "created_at",
            "output_refs_json",
        ]
        .into_iter()
        .map(|name| Field::new(name, DataType::Utf8, false))
        .collect::<Vec<_>>(),
    ))
}

fn batch(rows: &[RevisionRow]) -> Result<RecordBatch> {
    let strings = |values: Vec<String>| Arc::new(StringArray::from(values)) as _;
    RecordBatch::try_new(
        schema(),
        vec![
            strings(rows.iter().map(|r| r.revision_id.clone()).collect()),
            strings(
                rows.iter()
                    .map(|r| serde_json::to_string(&r.parent_revision_ids).unwrap())
                    .collect(),
            ),
            strings(rows.iter().map(|r| r.kind.clone()).collect()),
            strings(rows.iter().map(|r| r.canonical_snapshot.clone()).collect()),
            strings(rows.iter().map(|r| r.recipe.to_string()).collect()),
            strings(rows.iter().map(|r| r.status.clone()).collect()),
            strings(rows.iter().map(|r| r.created_at.clone()).collect()),
            strings(
                rows.iter()
                    .map(|r| serde_json::to_string(&r.output_refs).unwrap())
                    .collect(),
            ),
        ],
    )
    .context("build revision catalog batch")
}

fn text(batch: &RecordBatch, column: &str, row: usize) -> Result<String> {
    let array = batch
        .column(batch.schema().index_of(column)?)
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("revision column {column} must be Utf8"))?;
    anyhow::ensure!(
        !array.is_null(row),
        "revision column {column} must be non-null"
    );
    Ok(array.value(row).to_string())
}

pub async fn read_revisions(session: &StoryCoords) -> Result<Vec<RevisionRow>> {
    let uri = revision_dataset_path(session)?;
    let dataset = match Dataset::open(&uri).await {
        Ok(dataset) => dataset,
        Err(lance::Error::DatasetNotFound { .. }) => return Ok(Vec::new()),
        Err(error) => return Err(anyhow::anyhow!(error)).context("open revisions.lance"),
    };
    let batches: Vec<RecordBatch> = dataset
        .scan()
        .try_into_stream()
        .await?
        .try_collect()
        .await?;
    let mut rows = Vec::new();
    for batch in batches {
        for row in 0..batch.num_rows() {
            rows.push(RevisionRow {
                revision_id: text(&batch, "revision_id", row)?,
                parent_revision_ids: serde_json::from_str(&text(
                    &batch,
                    "parent_revision_ids_json",
                    row,
                )?)?,
                kind: text(&batch, "kind", row)?,
                canonical_snapshot: text(&batch, "canonical_snapshot", row)?,
                recipe: serde_json::from_str(&text(&batch, "recipe_json", row)?)?,
                status: text(&batch, "status", row)?,
                created_at: text(&batch, "created_at", row)?,
                output_refs: serde_json::from_str(&text(&batch, "output_refs_json", row)?)?,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.revision_id.cmp(&b.revision_id))
    });
    Ok(rows)
}

pub async fn write_revisions(session: &StoryCoords, rows: &[RevisionRow]) -> Result<String> {
    let uri = revision_dataset_path(session)?;
    if rows.is_empty() {
        return Ok(uri);
    }
    let _guard = crate::store::dataset_write_lock::acquire(&uri).await?;
    let batch = batch(rows)?;
    match Dataset::open(&uri).await {
        Ok(dataset) => {
            let reader = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema()));
            MergeInsertBuilder::try_new(Arc::new(dataset), vec!["revision_id".into()])?
                .when_matched(WhenMatched::UpdateAll)
                .when_not_matched(WhenNotMatched::InsertAll)
                .try_build()?
                .execute_reader(reader)
                .await?;
        }
        Err(lance::Error::DatasetNotFound { .. }) => {
            if let Some(parent) = std::path::Path::new(&uri).parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            InsertBuilder::new(&uri).execute(vec![batch]).await?;
        }
        Err(error) => return Err(anyhow::anyhow!(error)).context("open revisions.lance"),
    }
    Ok(uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn revision_catalog_upserts_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let session = StoryCoords::new(dir.path().to_string_lossy(), "a", "s", None);
        let mut row = RevisionRow {
            revision_id: "r1".into(),
            parent_revision_ids: vec![],
            kind: "clean".into(),
            canonical_snapshot: "manifest:1".into(),
            recipe: serde_json::json!({"op":"redact"}),
            status: "building".into(),
            created_at: "2026-08-09T00:00:00Z".into(),
            output_refs: vec![],
        };
        write_revisions(&session, &[row.clone()]).await.unwrap();
        row.status = "ready".into();
        write_revisions(&session, &[row]).await.unwrap();
        let rows = read_revisions(&session).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "ready");
    }
}
