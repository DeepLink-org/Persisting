//! Content-addressed storage for large Storyline cells.
//!
//! The normalized Storyline schemas remain unchanged. Large UTF-8 / JSON cells
//! are replaced internally with a compact, versioned descriptor and stored once
//! in `objects.lance`. Public pChronicle readers hydrate descriptors before they
//! return data.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{
    Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray, UInt64Array, UInt8Array,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, SchemaRef};
use lance::index::DatasetIndexExt;
use lance::{blob_field_with_options, BlobArrayBuilder, BlobFieldOptions, Dataset};
use lance_file::version::LanceFileVersion;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};
use lance_index::IndexType;

use super::storyline_datafusion::StorylineTableKind;

pub const STORYLINE_OBJECTS_DATASET: &str = "objects.lance";
pub const DEFAULT_CONTENT_OFFLOAD_THRESHOLD: usize = 64 * 1024;
pub const DEFAULT_CONTENT_PREVIEW_BYTES: usize = 256;
pub(crate) const CONTENT_REF_MAGIC: &str = "\u{001e}PCHRONICLE-CONTENT-1:";
const CONTENT_INDEX_NAME: &str = "pchronicle_content_id_idx";
const CONTENT_ID_COLUMN: &str = "content_id";
const PAYLOAD_COLUMN: &str = "payload";
const ROW_ADDRESS_COLUMN: &str = "_rowaddr";
const LOOKUP_CHUNK_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorylineContentOptions {
    /// Serialized cell size at which content is moved to `objects.lance`.
    pub offload_threshold: usize,
    /// Maximum number of UTF-8 bytes copied into the descriptor preview.
    pub preview_bytes: usize,
    /// Zstd compression level for UTF-8 and JSON content.
    pub zstd_level: i32,
}

impl Default for StorylineContentOptions {
    fn default() -> Self {
        Self {
            offload_threshold: DEFAULT_CONTENT_OFFLOAD_THRESHOLD,
            preview_bytes: DEFAULT_CONTENT_PREVIEW_BYTES,
            zstd_level: 3,
        }
    }
}

impl StorylineContentOptions {
    pub(crate) fn validate(self) -> Result<Self> {
        anyhow::ensure!(
            self.offload_threshold > 0,
            "Storyline content offload threshold must be greater than zero"
        );
        anyhow::ensure!(
            self.preview_bytes <= 4096,
            "Storyline content preview must not exceed 4096 bytes"
        );
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum LogicalType {
    Utf8 = 1,
    Json = 2,
    Binary = 3,
}

impl LogicalType {
    fn encode(self) -> &'static str {
        match self {
            Self::Utf8 => "u",
            Self::Json => "j",
            Self::Binary => "b",
        }
    }

    fn decode(value: &str) -> Result<Self> {
        match value {
            "u" => Ok(Self::Utf8),
            "j" => Ok(Self::Json),
            "b" => Ok(Self::Binary),
            _ => anyhow::bail!("unknown pChronicle content logical type '{value}'"),
        }
    }

    fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Utf8),
            2 => Ok(Self::Json),
            3 => Ok(Self::Binary),
            _ => anyhow::bail!("unknown pChronicle content logical type {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ContentCodec {
    Identity = 0,
    Zstd = 1,
}

impl ContentCodec {
    fn encode(self) -> &'static str {
        match self {
            Self::Identity => "i",
            Self::Zstd => "z",
        }
    }

    fn decode(value: &str) -> Result<Self> {
        match value {
            "i" => Ok(Self::Identity),
            "z" => Ok(Self::Zstd),
            _ => anyhow::bail!("unknown pChronicle content codec '{value}'"),
        }
    }

    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Identity),
            1 => Ok(Self::Zstd),
            _ => anyhow::bail!("unknown pChronicle content codec {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentRef {
    logical_type: LogicalType,
    codec: ContentCodec,
    content_id: String,
    raw_length: u64,
    preview: String,
}

impl ContentRef {
    fn encode(&self) -> String {
        format!(
            "{CONTENT_REF_MAGIC}{}:{}:{}:{}:{}",
            self.logical_type.encode(),
            self.codec.encode(),
            self.content_id,
            self.raw_length,
            URL_SAFE_NO_PAD.encode(self.preview.as_bytes())
        )
    }

