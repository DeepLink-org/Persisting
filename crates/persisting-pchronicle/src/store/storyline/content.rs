//! Content-addressed storage for large Storyline cells.
//!
//! Large UTF-8 / JSON cells are replaced internally with a compact descriptor
//! and stored once in `objects.lance`. Native Lance JSON columns keep their
//! outer JSONB envelope and only replace oversized nested values. Public
//! pChronicle readers hydrate descriptors before they return data.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{
    Array, Int64Array, LargeBinaryArray, RecordBatch, RecordBatchIterator, StringArray, UInt8Array,
    UInt64Array,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, SchemaRef};
use lance::index::DatasetIndexExt;
use lance::{BlobArrayBuilder, BlobFieldOptions, Dataset, blob_field_with_options};
use lance_file::version::LanceFileVersion;
use lance_index::IndexType;
use lance_index::scalar::{BuiltinIndexType, ScalarIndexParams};

use super::datafusion::StorylineTableKind;
use crate::formats::unknown_fields::{
    DEFAULT_MAX_UNKNOWN_BYTES, DEFAULT_MAX_UNKNOWN_FIELDS, StorylineUnknownFields,
    UnknownFieldLimits,
};

pub const STORYLINE_OBJECTS_DATASET: &str = "objects.lance";
pub const DEFAULT_CONTENT_OFFLOAD_THRESHOLD: usize = 64 * 1024;
pub const DEFAULT_CONTENT_PREVIEW_BYTES: usize = 256;
pub(crate) const CONTENT_REF_MAGIC: &str = "\u{001e}PCHRONICLE-CONTENT:";
const CONTENT_INDEX_NAME: &str = "pchronicle_content_id_idx";
const CONTENT_ID_COLUMN: &str = "content_id";
const PAYLOAD_COLUMN: &str = "payload";
const ROW_ADDRESS_COLUMN: &str = "_rowaddr";
const LOOKUP_CHUNK_SIZE: usize = 512;
const BLOB_READ_BUFFER_BYTES: u64 = 16 * 1024 * 1024;

