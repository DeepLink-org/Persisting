//! Read-only physical inspection of Catalog Lance sources.

use std::path::Path;

use anyhow::{bail, Context, Result};
use futures::TryStreamExt;
use lance::deps::arrow_array::{
    Array, BinaryArray, BooleanArray, Float64Array, Int32Array, Int64Array, LargeBinaryArray,
    LargeStringArray, StringArray, UInt64Array,
};
use lance::deps::arrow_schema::DataType;
use lance::Dataset;
use serde::Serialize;

use super::catalog::{DatasetCatalogSnapshot, PhysicalOpenTarget};
use super::events::inspect_visible_event_tables;
use super::storyline::StorylineTablePaths;
use super::{CatalogSourceStatus, DiscoveredSource};
use crate::format::DocumentFormat;

pub const DEFAULT_PHYSICAL_PAGE_LIMIT: usize = 32;
pub const MAX_PHYSICAL_COLUMNS: usize = 64;
pub const MAX_PHYSICAL_CELL_BYTES: usize = 4 * 1024;
pub const MAX_PHYSICAL_PREVIEW_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalSource {
    pub dataset: String,
    pub file: String,
    pub format: String,
    pub uri: String,
    pub size_bytes: Option<u64>,
    pub status: CatalogSourceStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalLayout {
    pub dataset: String,
    pub file: String,
    pub format: String,
    pub tables: Vec<PhysicalTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalTable {
    pub name: String,
    pub uri: String,
    pub version: u64,
    pub num_rows: u64,
    pub fragments: Vec<PhysicalFragment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalFragment {
    pub id: u64,
    pub physical_rows: Option<u64>,
    pub deletion_file: Option<String>,
    pub files: Vec<PhysicalDataFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalDataFile {
    pub path: String,
    pub field_ids: Vec<i32>,
    pub field_names: Vec<String>,
    pub size_bytes: Option<u64>,
    pub encoding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalFileLayout {
    pub table: String,
    pub fragment_id: u64,
    pub data_file: String,
    pub num_rows: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub remaining_columns: usize,
    pub columns: Vec<PhysicalColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalColumn {
    pub name: String,
    pub field_id: i32,
    pub pages: Vec<PhysicalPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalPage {
    pub index: u32,
    pub offset: u64,
    pub size: u64,
    pub num_rows: Option<u64>,
    pub encoding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalPagePreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub offset: usize,
    pub limit: usize,
    pub truncated: bool,
    pub truncated_cells: usize,
}

pub fn list_physical_sources(snapshot: &DatasetCatalogSnapshot) -> Vec<PhysicalSource> {
    snapshot
        .datasets()
        .iter()
        .flat_map(|dataset| {
            dataset.sources.iter().filter_map(|source| {
                let format = lance_format(source)?;
                Some(PhysicalSource {
                    dataset: dataset.mount.name.clone(),
                    file: source.file.clone(),
                    format: format.to_string(),
                    uri: join_uri(&dataset.mount.uri, &source.file),
                    size_bytes: source.size_bytes,
                    status: source.status,
                    error: source.error.clone(),
                })
            })
        })
        .collect()
}

pub async fn inspect_physical_layout(
    snapshot: &DatasetCatalogSnapshot,
    dataset: &str,
    file: &str,
) -> Result<PhysicalLayout> {
    let source = require_lance_source(snapshot, dataset, file)?;
    let format = lance_format(source).expect("checked lance source");
    let tables = match snapshot.physical_open_target(dataset, file)? {
        PhysicalOpenTarget::Events { uri } => inspect_event_tables(&uri).await?,
        PhysicalOpenTarget::Storyline { paths } => inspect_storyline_tables(&paths).await?,
    };
    Ok(PhysicalLayout {
        dataset: dataset.to_string(),
        file: file.to_string(),
        format: format.to_string(),
        tables,
    })
}

pub async fn inspect_physical_file(
    snapshot: &DatasetCatalogSnapshot,
    dataset: &str,
    file: &str,
    table: &str,
    fragment_id: u64,
    data_file: &str,
) -> Result<PhysicalFileLayout> {
    let layout = inspect_physical_layout(snapshot, dataset, file).await?;
    let table_layout = layout
        .tables
        .iter()
        .find(|candidate| candidate.name == table)
        .with_context(|| format!("physical table not found: {table}"))?;
    let fragment = table_layout
        .fragments
        .iter()
        .find(|candidate| candidate.id == fragment_id)
        .with_context(|| format!("physical fragment not found: {fragment_id}"))?;
    let data = fragment
        .files
        .iter()
        .find(|candidate| candidate.path == data_file)
        .with_context(|| format!("physical data file not found: {data_file}"))?;
    let remaining_columns = data.field_names.len().saturating_sub(MAX_PHYSICAL_COLUMNS);
    let columns = data
        .field_ids
        .iter()
        .zip(&data.field_names)
        .take(MAX_PHYSICAL_COLUMNS)
        .map(|(field_id, name)| PhysicalColumn {
            name: name.clone(),
            field_id: *field_id,
            pages: vec![PhysicalPage {
                index: 0,
                offset: 0,
                size: data.size_bytes.unwrap_or(0),
                num_rows: fragment.physical_rows,
                encoding: data.encoding.clone(),
            }],
        })
        .collect();
    Ok(PhysicalFileLayout {
        table: table.to_string(),
        fragment_id,
        data_file: data_file.to_string(),
        num_rows: fragment.physical_rows,
        file_size_bytes: data.size_bytes,
        remaining_columns,
        columns,
    })
}

pub async fn inspect_physical_page(
    snapshot: &DatasetCatalogSnapshot,
    dataset: &str,
    file: &str,
    table: &str,
    fragment_id: u64,
    data_file: &str,
    column: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<PhysicalPagePreview> {
    let file_layout =
        inspect_physical_file(snapshot, dataset, file, table, fragment_id, data_file).await?;
    let columns = match column {
        Some(name) => vec![name.to_string()],
        None => file_layout
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    };
    anyhow::ensure!(!columns.is_empty(), "physical data file has no columns");
    let limit = limit.clamp(1, DEFAULT_PHYSICAL_PAGE_LIMIT);
    let uri = table_uri(snapshot, dataset, file, table).await?;
    let lance = Dataset::open(&uri)
        .await
        .with_context(|| format!("open Lance table {uri}"))?;
    preview_rows(&lance, &columns, offset, limit).await
}

fn lance_format(source: &DiscoveredSource) -> Option<&str> {
    let format = source.format.as_deref()?;
    (format == DocumentFormat::CanonicalEvent.as_str()
        || format == DocumentFormat::StorylineLance.as_str())
    .then_some(format)
}

fn require_lance_source<'a>(
    snapshot: &'a DatasetCatalogSnapshot,
    dataset: &str,
    file: &str,
) -> Result<&'a DiscoveredSource> {
    let catalog = snapshot
        .dataset(dataset)
        .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
    let source = catalog
        .sources
        .iter()
        .find(|source| source.file == file)
        .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
    if lance_format(source).is_none() {
        bail!("physical source is not a Lance dataset: {dataset}/{file}");
    }
    Ok(source)
}

fn join_uri(mount: &str, file: &str) -> String {
    let mount = mount.trim_end_matches('/');
    if file == "." {
        mount.to_string()
    } else {
        format!("{mount}/{file}")
    }
}

async fn table_uri(
    snapshot: &DatasetCatalogSnapshot,
    dataset: &str,
    file: &str,
    table: &str,
) -> Result<String> {
    match snapshot.physical_open_target(dataset, file)? {
        PhysicalOpenTarget::Events { uri } => {
            let tables = inspect_visible_event_tables(&uri).await?;
            tables
                .into_iter()
                .find(|(name, _)| name == table)
                .map(|(_, dataset)| dataset.uri().to_string())
                .with_context(|| format!("physical table not found: {table}"))
        }
        PhysicalOpenTarget::Storyline { paths } => storyline_table_path(&paths, table)
            .map(|path| path.to_string_lossy().into_owned())
            .with_context(|| format!("physical table not found: {table}")),
    }
}

async fn inspect_event_tables(uri: &str) -> Result<Vec<PhysicalTable>> {
    let mut tables = Vec::new();
    for (name, dataset) in inspect_visible_event_tables(uri).await? {
        tables.push(physical_table(&name, dataset).await?);
    }
    Ok(tables)
}

async fn inspect_storyline_tables(paths: &StorylineTablePaths) -> Result<Vec<PhysicalTable>> {
    let mut tables = Vec::new();
    for (name, path, version) in [
        ("runs", paths.runs.as_path(), paths.runs_version),
        ("steps", paths.steps.as_path(), paths.steps_version),
        (
            "tool_calls",
            paths.tool_calls.as_path(),
            paths.tool_calls_version,
        ),
        ("objects", paths.objects.as_path(), paths.objects_version),
    ] {
        let dataset = open_table_version(path, version).await?;
        tables.push(physical_table(name, dataset).await?);
    }
    Ok(tables)
}

fn storyline_table_path<'a>(paths: &'a StorylineTablePaths, table: &str) -> Option<&'a Path> {
    match table {
        "runs" => Some(paths.runs.as_path()),
        "steps" => Some(paths.steps.as_path()),
        "tool_calls" => Some(paths.tool_calls.as_path()),
        "objects" => Some(paths.objects.as_path()),
        _ => None,
    }
}

async fn open_table_version(path: &Path, version: u64) -> Result<Dataset> {
    let uri = path.to_string_lossy();
    let dataset = Dataset::open(uri.as_ref())
        .await
        .with_context(|| format!("open Lance table {uri}"))?;
    dataset
        .checkout_version(version)
        .await
        .with_context(|| format!("open Lance table {uri} at version {version}"))
}

async fn physical_table(name: &str, dataset: Dataset) -> Result<PhysicalTable> {
    let schema = dataset.schema().clone();
    let fragments = dataset
        .get_fragments()
        .into_iter()
        .map(|fragment| {
            let meta = fragment.metadata();
            PhysicalFragment {
                id: meta.id,
                physical_rows: meta.physical_rows.map(|rows| rows as u64),
                deletion_file: meta.deletion_file.as_ref().map(|file| format!("{file:?}")),
                files: meta
                    .files
                    .iter()
                    .map(|file| PhysicalDataFile {
                        path: file.path.clone(),
                        field_ids: file.fields.to_vec(),
                        field_names: file
                            .fields
                            .iter()
                            .filter_map(|field_id| {
                                schema
                                    .field_by_id(*field_id)
                                    .map(|field| field.name.clone())
                            })
                            .collect(),
                        size_bytes: file.file_size_bytes.get().map(|value| value.get()),
                        encoding: format!(
                            "lance-{}.{}",
                            file.file_major_version, file.file_minor_version
                        ),
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(PhysicalTable {
        name: name.to_string(),
        uri: dataset.uri().to_string(),
        version: dataset.version_id(),
        num_rows: dataset.count_rows(None).await? as u64,
        fragments,
    })
}

async fn preview_rows(
    dataset: &Dataset,
    columns: &[String],
    offset: usize,
    limit: usize,
) -> Result<PhysicalPagePreview> {
    let mut scan = dataset.scan();
    scan.project(columns)
        .with_context(|| format!("project physical columns {}", columns.join(",")))?;
    scan.limit(Some(limit as i64), (offset > 0).then_some(offset as i64))
        .context("apply physical preview offset/limit")?;
    let batches = scan
        .try_into_stream()
        .await
        .context("scan physical preview")?
        .try_collect::<Vec<_>>()
        .await
        .context("collect physical preview")?;
    let mut rows = Vec::new();
    let mut truncated_cells = 0;
    let mut total_bytes = 0;
    let mut truncated = false;
    'batches: for batch in &batches {
        for row in 0..batch.num_rows() {
            let mut cells = Vec::with_capacity(batch.num_columns());
            for column in 0..batch.num_columns() {
                let (value, cell_truncated) = format_cell(batch.column(column).as_ref(), row);
                if cell_truncated {
                    truncated_cells += 1;
                }
                total_bytes += value.len();
                cells.push(value);
                if total_bytes >= MAX_PHYSICAL_PREVIEW_BYTES {
                    truncated = true;
                    rows.push(cells);
                    break 'batches;
                }
            }
            rows.push(cells);
        }
    }
    Ok(PhysicalPagePreview {
        columns: columns.to_vec(),
        rows,
        offset,
        limit,
        truncated,
        truncated_cells,
    })
}

fn format_cell(array: &dyn Array, row: usize) -> (String, bool) {
    if array.is_null(row) {
        return ("null".into(), false);
    }
    let raw = match array.data_type() {
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|values| values.value(row).to_string()),
        DataType::LargeUtf8 => array
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|values| values.value(row).to_string()),
        DataType::Binary => array.as_any().downcast_ref::<BinaryArray>().map(|values| {
            let bytes = values.value(row);
            format!("bytes={} digest={}", bytes.len(), short_digest(bytes))
        }),
        DataType::LargeBinary => array
            .as_any()
            .downcast_ref::<LargeBinaryArray>()
            .map(|values| {
                let bytes = values.value(row);
                format!("bytes={} digest={}", bytes.len(), short_digest(bytes))
            }),
        DataType::Int32 => array
            .as_any()
            .downcast_ref::<Int32Array>()
            .map(|values| values.value(row).to_string()),
        DataType::Int64 => array
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(|values| values.value(row).to_string()),
        DataType::UInt64 => array
            .as_any()
            .downcast_ref::<UInt64Array>()
            .map(|values| values.value(row).to_string()),
        DataType::Float64 => array
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|values| values.value(row).to_string()),
        DataType::Boolean => array
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map(|values| values.value(row).to_string()),
        _ => Some(format!("{:?}", array.slice(row, 1))),
    }
    .unwrap_or_else(|| format!("<{}>", array.data_type()));
    truncate_cell(&raw)
}

fn truncate_cell(value: &str) -> (String, bool) {
    if value.len() <= MAX_PHYSICAL_CELL_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_PHYSICAL_CELL_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &value[..end]), true)
}

fn short_digest(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        CatalogSnapshotOptions, DatasetCatalogSnapshot, DatasetMount, StorylineLanceStore,
        DEFAULT_DATASET_NAME,
    };
    use crate::{StorylineAgent, StorylineDocument, StorylineTurn};

    fn storyline(session_id: &str, run_id: &str) -> StorylineDocument {
        StorylineDocument {
            schema_version: crate::model::STORYLINE_SCHEMA_VERSION.into(),
            origin: None,
            run_id: Some(run_id.into()),
            trajectory_id: None,
            attempt_id: None,
            session_id: session_id.into(),
            agent: StorylineAgent {
                id: "agent".into(),
                name: None,
                version: None,
                model_name: Some("model".into()),
                tool_definitions: None,
                extra: None,
            },
            parent: None,
            child_session_ids: None,
            notes: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            meta: None,
            task: None,
            prompt: None,
            started_at: None,
            finished_at: None,
            unknown_fields: Default::default(),
            unknown_key_counts: Default::default(),
            turns: vec![StorylineTurn {
                id: 1,
                kind: None,
                timestamp: None,
                source: "user".into(),
                message: serde_json::json!("hello"),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            }],
        }
    }

    #[tokio::test]
    async fn inspects_storyline_lance_layout_and_page_preview() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = StorylineLanceStore::open(temp.path().join("story")).await?;
        store
            .replace_storyline(&storyline("session-a", "run-a"))
            .await?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let sources = list_physical_sources(&snapshot);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].file, "story");
        assert_eq!(sources[0].format, DocumentFormat::StorylineLance.as_str());

        let layout = inspect_physical_layout(&snapshot, DEFAULT_DATASET_NAME, "story").await?;
        let runs = layout
            .tables
            .iter()
            .find(|table| table.name == "runs")
            .expect("runs table");
        assert!(runs.num_rows >= 1);
        let fragment = runs.fragments.first().expect("fragment");
        let data_file = fragment.files.first().expect("data file");
        assert!(!data_file.field_names.is_empty());

        let file = inspect_physical_file(
            &snapshot,
            DEFAULT_DATASET_NAME,
            "story",
            "runs",
            fragment.id,
            &data_file.path,
        )
        .await?;
        assert_eq!(
            file.columns.len(),
            data_file.field_names.len().min(MAX_PHYSICAL_COLUMNS)
        );
        assert_eq!(file.columns[0].pages.len(), 1);

        let preview = inspect_physical_page(
            &snapshot,
            DEFAULT_DATASET_NAME,
            "story",
            "runs",
            fragment.id,
            &data_file.path,
            Some("session_id"),
            0,
            8,
        )
        .await?;
        assert_eq!(preview.columns, vec!["session_id".to_string()]);
        assert!(preview
            .rows
            .iter()
            .any(|row| row.contains(&"session-a".to_string())));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_non_lance_catalog_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("gateway.json"),
            r#"[{"id":"event-1","session_id":"s","step_id":1,"agent_model":"m","messages":[{"role":"user","content":"hi"}],"response":{"role":"assistant","content":"yo"}}]"#,
        )?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        assert!(list_physical_sources(&snapshot).is_empty());
        let error = inspect_physical_layout(&snapshot, DEFAULT_DATASET_NAME, "gateway.json")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("not a Lance dataset"),
            "{error:#}"
        );
        Ok(())
    }
}