    fn parse(value: &str) -> Result<Option<Self>> {
        let Some(encoded) = value.strip_prefix(CONTENT_REF_MAGIC) else {
            return Ok(None);
        };
        let mut fields = encoded.split(':');
        let logical_type = LogicalType::decode(fields.next().context("missing logical type")?)?;
        let codec = ContentCodec::decode(fields.next().context("missing codec")?)?;
        let content_id = fields.next().context("missing content id")?.to_string();
        anyhow::ensure!(
            content_id.len() == 64 && content_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid BLAKE3 content id"
        );
        let raw_length = fields
            .next()
            .context("missing raw length")?
            .parse::<u64>()
            .context("invalid raw length")?;
        let preview = String::from_utf8(
            URL_SAFE_NO_PAD
                .decode(fields.next().context("missing preview")?)
                .context("invalid content preview")?,
        )
        .context("content preview is not UTF-8")?;
        anyhow::ensure!(
            fields.next().is_none(),
            "unexpected content descriptor fields"
        );
        Ok(Some(Self {
            logical_type,
            codec,
            content_id,
            raw_length,
            preview,
        }))
    }
}

#[derive(Debug, Clone)]
struct ContentObject {
    reference: ContentRef,
    media_type: Option<String>,
    stored: Vec<u8>,
    created_at_ms: i64,
}

#[derive(Debug, Default)]
pub(crate) struct PendingContent {
    objects: HashMap<String, ContentObject>,
}