fn json_text_values(column: &dyn Array, name: &str) -> Result<Vec<Option<String>>> {
    if let Some(values) = column.as_any().downcast_ref::<StringArray>() {
        return Ok(values
            .iter()
            .map(|value| value.map(ToOwned::to_owned))
            .collect());
    }
    if let Some(values) = column.as_any().downcast_ref::<LargeBinaryArray>() {
        return Ok(values
            .iter()
            .map(|value| value.map(lance_arrow::json::decode_json))
            .collect());
    }
    anyhow::bail!("Storyline JSON column '{name}' is neither Utf8 nor Lance JSONB")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorylineContentOptions {
    /// Serialized cell size at which content is moved to `objects.lance`.
    pub offload_threshold: usize,
    /// Maximum number of UTF-8 bytes copied into the descriptor preview.
    pub preview_bytes: usize,
    /// Zstd compression level for UTF-8 and JSON content.
    pub zstd_level: i32,
    /// Maximum normalized rows produced by one Storyline document.
    pub max_document_rows: Option<usize>,
    /// Maximum serialized JSON bytes in one Storyline document.
    pub max_document_bytes: Option<usize>,
    /// Maximum normalized rows retained in one streamed import chunk.
    pub max_chunk_rows: Option<usize>,
    /// Maximum serialized document bytes retained in one streamed import chunk.
    pub max_chunk_bytes: Option<usize>,
    /// Maximum number of documents accepted by one streamed import.
    pub max_import_documents: Option<usize>,
    /// Maximum number of logical unknown fields retained by one document.
    /// `usize::MAX` disables the field-count limit.
    pub max_unknown_fields: usize,
    /// Maximum logical JSON bytes retained in unknown fields by one document.
    /// `usize::MAX` disables the byte limit.
    pub max_unknown_bytes: usize,
}

impl Default for StorylineContentOptions {
    fn default() -> Self {
        Self {
            offload_threshold: DEFAULT_CONTENT_OFFLOAD_THRESHOLD,
            preview_bytes: DEFAULT_CONTENT_PREVIEW_BYTES,
            zstd_level: 3,
            max_document_rows: None,
            max_document_bytes: None,
            max_chunk_rows: None,
            max_chunk_bytes: None,
            max_import_documents: None,
            max_unknown_fields: DEFAULT_MAX_UNKNOWN_FIELDS,
            max_unknown_bytes: DEFAULT_MAX_UNKNOWN_BYTES,
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
        for (name, value) in [
            ("max_document_rows", self.max_document_rows),
            ("max_document_bytes", self.max_document_bytes),
            ("max_chunk_rows", self.max_chunk_rows),
            ("max_chunk_bytes", self.max_chunk_bytes),
            ("max_import_documents", self.max_import_documents),
        ] {
            if let Some(value) = value {
                anyhow::ensure!(value > 0, "{name} must be positive");
            }
        }
        self.unknown_field_limits().validate()?;
        Ok(self)
    }

    pub(crate) fn unknown_field_limits(self) -> UnknownFieldLimits {
        UnknownFieldLimits {
            max_fields: self.max_unknown_fields,
            max_bytes: self.max_unknown_bytes,
        }
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
    codec: ContentCodec,
    raw_length: u64,
    bytes: Vec<u8>,
}

pub(crate) fn externalize_unknown_field_values(
    fields: &mut StorylineUnknownFields,
    options: StorylineContentOptions,
    pending: &mut PendingContent,
) -> Result<()> {
    for source in fields.sources.values_mut() {
        for value in source.fields.values_mut() {
            let encoded = serde_json::to_vec(value).context("serialize Storyline unknown value")?;
            let collides_with_descriptor = value
                .as_str()
                .is_some_and(|value| value.starts_with(CONTENT_REF_MAGIC));
            if encoded.len() < options.offload_threshold && !collides_with_descriptor {
                continue;
            }
            let object = build_object(&encoded, LogicalType::Json, options)?;
            let descriptor = object.reference.encode();
            pending.insert(object)?;
            *value = serde_json::Value::String(descriptor);
        }
    }
    Ok(())
}

pub(crate) fn content_columns(kind: StorylineTableKind) -> &'static [(&'static str, bool)] {
    match kind {
        StorylineTableKind::Runs => &[
            ("origin", true),
            ("agent_tool_definitions", true),
            ("parent", true),
            ("child_session_ids", true),
            ("notes", false),
            ("unknown_key_counts", true),
            ("task", true),
            ("continued_trajectory_ref", false),
            ("prompt", true),
        ],
        StorylineTableKind::Steps => &[
            ("message_value", true),
            ("reasoning_content", false),
            ("reasoning_effort_value", true),
            ("observation", true),
            ("env", true),
            ("prompt", true),
        ],
        StorylineTableKind::ToolCalls => {
            &[("arguments", true), ("result", true), ("results", true)]
        }
    }
}

/// Native Lance JSON columns are kept queryable as JSONB envelopes. Large
/// values inside these envelopes are replaced by content descriptors instead
/// of offloading the whole cell.
pub(crate) fn native_json_columns(kind: StorylineTableKind) -> &'static [&'static str] {
    match kind {
        StorylineTableKind::Runs => &[
            "agent_extra",
            "final_metrics",
            "extra",
            "meta",
            "unknown_fields",
        ],
        StorylineTableKind::Steps => &["metrics", "extra"],
        StorylineTableKind::ToolCalls => &["extra", "response"],
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
    for name in native_json_columns(kind) {
        let Ok(index) = batch.schema().index_of(name) else {
            continue;
        };
        let values = json_text_values(columns[index].as_ref(), name)?;
        let mut encoded = Vec::with_capacity(values.len());
        for value in values {
            let Some(value) = value else {
                encoded.push(None);
                continue;
            };
            let mut json_value: serde_json::Value = serde_json::from_str(&value)
                .with_context(|| format!("decode native Storyline JSON column '{name}'"))?;
            externalize_nested_json_values(&mut json_value, options, pending)?;
            encoded.push(Some(serde_json::to_string(&json_value).with_context(
                || format!("encode native Storyline JSON column '{name}'"),
            )?));
        }
        columns[index] = Arc::new(
            lance_arrow::json::JsonArray::try_from_iter(encoded)
                .context("encode native Storyline JSONB column")?
                .into_inner(),
        );
    }
    for (name, is_json) in content_columns(kind) {
        let Ok(index) = batch.schema().index_of(name) else {
            continue;
        };
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

fn externalize_nested_json_values(
    value: &mut serde_json::Value,
    options: StorylineContentOptions,
    pending: &mut PendingContent,
) -> Result<()> {
    match value {
        serde_json::Value::Object(fields) => {
            for child in fields.values_mut() {
                externalize_nested_json_slot(child, options, pending)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                externalize_nested_json_slot(child, options, pending)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

fn externalize_nested_json_slot(
    value: &mut serde_json::Value,
    options: StorylineContentOptions,
    pending: &mut PendingContent,
) -> Result<()> {
    if let Some(existing) = value.as_str()
        && ContentRef::parse(existing)?.is_some()
    {
        return Ok(());
    }
    let encoded = serde_json::to_vec(value).context("serialize nested Storyline JSON value")?;
    if encoded.len() >= options.offload_threshold {
        let object = build_object(&encoded, LogicalType::Json, options)?;
        let descriptor = object.reference.encode();
        pending.insert(object)?;
        *value = serde_json::Value::String(descriptor);
        return Ok(());
    }
    externalize_nested_json_values(value, options, pending)
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
    reopen_concurrent_create: bool,
) -> Result<u64> {
    let mut objects = pending.objects.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| left.reference.content_id.cmp(&right.reference.content_id));
    let uri = path.to_string_lossy().into_owned();

    let mut dataset = if let Some(snapshot_version) = snapshot_version {
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
        dataset
    } else {
        let batch = objects_to_batch(&objects)?;
        let reader = RecordBatchIterator::new(vec![Ok(batch)], objects_arrow_schema());
        match InsertBuilder::new(&uri)
            .with_params(&WriteParams {
                mode: WriteMode::Create,
                data_storage_version: Some(LanceFileVersion::V2_2),
                ..Default::default()
            })
            .execute_stream(reader)
            .await
        {
            Ok(mut dataset) => {
                ensure_content_index(&mut dataset).await?;
                return Ok(dataset.version_id());
            }
            Err(lance::Error::DatasetAlreadyExists { .. }) if reopen_concurrent_create => {
                Dataset::open(&uri).await.with_context(|| {
                    format!(
                        "reopen concurrently created Storyline content store {}",
                        path.display()
                    )
                })?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create Storyline content store {}", path.display()));
            }
        }
    };
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
    let _admission = super::super::index_build_gate::acquire().await;
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

pub(crate) fn collect_content_ids(
    batches: &[RecordBatch],
    kind: StorylineTableKind,
) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    for batch in batches {
        for (name, _) in content_columns(kind) {
            let Some(column) = batch.column_by_name(name) else {
                continue;
            };
            let values = json_text_values(column.as_ref(), name)?;
            for value in values.iter().flatten() {
                if let Some(reference) = ContentRef::parse(value)? {
                    ids.insert(reference.content_id);
                }
            }
        }
        for name in native_json_columns(kind) {
            let Some(column) = batch.column_by_name(name) else {
                continue;
            };
            for encoded in json_text_values(column.as_ref(), name)?
                .into_iter()
                .flatten()
            {
                let value: serde_json::Value = serde_json::from_str(&encoded)
                    .with_context(|| format!("decode native Storyline JSON column '{name}'"))?;
                collect_nested_content_ids(&value, &mut ids)?;
            }
        }
    }
    Ok(ids)
}

pub(crate) async fn prune_unreferenced_objects(
    path: &Path,
    snapshot_version: u64,
    live: &HashSet<String>,
) -> Result<(u64, usize)> {
    let mut dataset = open_objects(path, snapshot_version).await?;
    let mut scan = dataset.scan();
    scan.project(&[CONTENT_ID_COLUMN])?;
    let batches: Vec<RecordBatch> = scan.try_into_stream().await?.try_collect().await?;
    let mut unreachable = Vec::new();
    for batch in batches {
        let ids = batch
            .column_by_name(CONTENT_ID_COLUMN)
            .context("objects missing content_id")?
            .as_any()
            .downcast_ref::<StringArray>()
            .context("content_id is not Utf8")?;
        unreachable.extend(
            ids.iter()
                .flatten()
                .filter(|id| !live.contains(*id))
                .map(str::to_string),
        );
    }
    let removed = unreachable.len();
    for chunk in unreachable.chunks(LOOKUP_CHUNK_SIZE) {
        dataset
            .delete(&content_id_predicate(chunk.iter().map(String::as_str)))
            .await?;
    }
    Ok((dataset.version_id(), removed))
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
    let batches = hydrate_selected_batches(dataset, batches, &selected).await?;
    hydrate_native_json_values(dataset, batches, kind).await
}

fn collect_nested_content_ids(value: &serde_json::Value, ids: &mut HashSet<String>) -> Result<()> {
    if let Some(encoded) = value.as_str() {
        if let Some(reference) = ContentRef::parse(encoded)? {
            ids.insert(reference.content_id);
        }
        return Ok(());
    }
    match value {
        serde_json::Value::Object(fields) => {
            for child in fields.values() {
                collect_nested_content_ids(child, ids)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_nested_content_ids(child, ids)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

async fn hydrate_native_json_values(
    dataset: &Arc<Dataset>,
    batches: Vec<RecordBatch>,
    kind: StorylineTableKind,
) -> Result<Vec<RecordBatch>> {
    let mut references = HashMap::<String, ContentRef>::new();
    for batch in &batches {
        for name in native_json_columns(kind) {
            let Some(column) = batch.column_by_name(name) else {
                continue;
            };
            for encoded in json_text_values(column.as_ref(), name)?
                .into_iter()
                .flatten()
            {
                let value: serde_json::Value = serde_json::from_str(&encoded)
                    .with_context(|| format!("decode native Storyline JSON column '{name}'"))?;
                collect_nested_content_refs(&value, &mut references)?;
            }
        }
    }
    if references.is_empty() {
        return Ok(batches);
    }
    let resolved = resolve_objects(dataset, &references).await?;
    batches
        .into_iter()
        .map(|batch| hydrate_native_json_batch(batch, kind, &resolved))
        .collect()
}

fn collect_nested_content_refs(
    value: &serde_json::Value,
    references: &mut HashMap<String, ContentRef>,
) -> Result<()> {
    if let Some(encoded) = value.as_str() {
        if let Some(reference) = ContentRef::parse(encoded)? {
            if let Some(existing) = references.get(&reference.content_id) {
                anyhow::ensure!(
                    existing == &reference,
                    "conflicting Storyline content descriptors for '{}'",
                    reference.content_id
                );
            } else {
                references.insert(reference.content_id.clone(), reference);
            }
        }
        return Ok(());
    }
    match value {
        serde_json::Value::Object(fields) => {
            for child in fields.values() {
                collect_nested_content_refs(child, references)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_nested_content_refs(child, references)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

fn hydrate_native_json_batch(
    batch: RecordBatch,
    kind: StorylineTableKind,
    resolved: &HashMap<String, ResolvedObject>,
) -> Result<RecordBatch> {
    let mut columns = batch.columns().to_vec();
    for name in native_json_columns(kind) {
        let Ok(index) = batch.schema().index_of(name) else {
            continue;
        };
        let values = json_text_values(batch.column(index).as_ref(), name)?;
        let hydrated = values
            .into_iter()
            .map(|encoded| {
                let Some(encoded) = encoded else {
                    return Ok(None);
                };
                let mut value: serde_json::Value = serde_json::from_str(&encoded)
                    .with_context(|| format!("decode native Storyline JSON column '{name}'"))?;
                hydrate_nested_json_values(&mut value, resolved)?;
                serde_json::to_string(&value)
                    .map(Some)
                    .with_context(|| format!("encode native Storyline JSON column '{name}'"))
            })
            .collect::<Result<Vec<_>>>()?;
        columns[index] = if lance_arrow::json::is_json_field(batch.schema().field(index)) {
            Arc::new(
                lance_arrow::json::JsonArray::try_from_iter(hydrated)
                    .context("encode hydrated native Storyline JSONB column")?
                    .into_inner(),
            )
        } else {
            Arc::new(StringArray::from(hydrated))
        };
    }
    RecordBatch::try_new(batch.schema(), columns)
        .context("hydrate native Storyline JSON content batch")
}

fn hydrate_nested_json_values(
    value: &mut serde_json::Value,
    resolved: &HashMap<String, ResolvedObject>,
) -> Result<()> {
    if let Some(encoded) = value.as_str() {
        let Some(reference) = ContentRef::parse(encoded)? else {
            return Ok(());
        };
        let object = resolved.get(&reference.content_id).with_context(|| {
            format!(
                "Storyline content object '{}' is missing from the committed snapshot",
                reference.content_id
            )
        })?;
        anyhow::ensure!(
            reference.logical_type == LogicalType::Json,
            "native JSON content descriptor is not JSON"
        );
        anyhow::ensure!(
            object.codec == reference.codec && object.raw_length == reference.raw_length,
            "Storyline content descriptor metadata mismatch for '{}'",
            reference.content_id
        );
        *value = serde_json::from_slice(&object.bytes).with_context(|| {
            format!(
                "Storyline native JSON content object '{}' is invalid JSON",
                reference.content_id
            )
        })?;
        return Ok(());
    }
    match value {
        serde_json::Value::Object(fields) => {
            for child in fields.values_mut() {
                hydrate_nested_json_values(child, resolved)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                hydrate_nested_json_values(child, resolved)?;
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
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
                object.codec == reference.codec && object.raw_length == reference.raw_length,
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
            // Plan reads together so Lance can coalesce packed blobs and schedule
            // remote I/O concurrently. Stream results to avoid retaining every
            // compressed payload alongside its decompressed contents.
            let mut blobs = dataset
                .read_blobs(PAYLOAD_COLUMN)?
                .with_row_addresses(address_values)
                .with_io_buffer_size_bytes(BLOB_READ_BUFFER_BYTES)
                .preserve_order(true)
                .try_into_stream()
                .await?;
            for row in 0..batch.num_rows() {
                let blob = blobs
                    .try_next()
                    .await?
                    .context("Blob lookup length mismatch")?;
                anyhow::ensure!(
                    blob.row_address == addresses.value(row),
                    "Blob lookup row address mismatch"
                );
                let content_id = ids.value(row).to_string();
                let _logical_type = LogicalType::from_u8(logical_types.value(row))?;
                let codec = ContentCodec::from_u8(codecs.value(row))?;
                let raw_length = raw_lengths.value(row);
                let stored = blob
                    .data
                    .ok_or_else(|| anyhow!("Missing Storyline content blob for '{content_id}'"))?;
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
                        .insert(
                            content_id.clone(),
                            ResolvedObject {
                                codec,
                                raw_length,
                                bytes,
                            },
                        )
                        .is_none(),
                    "duplicate Storyline content object '{content_id}'"
                );
            }
            anyhow::ensure!(
                blobs.try_next().await?.is_none(),
                "Blob lookup length mismatch"
            );
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
    fn descriptor_is_strict() {
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

    #[tokio::test]
    async fn planned_blob_reads_preserve_contents_across_fragments_and_lookup_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let options = StorylineContentOptions::default();
        let contents = (0..LOOKUP_CHUNK_SIZE + 2)
            .map(|index| match index {
                0 => String::new(),
                1 => "你好，批量内容恢复！".repeat(1024),
                _ => format!("short content {index}"),
            })
            .collect::<Vec<_>>();
        let objects = contents
            .iter()
            .map(|value| build_object(value.as_bytes(), LogicalType::Utf8, options).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(objects[0].reference.codec, ContentCodec::Identity);
        assert_eq!(objects[1].reference.codec, ContentCodec::Zstd);
        let references = objects
            .iter()
            .map(|object| {
                (
                    object.reference.content_id.clone(),
                    object.reference.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut dataset = InsertBuilder::new(dir.path().to_string_lossy().as_ref())
            .with_params(&WriteParams {
                data_storage_version: Some(LanceFileVersion::V2_2),
                ..Default::default()
            })
            .execute(vec![objects_to_batch(&objects[..256]).unwrap()])
            .await
            .unwrap();
        dataset = InsertBuilder::new(Arc::new(dataset))
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                data_storage_version: Some(LanceFileVersion::V2_2),
                ..Default::default()
            })
            .execute(vec![objects_to_batch(&objects[256..]).unwrap()])
            .await
            .unwrap();
        let dataset = Arc::new(dataset);
        let resolved = resolve_objects(&dataset, &references).await.unwrap();
        for (object, contents) in objects.iter().zip(contents) {
            assert_eq!(
                resolved[&object.reference.content_id].bytes,
                contents.as_bytes()
            );
        }

        let mut references = references;
        let absent = build_object(b"not stored", LogicalType::Utf8, options).unwrap();
        references.insert(absent.reference.content_id.clone(), absent.reference);
        let error = resolve_objects(&dataset, &references).await.err().unwrap();
        assert!(
            error.to_string().contains("dangling content references"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn planned_blob_reads_reject_corrupt_or_null_contents() {
        for null_payload in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut object = build_object(
                b"original",
                LogicalType::Utf8,
                StorylineContentOptions::default(),
            )
            .unwrap();
            let references = HashMap::from([(
                object.reference.content_id.clone(),
                object.reference.clone(),
            )]);
            // Keep the length and codec valid, but make the stored checksum wrong.
            object.stored[0] ^= 1;
            let mut batch = objects_to_batch(&[object]).unwrap();
            if null_payload {
                let index = batch.schema().index_of(PAYLOAD_COLUMN).unwrap();
                let mut fields = batch.schema().fields().to_vec();
                fields[index] = Arc::new(fields[index].as_ref().clone().with_nullable(true));
                let mut payloads = BlobArrayBuilder::new(1);
                payloads.push_null().unwrap();
                let mut columns = batch.columns().to_vec();
                columns[index] = payloads.finish().unwrap();
                batch = RecordBatch::try_new(Arc::new(ArrowSchema::new(fields)), columns).unwrap();
            }
            let dataset = InsertBuilder::new(dir.path().to_string_lossy().as_ref())
                .with_params(&WriteParams {
                    data_storage_version: Some(LanceFileVersion::V2_2),
                    ..Default::default()
                })
                .execute(vec![batch])
                .await
                .unwrap();
            let error = resolve_objects(&Arc::new(dataset), &references)
                .await
                .err()
                .unwrap();
            let expected = if null_payload {
                "Missing Storyline content blob"
            } else {
                "checksum mismatch"
            };
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    fn content_id_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[0-9a-f]{64}").unwrap()
    }

    proptest! {
        #[test]
        fn content_descriptors_roundtrip_all_logical_types_and_codecs(
            logical_type in prop_oneof![Just(LogicalType::Utf8), Just(LogicalType::Json), Just(LogicalType::Binary)],
            codec in prop_oneof![Just(ContentCodec::Identity), Just(ContentCodec::Zstd)],
            content_id in content_id_strategy(),
            raw_length in any::<u64>(),
            preview in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/\\n]{0,96}").unwrap(),
        ) {
            let reference = ContentRef { logical_type, codec, content_id, raw_length, preview };
            let encoded = reference.encode();
            prop_assert_eq!(ContentRef::parse(&encoded).expect("encoded descriptor parses"), Some(reference));
        }

        #[test]
        fn utf8_previews_never_split_a_codepoint(
            text in proptest::string::string_regex("[A-Za-z0-9你好世界🌍]{0,96}").unwrap(),
            maximum in 0usize..128,
        ) {
            let preview = utf8_preview(text.as_bytes(), maximum).expect("valid UTF-8 preview");
            prop_assert!(preview.len() <= maximum);
            prop_assert!(text.starts_with(&preview));
            prop_assert_eq!(preview.as_bytes(), &text.as_bytes()[..preview.len()]);
        }

        #[test]
        fn content_options_validate_exactly_the_documented_bounds(
            offload_threshold in 0usize..100_000,
            preview_bytes in 0usize..5_001,
            max_document_rows in prop::option::of(0usize..1_000),
            max_document_bytes in prop::option::of(0usize..1_000),
            max_chunk_rows in prop::option::of(0usize..1_000),
            max_chunk_bytes in prop::option::of(0usize..1_000),
            max_import_documents in prop::option::of(0usize..1_000),
            max_unknown_fields in 0usize..1_000,
            max_unknown_bytes in 0usize..1_000,
        ) {
            let options = StorylineContentOptions {
                offload_threshold,
                preview_bytes,
                max_document_rows,
                max_document_bytes,
                max_chunk_rows,
                max_chunk_bytes,
                max_import_documents,
                max_unknown_fields,
                max_unknown_bytes,
                ..StorylineContentOptions::default()
            };
            let optional_limits_valid = [
                max_document_rows,
                max_document_bytes,
                max_chunk_rows,
                max_chunk_bytes,
                max_import_documents,
            ]
            .into_iter()
            .flatten()
            .all(|value| value > 0);
            let expected = offload_threshold > 0
                && preview_bytes <= 4_096
                && optional_limits_valid
                && max_unknown_fields > 0
                && max_unknown_bytes > 0;
            prop_assert_eq!(options.validate().is_ok(), expected);
        }
    }
}
