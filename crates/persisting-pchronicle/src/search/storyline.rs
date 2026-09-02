//! FTS search for the normalized Storyline tables.
//!
//! This is the search kernel used by `find` and the Web explorer. It queries a
//! pinned Storyline snapshot, so returned step ids are paired with their
//! owning document ids.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lance::Dataset;
use lance::deps::arrow_array::{Array, Int64Array, StringArray};
use lance::deps::arrow_schema::Schema as ArrowSchema;
use lance::index::DatasetIndexExt;
use lance_index::IndexType;
use lance_index::scalar::InvertedIndexParams;

use crate::storage::StorylineTablePaths;

const DEFAULT_JIEBA_MODEL: &str = "jieba/default";
const DEFAULT_JIEBA_CONFIG: &[u8] = include_bytes!("../../resources/jieba/default/config.json");
const DEFAULT_JIEBA_DICT: &[u8] = include_bytes!("../../resources/jieba/default/dict.txt");

/// Text columns indexed across the normalized Storyline tables. JSON columns
/// are discovered from Arrow metadata and receive a JSON-aware index.
const STORYLINE_FTS_COLUMNS: &[&str] = &[
    "agent_name",
    "agent_version",
    "agent_model_name",
    "agent_tool_definitions",
    "task",
    "prompt",
    "notes",
    "message_value",
    "reasoning_content",
    "model_name",
    "observation",
    "env",
    "function_name",
    "arguments",
    "result",
    "results",
];

/// Default Storyline step columns searched by an unqualified text query.
pub const STORYLINE_STEP_SEARCH_COLUMNS: &[&str] = &[
    "message_value",
    "reasoning_content",
    "model_name",
    "observation",
    "env",
    "prompt",
];

/// Ensure all FTS and JSON search indexes supported by a Storyline table.
pub(crate) async fn ensure_storyline_search_indexes(dataset: &mut Dataset) -> Result<()> {
    let schema = ArrowSchema::from(dataset.schema());
    let has_searchable_columns = schema.fields().iter().any(|field| {
        lance_arrow::json::is_json_field(field)
            || STORYLINE_FTS_COLUMNS
                .iter()
                .any(|column| *column == field.name())
    });
    if !has_searchable_columns {
        return Ok(());
    }
    ensure_default_jieba_model()?;

    for field in schema.fields() {
        if lance_arrow::json::is_json_field(field) {
            ensure_storyline_search_index(dataset, field.name(), Some("json")).await?;
        }
    }
    for column in STORYLINE_FTS_COLUMNS {
        if schema
            .field_with_name(column)
            .is_ok_and(|field| !lance_arrow::json::is_json_field(field))
        {
            ensure_storyline_search_index(dataset, column, None).await?;
        }
    }
    Ok(())
}

fn storyline_inverted_index_params(lance_tokenizer: Option<&str>) -> InvertedIndexParams {
    let params = InvertedIndexParams::default()
        .base_tokenizer(DEFAULT_JIEBA_MODEL.to_string())
        .lower_case(true)
        .stem(false)
        .remove_stop_words(false)
        .ascii_folding(false);
    match lance_tokenizer {
        Some(tokenizer) => params.lance_tokenizer(tokenizer.to_string()),
        None => params,
    }
}

async fn ensure_storyline_search_index(
    dataset: &mut Dataset,
    column: &str,
    lance_tokenizer: Option<&str>,
) -> Result<()> {
    let prefix = if lance_tokenizer == Some("json") {
        "json"
    } else {
        "fts"
    };
    let name = format!("pchronicle_{prefix}_{column}_idx");
    let existing = dataset.load_indices_by_name(&name).await?;
    if !existing.is_empty()
        && existing.iter().all(|metadata| {
            metadata.index_details.as_ref().is_some_and(|details| {
                details
                    .to_msg::<lance_index::pbold::InvertedIndexDetails>()
                    .ok()
                    .is_some_and(|details| {
                        details.lower_case
                            && details.base_tokenizer.as_deref() == Some(DEFAULT_JIEBA_MODEL)
                    })
            })
        })
    {
        return Ok(());
    }
    if !existing.is_empty() {
        dataset.drop_index(&name).await?;
    }
    let params = storyline_inverted_index_params(lance_tokenizer);
    let _admission = crate::store::index_build_gate::acquire().await;
    dataset
        .create_index(&[column], IndexType::Inverted, Some(name), &params, false)
        .await?;
    Ok(())
}