impl PendingContent {
    fn insert(&mut self, object: ContentObject) -> Result<()> {
        if let Some(existing) = self.objects.get(&object.reference.content_id) {
            anyhow::ensure!(
                existing.reference.codec == object.reference.codec
                    && existing.reference.raw_length == object.reference.raw_length
                    && existing.stored == object.stored,
                "BLAKE3 content-id collision detected"
            );
        } else {
            self.objects
                .insert(object.reference.content_id.clone(), object);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ResolvedObject {
    raw_length: u64,
    bytes: Vec<u8>,
}

pub(crate) fn content_columns(kind: StorylineTableKind) -> &'static [(&'static str, bool)] {
    match kind {
        StorylineTableKind::Runs => &[
            ("agent_tool_definitions_json", true),
            ("agent_extra_json", true),
            ("parent_json", true),
            ("child_session_ids_json", true),
            ("notes", false),
            ("final_metrics_json", true),
            ("continued_trajectory_ref", false),
            ("extra_json", true),
        ],
        StorylineTableKind::Steps => &[
            ("message_json", true),
            ("reasoning_content", false),
            ("reasoning_effort_json", true),
            ("metrics_json", true),
            ("extra_json", true),
        ],
        StorylineTableKind::ToolCalls => &[
            ("arguments_json", true),
            ("results_json", true),
            ("extra_json", true),
        ],
    }
}

pub(crate) fn externalize_batches(
    batches: Vec<RecordBatch>,
    kind: StorylineTableKind,
    options: StorylineContentOptions,
    pending: &mut PendingContent,
) -> Result<Vec<RecordBatch>> {
    batches
        .into_iter()
        .map(|batch| externalize_batch(batch, kind, options, pending))
        .collect()
}

fn externalize_batch(
    batch: RecordBatch,
    kind: StorylineTableKind,
    options: StorylineContentOptions,
    pending: &mut PendingContent,
) -> Result<RecordBatch> {
    let mut columns = batch.columns().to_vec();
    for (name, is_json) in content_columns(kind) {
        let index = batch.schema().index_of(name)?;
        let values = columns[index]
            .as_any()
            .downcast_ref::<StringArray>()
            .with_context(|| format!("Storyline content column '{name}' is not Utf8"))?;
        let mut encoded = Vec::with_capacity(values.len());
        for row in 0..values.len() {
            if values.is_null(row) {
                encoded.push(None);
                continue;
            }
            let value = values.value(row);
            let should_offload =
                value.len() >= options.offload_threshold || value.starts_with(CONTENT_REF_MAGIC);
            if !should_offload {
                encoded.push(Some(value.to_string()));
                continue;
            }
            let object = build_object(
                value.as_bytes(),
                if *is_json {
                    LogicalType::Json
                } else {
                    LogicalType::Utf8
                },
                options,
            )?;
            let descriptor = object.reference.encode();
            pending.insert(object)?;
            encoded.push(Some(descriptor));
        }
        columns[index] = Arc::new(StringArray::from(encoded));
    }
    RecordBatch::try_new(batch.schema(), columns).context("externalize Storyline content batch")
}

fn build_object(
    bytes: &[u8],
    logical_type: LogicalType,
    options: StorylineContentOptions,
) -> Result<ContentObject> {
    let content_id = blake3::hash(bytes).to_hex().to_string();
    let compressed = zstd::stream::encode_all(bytes, options.zstd_level)
        .context("compress Storyline content")?;
    let (codec, stored) = if compressed.len() + 32 < bytes.len() {
        (ContentCodec::Zstd, compressed)
    } else {
        (ContentCodec::Identity, bytes.to_vec())
    };
    let preview = utf8_preview(bytes, options.preview_bytes)?;
    Ok(ContentObject {
        reference: ContentRef {
            logical_type,
            codec,
            content_id,
            raw_length: bytes.len() as u64,
            preview,
        },
        media_type: Some(
            match logical_type {
                LogicalType::Json => "application/json",
                LogicalType::Utf8 => "text/plain; charset=utf-8",
                LogicalType::Binary => "application/octet-stream",
            }
            .to_string(),
        ),
        stored,
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    })
}

fn utf8_preview(bytes: &[u8], maximum: usize) -> Result<String> {
    let value =
        std::str::from_utf8(bytes).context("UTF-8 content column contains invalid bytes")?;
    if value.len() <= maximum {
        return Ok(value.to_string());
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Ok(value[..end].to_string())
}

pub(crate) fn objects_arrow_schema() -> SchemaRef {
    Arc::new(ArrowSchema::new(vec![
        Field::new(CONTENT_ID_COLUMN, DataType::Utf8, false),
        Field::new("logical_type", DataType::UInt8, false),
        Field::new("media_type", DataType::Utf8, true),
        Field::new("raw_length", DataType::UInt64, false),
        Field::new("stored_length", DataType::UInt64, false),
        Field::new("codec", DataType::UInt8, false),
        Field::new("preview", DataType::Utf8, true),
        blob_field_with_options(
            PAYLOAD_COLUMN,
            false,
            BlobFieldOptions::default().with_inline_size_threshold(0),
        ),
        Field::new("created_at_ms", DataType::Int64, false),
    ]))
}

fn objects_to_batch(objects: &[ContentObject]) -> Result<RecordBatch> {
    let mut payloads = BlobArrayBuilder::new(objects.len());
    for object in objects {
        payloads.push_bytes(&object.stored)?;
    }
    RecordBatch::try_new(
        objects_arrow_schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                objects
                    .iter()
                    .map(|object| object.reference.content_id.as_str()),
            )),
            Arc::new(UInt8Array::from_iter_values(
                objects
                    .iter()
                    .map(|object| object.reference.logical_type as u8),
            )),
            Arc::new(StringArray::from(
                objects
                    .iter()
                    .map(|object| object.media_type.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from_iter_values(
                objects.iter().map(|object| object.reference.raw_length),
            )),
            Arc::new(UInt64Array::from_iter_values(
                objects.iter().map(|object| object.stored.len() as u64),
            )),
            Arc::new(UInt8Array::from_iter_values(
                objects.iter().map(|object| object.reference.codec as u8),
            )),
            Arc::new(StringArray::from_iter_values(
                objects
                    .iter()
                    .map(|object| object.reference.preview.as_str()),
            )),
            payloads.finish()?,
            Arc::new(Int64Array::from_iter_values(
                objects.iter().map(|object| object.created_at_ms),
            )),
        ],
    )
    .context("build Storyline objects batch")
}

pub(crate) async fn commit_pending_content(
    path: &Path,
    snapshot_version: Option<u64>,
    pending: PendingContent,
) -> Result<u64> {
    let mut objects = pending.objects.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| left.reference.content_id.cmp(&right.reference.content_id));
    let uri = path.to_string_lossy().into_owned();

