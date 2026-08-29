//! Queryable lineage catalog for derived trajectory artifacts.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::{InsertBuilder, MergeInsertBuilder, WhenMatched, WhenNotMatched};
use lance::deps::arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::layout::StoryCoords;

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
    let parent_revision_ids = rows
        .iter()
        .map(|row| serde_json::to_string(&row.parent_revision_ids))
        .collect::<serde_json::Result<Vec<_>>>()?;
    let output_refs = rows
        .iter()
        .map(|row| serde_json::to_string(&row.output_refs))
        .collect::<serde_json::Result<Vec<_>>>()?;
    RecordBatch::try_new(
        schema(),
        vec![
            strings(rows.iter().map(|r| r.revision_id.clone()).collect()),
            strings(parent_revision_ids),
            strings(rows.iter().map(|r| r.kind.clone()).collect()),
            strings(rows.iter().map(|r| r.canonical_snapshot.clone()).collect()),
            strings(rows.iter().map(|r| r.recipe.to_string()).collect()),
            strings(rows.iter().map(|r| r.status.clone()).collect()),
            strings(rows.iter().map(|r| r.created_at.clone()).collect()),
            strings(output_refs),
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

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;
        use serde_json::Value;

        use crate::revision::{RevisionRow, batch, text};

        fn token_strategy() -> impl Strategy<Value = String> {
            proptest::string::string_regex("[a-zA-Z0-9._:/-]{0,32}").unwrap()
        }

        fn recipe_strategy() -> impl Strategy<Value = Value> {
            prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                (0u64..100_000).prop_map(Value::from),
                token_strategy().prop_map(Value::String),
                token_strategy().prop_map(|value| serde_json::json!({"op": value})),
            ]
        }

        fn revision_row_strategy() -> impl Strategy<Value = RevisionRow> {
            (
                token_strategy(),
                proptest::collection::vec(token_strategy(), 0..4),
                token_strategy(),
                token_strategy(),
                recipe_strategy(),
                token_strategy(),
                token_strategy(),
                proptest::collection::vec(token_strategy(), 0..4),
            )
                .prop_map(
                    |(
                        revision_id,
                        parent_revision_ids,
                        kind,
                        canonical_snapshot,
                        recipe,
                        status,
                        created_at,
                        output_refs,
                    )| RevisionRow {
                        revision_id,
                        parent_revision_ids,
                        kind,
                        canonical_snapshot,
                        recipe,
                        status,
                        created_at,
                        output_refs,
                    },
                )
        }

        proptest! {
            #[test]
            fn revision_batches_preserve_all_columns(rows in proptest::collection::vec(revision_row_strategy(), 0..12)) {
                let record_batch = batch(&rows).unwrap();
                prop_assert_eq!(record_batch.num_rows(), rows.len());
                for (index, expected) in rows.iter().enumerate() {
                    prop_assert_eq!(text(&record_batch, "revision_id", index).unwrap(), expected.revision_id.clone());
                    prop_assert_eq!(serde_json::from_str::<Vec<String>>(&text(&record_batch, "parent_revision_ids_json", index).unwrap()).unwrap(), expected.parent_revision_ids.clone());
                    prop_assert_eq!(text(&record_batch, "kind", index).unwrap(), expected.kind.clone());
                    prop_assert_eq!(text(&record_batch, "canonical_snapshot", index).unwrap(), expected.canonical_snapshot.clone());
                    prop_assert_eq!(serde_json::from_str::<Value>(&text(&record_batch, "recipe_json", index).unwrap()).unwrap(), expected.recipe.clone());
                    prop_assert_eq!(text(&record_batch, "status", index).unwrap(), expected.status.clone());
                    prop_assert_eq!(text(&record_batch, "created_at", index).unwrap(), expected.created_at.clone());
                    prop_assert_eq!(serde_json::from_str::<Vec<String>>(&text(&record_batch, "output_refs_json", index).unwrap()).unwrap(), expected.output_refs.clone());
                }
            }

            #[test]
            fn revision_batch_preserves_unicode_and_delimiters(
                id in any::<String>(),
                snapshot in any::<String>(),
                status in any::<String>(),
            ) {
                let row = RevisionRow { revision_id: id.clone(), parent_revision_ids: vec!["父/parent".into()], kind: "kind\nline".into(), canonical_snapshot: snapshot.clone(), recipe: serde_json::json!({"文本": status.clone()}), status, created_at: "2026-01-01T00:00:00Z".into(), output_refs: vec!["ref\nwith\nnewline".into()] };
                let record_batch = batch(&[row]).unwrap();
                prop_assert_eq!(text(&record_batch, "revision_id", 0).unwrap(), id);
                prop_assert_eq!(text(&record_batch, "canonical_snapshot", 0).unwrap(), snapshot);
            }
        }
    }
}