/// Search all default Storyline step text columns.
pub async fn search_storyline_steps_fts(
    paths: &StorylineTablePaths,
    query: &str,
) -> Result<Vec<i64>> {
    Ok(search_storyline_step_matches_fts(paths, query)
        .await?
        .into_iter()
        .map(|(_, step_id)| step_id)
        .collect())
}

/// Search Storyline steps and retain the owning document id for each hit.
pub async fn search_storyline_step_matches_fts(
    paths: &StorylineTablePaths,
    query: &str,
) -> Result<Vec<(String, i64)>> {
    search_storyline_step_matches_fts_in_columns(paths, query, STORYLINE_STEP_SEARCH_COLUMNS).await
}

/// Search Storyline steps through selected indexed text columns.
pub async fn search_storyline_step_matches_fts_in_columns(
    paths: &StorylineTablePaths,
    query: &str,
    columns: &[&str],
) -> Result<Vec<(String, i64)>> {
    let query = query.trim();
    anyhow::ensure!(!query.is_empty(), "Storyline FTS query must not be empty");
    ensure_default_jieba_model()?;

    let dataset = open_table_version(&paths.steps, paths.steps_version).await?;
    let columns = columns
        .iter()
        .copied()
        .filter(|column| dataset.schema().field(column).is_some())
        .map(str::to_string)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !columns.is_empty(),
        "Storyline steps have no searchable text columns"
    );

    let query = lance_index::scalar::FullTextSearchQuery::new(query.to_string())
        .with_columns(&columns)
        .map_err(anyhow::Error::from)?;
    let mut scan = dataset.scan();
    scan.full_text_search(query).map_err(anyhow::Error::from)?;
    scan.project(&["document_id", "step_id"])
        .map_err(anyhow::Error::from)?;
    let batch = scan.try_into_batch().await.map_err(anyhow::Error::from)?;

    let document_ids = batch
        .column_by_name("document_id")
        .context("Storyline FTS result is missing document_id")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Storyline FTS document_id has an unexpected type")?;
    let step_ids = batch
        .column_by_name("step_id")
        .context("Storyline FTS result is missing step_id")?
        .as_any()
        .downcast_ref::<Int64Array>()
        .context("Storyline FTS step_id has an unexpected type")?;

    Ok(document_ids
        .iter()
        .zip(step_ids.values().iter())
        .filter_map(|(document_id, step_id)| document_id.map(|id| (id.to_string(), *step_id)))
        .collect())
}

/// Search Storyline steps and return owning document ids.
pub async fn search_storyline_documents_fts(
    paths: &StorylineTablePaths,
    query: &str,
) -> Result<Vec<String>> {
    let query = query.trim();
    anyhow::ensure!(!query.is_empty(), "Storyline FTS query must not be empty");
    ensure_default_jieba_model()?;

    let dataset = open_table_version(&paths.steps, paths.steps_version).await?;
    let columns = STORYLINE_STEP_SEARCH_COLUMNS
        .iter()
        .copied()
        .filter(|column| dataset.schema().field(column).is_some())
        .map(str::to_string)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !columns.is_empty(),
        "Storyline steps have no searchable text columns"
    );

    let query = lance_index::scalar::FullTextSearchQuery::new(query.to_string())
        .with_columns(&columns)
        .map_err(anyhow::Error::from)?;
    let mut scan = dataset.scan();
    scan.full_text_search(query).map_err(anyhow::Error::from)?;
    scan.project(&["document_id"])
        .map_err(anyhow::Error::from)?;
    let batch = scan.try_into_batch().await.map_err(anyhow::Error::from)?;
    let documents = batch
        .column_by_name("document_id")
        .context("Storyline FTS result is missing document_id")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Storyline FTS document_id has an unexpected type")?;
    Ok(documents.iter().flatten().map(str::to_string).collect())
}