    let Some(snapshot_version) = snapshot_version else {
        let batch = objects_to_batch(&objects)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], objects_arrow_schema());
        let mut dataset = InsertBuilder::new(&uri)
            .with_params(&WriteParams {
                mode: WriteMode::Create,
                data_storage_version: Some(LanceFileVersion::V2_2),
                ..Default::default()
            })
            .execute_stream(reader)
            .await
            .with_context(|| format!("create Storyline content store {}", path.display()))?;
        ensure_content_index(&mut dataset).await?;
        return Ok(dataset.version_id());
    };

    let mut dataset = open_objects(path, snapshot_version).await?;
    let latest = Dataset::open(&uri).await?.version_id();
    if latest != snapshot_version {
        dataset.restore().await.with_context(|| {
            format!(
                "restore Storyline content store {} to version {snapshot_version}",
                path.display()
            )
        })?;
    }
    if objects.is_empty() {
        return Ok(dataset.version_id());
    }
    let ids = objects
        .iter()
        .map(|object| object.reference.content_id.clone())
        .collect::<HashSet<_>>();
    let existing = existing_content_ids(&dataset, &ids).await?;
    objects.retain(|object| !existing.contains(&object.reference.content_id));
    if objects.is_empty() {
        return Ok(dataset.version_id());
    }
    let batch = objects_to_batch(&objects)?;
    let reader = RecordBatchIterator::new(vec![Ok(batch)], objects_arrow_schema());
    dataset = InsertBuilder::new(Arc::new(dataset))
        .with_params(&WriteParams {
            mode: WriteMode::Append,
            data_storage_version: Some(LanceFileVersion::V2_2),
            ..Default::default()
        })
        .execute_stream(reader)
        .await
        .with_context(|| format!("append Storyline content store {}", path.display()))?;
    ensure_content_index(&mut dataset).await?;
    dataset
        .optimize_indices(&lance_index::optimize::OptimizeOptions::append())
        .await
        .with_context(|| format!("extend Storyline content index {}", path.display()))?;
    Ok(dataset.version_id())
}

async fn ensure_content_index(dataset: &mut Dataset) -> Result<()> {
    if dataset.count_rows(None).await? == 0
        || !dataset
            .load_indices_by_name(CONTENT_INDEX_NAME)
            .await?
            .is_empty()
    {
        return Ok(());
    }
    let _admission = super::index_build_gate::acquire().await;
    dataset
        .create_index(
            &[CONTENT_ID_COLUMN],
            IndexType::BTree,
            Some(CONTENT_INDEX_NAME.to_string()),
            &ScalarIndexParams::for_builtin(BuiltinIndexType::BTree),
            false,
        )
        .await
        .context("create Storyline content-id index")?;
    Ok(())
}

async fn existing_content_ids(
    dataset: &Dataset,
    requested: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut existing = HashSet::new();
    let mut values = requested.iter().collect::<Vec<_>>();
    values.sort();
    for chunk in values.chunks(LOOKUP_CHUNK_SIZE) {
        let predicate = content_id_predicate(chunk.iter().map(|value| value.as_str()));
        let mut scan = dataset.scan();
        scan.project(&[CONTENT_ID_COLUMN])?;
        scan.filter(&predicate)?;
        scan.use_scalar_index(true);
        let batches: Vec<RecordBatch> = scan.try_into_stream().await?.try_collect().await?;
        for batch in batches {
            let ids = batch
                .column_by_name(CONTENT_ID_COLUMN)
                .context("content lookup missing content_id")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("content_id is not Utf8")?;
            existing.extend(ids.iter().flatten().map(ToOwned::to_owned));
        }
    }
    Ok(existing)
}

fn content_id_predicate<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let values = values
        .into_iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>();
    format!("{CONTENT_ID_COLUMN} IN ({})", values.join(", "))
}

pub(crate) async fn open_objects(path: &Path, version: u64) -> Result<Dataset> {
    let dataset = Dataset::open(path.to_string_lossy().as_ref())
        .await
        .with_context(|| format!("open Storyline content store {}", path.display()))?;
    dataset.checkout_version(version).await.with_context(|| {
        format!(
            "open Storyline content store {} at version {version}",
            path.display()
        )
    })
}

