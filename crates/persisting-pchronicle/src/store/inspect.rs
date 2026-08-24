//! Read-only physical inspection of Catalog Lance sources.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::statistics::DatasetStatisticsExt;
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
const MAX_PHYSICAL_STATS_ROWS: usize = 100_000;
const MAX_VALUE_BUCKETS: usize = 8;
const MAX_TRACKED_VALUES: usize = 64;
const SIZE_BUCKET_LABELS: [&str; 8] = [
    "0 B",
    "1–8 B",
    "9–64 B",
    "65–256 B",
    "257 B–1 KB",
    "1–4 KB",
    "4–16 KB",
    ">16 KB",
];

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
    pub size_bytes: Option<u64>,
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
    #[serde(default)]
    pub data_type: String,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default)]
    pub null_count: u64,
    #[serde(default)]
    pub non_null_count: u64,
    #[serde(default)]
    pub compressed_bytes: Option<u64>,
    #[serde(default)]
    pub uncompressed_bytes: Option<u64>,
    #[serde(default)]
    pub max_value: Option<PhysicalExtremeValue>,
    #[serde(default)]
    pub value_distribution: Vec<PhysicalBucket>,
    #[serde(default)]
    pub size_distribution: Vec<PhysicalBucket>,
    pub pages: Vec<PhysicalPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalExtremeValue {
    pub row_offset: u64,
    pub size_bytes: u64,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalBucket {
    pub label: String,
    pub count: u64,
    pub weight: u64,
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
    let format = require_lance_source(snapshot, dataset, file)?;
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
    let mut columns = data
        .field_ids
        .iter()
        .zip(&data.field_names)
        .take(MAX_PHYSICAL_COLUMNS)
        .map(|(field_id, name)| PhysicalColumn {
            name: name.clone(),
            field_id: *field_id,
            data_type: String::new(),
            row_count: fragment.physical_rows.unwrap_or(0),
            null_count: 0,
            non_null_count: 0,
            compressed_bytes: None,
            uncompressed_bytes: None,
            max_value: None,
            value_distribution: Vec::new(),
            size_distribution: Vec::new(),
            pages: vec![PhysicalPage {
                index: 0,
                offset: 0,
                size: data.size_bytes.unwrap_or(0),
                num_rows: fragment.physical_rows,
                encoding: data.encoding.clone(),
            }],
        })
        .collect::<Vec<_>>();
    if let Ok(uri) = table_uri(snapshot, dataset, file, table).await {
        if let Ok(lance) = Dataset::open(&uri).await {
            enrich_column_stats(&lance, fragment_id, &mut columns).await;
        }
    }
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

#[derive(Debug, Clone, Copy)]
pub struct PhysicalPageQuery<'a> {
    pub dataset: &'a str,
    pub file: &'a str,
    pub table: &'a str,
    pub fragment_id: u64,
    pub data_file: &'a str,
    pub column: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

pub async fn inspect_physical_page(
    snapshot: &DatasetCatalogSnapshot,
    query: PhysicalPageQuery<'_>,
) -> Result<PhysicalPagePreview> {
    let layout = inspect_physical_layout(snapshot, query.dataset, query.file).await?;
    let table_layout = layout
        .tables
        .iter()
        .find(|candidate| candidate.name == query.table)
        .with_context(|| format!("physical table not found: {}", query.table))?;
    let fragment = table_layout
        .fragments
        .iter()
        .find(|candidate| candidate.id == query.fragment_id)
        .with_context(|| format!("physical fragment not found: {}", query.fragment_id))?;
    let data = fragment
        .files
        .iter()
        .find(|candidate| candidate.path == query.data_file)
        .with_context(|| format!("physical data file not found: {}", query.data_file))?;
    let columns = match query.column {
        Some(name) => vec![name.to_string()],
        None => data
            .field_names
            .iter()
            .take(MAX_PHYSICAL_COLUMNS)
            .cloned()
            .collect(),
    };
    anyhow::ensure!(!columns.is_empty(), "physical data file has no columns");
    let limit = query.limit.clamp(1, DEFAULT_PHYSICAL_PAGE_LIMIT);
    let uri = table_uri(snapshot, query.dataset, query.file, query.table).await?;
    let lance = Dataset::open(&uri)
        .await
        .with_context(|| format!("open Lance table {uri}"))?;
    preview_rows(&lance, &columns, query.offset, limit).await
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
) -> Result<&'a str> {
    let catalog = snapshot
        .dataset(dataset)
        .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
    let source = catalog
        .sources
        .iter()
        .find(|source| source.file == file)
        .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
    lance_format(source)
        .with_context(|| format!("physical source is not a Lance dataset: {dataset}/{file}"))
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
            let files = meta
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
                .collect::<Vec<_>>();
            PhysicalFragment {
                id: meta.id,
                physical_rows: meta.physical_rows.map(|rows| rows as u64),
                size_bytes: sum_known_sizes(files.iter().map(|file| file.size_bytes)),
                deletion_file: meta.deletion_file.as_ref().map(|file| format!("{file:?}")),
                files,
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

fn sum_known_sizes(sizes: impl IntoIterator<Item = Option<u64>>) -> Option<u64> {
    let sizes = sizes.into_iter().flatten().collect::<Vec<_>>();
    (!sizes.is_empty()).then_some(sizes.into_iter().sum())
}

struct ColumnScratch {
    data_type: String,
    row_count: u64,
    null_count: u64,
    uncompressed_bytes: u64,
    values: HashMap<String, u64>,
    extra_values: u64,
    size_counts: [u64; 8],
    max_size: u64,
    max_row: u64,
    max_preview: String,
}

impl ColumnScratch {
    fn new(data_type: impl Into<String>) -> Self {
        Self {
            data_type: data_type.into(),
            row_count: 0,
            null_count: 0,
            uncompressed_bytes: 0,
            values: HashMap::new(),
            extra_values: 0,
            size_counts: [0; 8],
            max_size: 0,
            max_row: 0,
            max_preview: String::new(),
        }
    }

    fn observe(&mut self, array: &dyn Array, row: usize, row_offset: u64) {
        self.row_count += 1;
        if array.is_null(row) {
            self.null_count += 1;
            self.size_counts[0] += 1;
            return;
        }
        let size = cell_uncompressed_bytes(array, row);
        self.uncompressed_bytes += size;
        self.size_counts[size_bucket(size)] += 1;
        let (preview, _) = format_cell(array, row);
        if self.values.len() < MAX_TRACKED_VALUES || self.values.contains_key(&preview) {
            *self.values.entry(preview.clone()).or_insert(0) += 1;
        } else {
            self.extra_values += 1;
        }
        if size > self.max_size {
            self.max_size = size;
            self.max_row = row_offset;
            self.max_preview = preview;
        }
    }

    fn finish(self, compressed_bytes: Option<u64>) -> FinishedColumnStats {
        let mut values = self.values.into_iter().collect::<Vec<_>>();
        values.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let mut other = self.extra_values;
        if values.len() > MAX_VALUE_BUCKETS {
            other += values
                .iter()
                .skip(MAX_VALUE_BUCKETS)
                .map(|item| item.1)
                .sum::<u64>();
            values.truncate(MAX_VALUE_BUCKETS);
        }
        let mut value_distribution = values
            .into_iter()
            .map(|(label, count)| PhysicalBucket {
                label,
                count,
                weight: count,
            })
            .collect::<Vec<_>>();
        if other > 0 {
            value_distribution.push(PhysicalBucket {
                label: "other".into(),
                count: other,
                weight: other,
            });
        }
        let size_distribution = self
            .size_counts
            .into_iter()
            .enumerate()
            .filter(|(_, count)| *count > 0)
            .map(|(index, count)| PhysicalBucket {
                label: SIZE_BUCKET_LABELS[index].into(),
                count,
                weight: count,
            })
            .collect();
        FinishedColumnStats {
            data_type: self.data_type,
            row_count: self.row_count,
            null_count: self.null_count,
            non_null_count: self.row_count.saturating_sub(self.null_count),
            compressed_bytes,
            uncompressed_bytes: Some(self.uncompressed_bytes),
            max_value: (self.max_size > 0).then_some(PhysicalExtremeValue {
                row_offset: self.max_row,
                size_bytes: self.max_size,
                preview: self.max_preview,
            }),
            value_distribution,
            size_distribution,
        }
    }
}

struct FinishedColumnStats {
    data_type: String,
    row_count: u64,
    null_count: u64,
    non_null_count: u64,
    compressed_bytes: Option<u64>,
    uncompressed_bytes: Option<u64>,
    max_value: Option<PhysicalExtremeValue>,
    value_distribution: Vec<PhysicalBucket>,
    size_distribution: Vec<PhysicalBucket>,
}

fn size_bucket(bytes: u64) -> usize {
    match bytes {
        0 => 0,
        1..=8 => 1,
        9..=64 => 2,
        65..=256 => 3,
        257..=1024 => 4,
        1025..=4096 => 5,
        4097..=16384 => 6,
        _ => 7,
    }
}

fn cell_uncompressed_bytes(array: &dyn Array, row: usize) -> u64 {
    match array.data_type() {
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|values| values.value(row).len() as u64)
            .unwrap_or(0),
        DataType::LargeUtf8 => array
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(|values| values.value(row).len() as u64)
            .unwrap_or(0),
        DataType::Binary => array
            .as_any()
            .downcast_ref::<BinaryArray>()
            .map(|values| values.value(row).len() as u64)
            .unwrap_or(0),
        DataType::LargeBinary => array
            .as_any()
            .downcast_ref::<LargeBinaryArray>()
            .map(|values| values.value(row).len() as u64)
            .unwrap_or(0),
        DataType::Boolean => 1,
        DataType::Int32 => 4,
        DataType::Int64 | DataType::UInt64 | DataType::Float64 => 8,
        _ => format_cell(array, row).0.len() as u64,
    }
}

async fn compressed_bytes_by_field(dataset: &Dataset) -> HashMap<i32, u64> {
    if dataset.get_fragments().len() != 1 {
        return HashMap::new();
    }
    let Ok(stats) = Arc::new(dataset.clone()).calculate_data_stats().await else {
        return HashMap::new();
    };
    stats
        .fields
        .into_iter()
        .filter(|field| field.bytes_on_disk > 0)
        .map(|field| (field.id as i32, field.bytes_on_disk))
        .collect()
}

async fn enrich_column_stats(dataset: &Dataset, fragment_id: u64, columns: &mut [PhysicalColumn]) {
    if columns.is_empty() {
        return;
    }
    let compressed = compressed_bytes_by_field(dataset).await;
    let names = columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let mut scan = dataset.scan();
    if scan.project(&names).is_err() {
        return;
    }
    if let Some(fragment) = dataset.get_fragment(fragment_id as usize) {
        scan.with_fragments(vec![fragment.metadata().clone()]);
    }
    if scan
        .limit(Some(MAX_PHYSICAL_STATS_ROWS as i64), None)
        .is_err()
    {
        return;
    }
    let Ok(stream) = scan.try_into_stream().await else {
        return;
    };
    let Ok(batches) = stream.try_collect::<Vec<_>>().await else {
        return;
    };
    let mut scratches = columns
        .iter()
        .map(|column| ColumnScratch::new(column.data_type.clone()))
        .collect::<Vec<_>>();
    let mut row_offset = 0u64;
    for batch in &batches {
        for (index, scratch) in scratches.iter_mut().enumerate() {
            if scratch.data_type.is_empty() {
                scratch.data_type = batch.schema().field(index).data_type().to_string();
            }
        }
        for row in 0..batch.num_rows() {
            for (index, scratch) in scratches.iter_mut().enumerate() {
                scratch.observe(batch.column(index).as_ref(), row, row_offset);
            }
            row_offset += 1;
        }
    }
    for (column, scratch) in columns.iter_mut().zip(scratches) {
        let stats = scratch.finish(compressed.get(&column.field_id).copied());
        column.data_type = stats.data_type;
        column.row_count = stats.row_count;
        column.null_count = stats.null_count;
        column.non_null_count = stats.non_null_count;
        column.compressed_bytes = stats.compressed_bytes;
        column.uncompressed_bytes = stats.uncompressed_bytes;
        column.max_value = stats.max_value;
        column.value_distribution = stats.value_distribution;
        column.size_distribution = stats.size_distribution;
    }
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
        assert_eq!(runs.fragments.len(), 1);
        let fragment = runs.fragments.first().expect("fragment");
        assert!(fragment.physical_rows.unwrap_or(0) >= 1);
        assert!(fragment.size_bytes.unwrap_or(0) > 0);
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
        let session_id = file
            .columns
            .iter()
            .find(|column| column.name == "session_id")
            .expect("session_id column");
        assert!(session_id.row_count >= 1);
        assert!(session_id.non_null_count >= 1);
        assert!(session_id.uncompressed_bytes.unwrap_or(0) > 0);
        assert!(!session_id.value_distribution.is_empty());
        assert!(session_id
            .value_distribution
            .iter()
            .any(|bucket| bucket.label.contains("session-a")));
        assert!(session_id.max_value.is_some());

        let preview = inspect_physical_page(
            &snapshot,
            PhysicalPageQuery {
                dataset: DEFAULT_DATASET_NAME,
                file: "story",
                table: "runs",
                fragment_id: fragment.id,
                data_file: &data_file.path,
                column: Some("session_id"),
                offset: 0,
                limit: 8,
            },
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

    #[test]
    fn sum_known_sizes_ignores_unknown_and_returns_none_when_empty() {
        assert_eq!(sum_known_sizes([None, None]), None);
        assert_eq!(sum_known_sizes([Some(2), None, Some(3)]), Some(5));
    }
}