/// Report whether all default Storyline step FTS indexes exist in a snapshot.
pub async fn storyline_steps_fts_available(paths: &StorylineTablePaths) -> Result<bool> {
    ensure_default_jieba_model()?;
    let dataset = open_table_version(&paths.steps, paths.steps_version).await?;
    let columns = STORYLINE_STEP_SEARCH_COLUMNS
        .iter()
        .copied()
        .filter(|column| dataset.schema().field(column).is_some())
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Ok(false);
    }
    let names = dataset
        .load_indices()
        .await?
        .iter()
        .map(|index| index.name.clone())
        .collect::<HashSet<_>>();
    Ok(columns
        .iter()
        .all(|column| names.contains(&format!("pchronicle_fts_{column}_idx"))))
}

async fn open_table_version(path: &Path, version: u64) -> Result<Dataset> {
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline Lance table {}", path.display()))?;
    dataset.checkout_version(version).await.with_context(|| {
        format!(
            "open Storyline Lance table {} at version {version}",
            path.display()
        )
    })
}

fn ensure_default_jieba_model() -> Result<()> {
    let configured_home = std::env::var_os("LANCE_LANGUAGE_MODEL_HOME").map(PathBuf::from);
    let preferred_home = configured_home
        .clone()
        .or_else(dirs::data_local_dir)
        .map(|path| {
            if configured_home.is_some() {
                path
            } else {
                path.join("lance").join("language_models")
            }
        });
    let Some(preferred_home) = preferred_home else {
        return Err(anyhow::anyhow!(
            "cannot determine Lance language model directory"
        ));
    };

    if let Err(error) = materialize_default_jieba_model(&preferred_home) {
        if configured_home.is_some()
            || !matches!(
                error.root_cause().downcast_ref::<std::io::Error>(),
                Some(io_error) if io_error.kind() == std::io::ErrorKind::PermissionDenied
            )
        {
            return Err(error);
        }
        let fallback_home = std::env::temp_dir()
            .join("pchronicle")
            .join("language_models");
        materialize_default_jieba_model(&fallback_home).with_context(|| {
            format!(
                "materialize bundled Jieba model in fallback directory {}",
                fallback_home.display()
            )
        })?;
        // SAFETY: Lance reads this process-wide setting when opening its model.
        unsafe { std::env::set_var("LANCE_LANGUAGE_MODEL_HOME", &fallback_home) };
    }
    Ok(())
}

fn materialize_default_jieba_model(home: &Path) -> Result<()> {
    let model_dir = home.join(DEFAULT_JIEBA_MODEL);
    fs::create_dir_all(&model_dir)
        .with_context(|| format!("create Jieba model directory {}", model_dir.display()))?;

    for (name, contents) in [
        ("config.json", DEFAULT_JIEBA_CONFIG),
        ("dict.txt", DEFAULT_JIEBA_DICT),
    ] {
        let path = model_dir.join(name);
        if path.exists() {
            continue;
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(contents)
                    .with_context(|| format!("write bundled Jieba model {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("flush bundled Jieba model {}", path.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create bundled Jieba model {}", path.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storyline_fts_indexes_default_to_case_insensitive_matching() {
        let params = storyline_inverted_index_params(None);
        let encoded = serde_json::to_value(params).expect("FTS parameters should serialize");
        assert_eq!(encoded["lower_case"], true);
    }
}