pub(crate) async fn hydrate_batches(
    dataset: &Arc<Dataset>,
    batches: Vec<RecordBatch>,
    kind: StorylineTableKind,
) -> Result<Vec<RecordBatch>> {
    let selected = content_columns(kind)
        .iter()
        .map(|(name, _)| *name)
        .collect::<HashSet<_>>();
    hydrate_selected_batches(dataset, batches, &selected).await
}

pub(crate) async fn hydrate_selected_batches(
    dataset: &Arc<Dataset>,
    batches: Vec<RecordBatch>,
    selected: &HashSet<&str>,
) -> Result<Vec<RecordBatch>> {
    let mut references = HashMap::<String, ContentRef>::new();
    for batch in &batches {
        for name in selected {
            let Some(column) = batch.column_by_name(name) else {
                continue;
            };
            let values = column
                .as_any()
                .downcast_ref::<StringArray>()
                .with_context(|| format!("Storyline content column '{name}' is not Utf8"))?;
            for value in values.iter().flatten() {
                if let Some(reference) = ContentRef::parse(value)
                    .with_context(|| format!("invalid internal content descriptor in '{name}'"))?
                {
                    references
                        .entry(reference.content_id.clone())
                        .or_insert(reference);
                }
            }
        }
    }
    if references.is_empty() {
        return Ok(batches);
    }
    let resolved = resolve_objects(dataset, &references).await?;
    batches
        .into_iter()
        .map(|batch| hydrate_batch(batch, selected, &resolved))
        .collect()
}

pub(crate) fn preview_selected_batches(
    batches: Vec<RecordBatch>,
    selected: &HashSet<&str>,
) -> Result<Vec<RecordBatch>> {
    batches
        .into_iter()
        .map(|batch| preview_batch(batch, selected))
        .collect()
}

fn preview_batch(batch: RecordBatch, selected: &HashSet<&str>) -> Result<RecordBatch> {
    let mut columns = batch.columns().to_vec();
    for name in selected {
        let Ok(index) = batch.schema().index_of(name) else {
            continue;
        };
        let values = columns[index]
            .as_any()
            .downcast_ref::<StringArray>()
            .with_context(|| format!("Storyline content column '{name}' is not Utf8"))?;
        let mut previews = Vec::with_capacity(values.len());
        for value in values.iter() {
            let Some(value) = value else {
                previews.push(None);
                continue;
            };
            previews.push(Some(match ContentRef::parse(value)? {
                Some(reference) => reference.preview,
                None => value.to_string(),
            }));
        }
        columns[index] = Arc::new(StringArray::from(previews));
    }
    RecordBatch::try_new(batch.schema(), columns).context("preview Storyline content batch")
}

fn hydrate_batch(
    batch: RecordBatch,
    selected: &HashSet<&str>,
    resolved: &HashMap<String, ResolvedObject>,
) -> Result<RecordBatch> {
    let mut columns = batch.columns().to_vec();
    for name in selected {
        let Ok(index) = batch.schema().index_of(name) else {
            continue;
        };
        let values = columns[index]
            .as_any()
            .downcast_ref::<StringArray>()
            .with_context(|| format!("Storyline content column '{name}' is not Utf8"))?;
        let mut hydrated = Vec::with_capacity(values.len());
        for value in values.iter() {
            let Some(value) = value else {
                hydrated.push(None);
                continue;
            };
            let Some(reference) = ContentRef::parse(value)? else {
                hydrated.push(Some(value.to_string()));
                continue;
            };
            let object = resolved.get(&reference.content_id).with_context(|| {
                format!(
                    "Storyline content object '{}' is missing from the committed snapshot",
                    reference.content_id
                )
            })?;
            anyhow::ensure!(
                object.raw_length == reference.raw_length,
                "Storyline content descriptor metadata mismatch for '{}'",
                reference.content_id
            );
            hydrated
                .push(Some(String::from_utf8(object.bytes.clone()).context(
                    "Storyline UTF-8 content object contains invalid bytes",
                )?));
        }
        columns[index] = Arc::new(StringArray::from(hydrated));
    }
    RecordBatch::try_new(batch.schema(), columns).context("hydrate Storyline content batch")
}

async fn resolve_objects(
    dataset: &Arc<Dataset>,
    references: &HashMap<String, ContentRef>,
) -> Result<HashMap<String, ResolvedObject>> {
    let mut resolved = HashMap::with_capacity(references.len());
    let mut ids = references.keys().collect::<Vec<_>>();
    ids.sort();
    for chunk in ids.chunks(LOOKUP_CHUNK_SIZE) {
        let predicate = content_id_predicate(chunk.iter().map(|value| value.as_str()));
        let mut scan = dataset.scan();
        scan.project(&[CONTENT_ID_COLUMN, "logical_type", "raw_length", "codec"])?;
        scan.filter(&predicate)?;
        scan.use_scalar_index(true);
        scan.with_row_address();
        let batches: Vec<RecordBatch> = scan.try_into_stream().await?.try_collect().await?;
        for batch in batches {
            let ids = string_column(&batch, CONTENT_ID_COLUMN)?;
            let logical_types = u8_column(&batch, "logical_type")?;
            let raw_lengths = u64_column(&batch, "raw_length")?;
            let codecs = u8_column(&batch, "codec")?;
            let addresses = u64_column(&batch, ROW_ADDRESS_COLUMN)?;
            let address_values = (0..batch.num_rows())
                .map(|row| addresses.value(row))
                .collect::<Vec<_>>();
            let blobs = dataset
                .take_blobs_by_addresses(&address_values, PAYLOAD_COLUMN)
                .await?;
            anyhow::ensure!(
                blobs.len() == batch.num_rows(),
                "Blob lookup length mismatch"
            );
            for (row, blob) in blobs.into_iter().enumerate() {
                let content_id = ids.value(row).to_string();
                let _logical_type = LogicalType::from_u8(logical_types.value(row))?;
                let codec = ContentCodec::from_u8(codecs.value(row))?;
                let raw_length = raw_lengths.value(row);
                let stored = blob.read().await?;
                let bytes = match codec {
                    ContentCodec::Identity => stored.to_vec(),
                    ContentCodec::Zstd => zstd::stream::decode_all(stored.as_ref())
                        .context("decompress Storyline content")?,
                };
                anyhow::ensure!(
                    bytes.len() as u64 == raw_length,
                    "Storyline content length mismatch for '{content_id}'"
                );
                anyhow::ensure!(
                    blake3::hash(&bytes).to_hex().as_str() == content_id,
                    "Storyline content checksum mismatch for '{content_id}'"
                );
                anyhow::ensure!(
                    resolved
                        .insert(content_id.clone(), ResolvedObject { raw_length, bytes },)
                        .is_none(),
                    "duplicate Storyline content object '{content_id}'"
                );
            }
        }
    }
    anyhow::ensure!(
        resolved.len() == references.len(),
        "committed Storyline snapshot has dangling content references"
    );
    Ok(resolved)
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("content lookup missing '{name}'"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("content lookup column '{name}' is not Utf8"))
}

fn u8_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt8Array> {
    batch
        .column_by_name(name)
        .with_context(|| format!("content lookup missing '{name}'"))?
        .as_any()
        .downcast_ref::<UInt8Array>()
        .with_context(|| format!("content lookup column '{name}' is not UInt8"))
}

fn u64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .with_context(|| format!("content lookup missing '{name}'"))?
        .as_any()
        .downcast_ref::<UInt64Array>()
        .with_context(|| format!("content lookup column '{name}' is not UInt64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_versioned_and_strict() {
        let object = build_object(
            "你好 world".as_bytes(),
            LogicalType::Utf8,
            StorylineContentOptions::default(),
        )
        .unwrap();
        let encoded = object.reference.encode();
        assert_eq!(ContentRef::parse(&encoded).unwrap(), Some(object.reference));
        assert!(ContentRef::parse("plain text").unwrap().is_none());
        assert!(ContentRef::parse(&format!("{encoded}:extra")).is_err());
    }

    #[test]
    fn preview_does_not_split_utf8() {
        assert_eq!(utf8_preview("你好abc".as_bytes(), 4).unwrap(), "你");
    }
}
