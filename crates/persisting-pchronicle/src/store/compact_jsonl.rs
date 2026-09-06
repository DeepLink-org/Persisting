//! Compact JSONL: one JSON object per row in a columnar Lance dataset.
//!
//! The normative wire/storage contract is RFC-0014 (`docs/src/rfcs/0014-compact-jsonl.md`).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, ensure};
use futures::TryStreamExt;
use lance::Dataset;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{
    Array, LargeBinaryArray, RecordBatch, RecordBatchIterator, StringArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema};
use lance_arrow::json::{decode_json, encode_json, json_field};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RAW_COLUMN: &str = "_raw_";
const OFFLOAD_COLUMN: &str = "_offload_";
const FORMAT_KEY: &str = "pchronicle.format";
const FORMAT_NAME: &str = "compact-jsonl/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactJsonlColumn {
    pub name: String,
    pub path: String,
}

impl CompactJsonlColumn {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Result<Self> {
        let column = Self {
            name: name.into(),
            path: path.into(),
        };
        ensure!(
            !column.name.is_empty() && column.name != RAW_COLUMN && column.name != OFFLOAD_COLUMN,
            "compact JSONL column name is invalid"
        );
        ensure!(
            column
                .name
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_'
                    || c.is_ascii_alphanumeric() && (i > 0 || !c.is_ascii_digit())),
            "compact JSONL column name '{}' is not an identifier",
            column.name
        );
        ensure!(
            valid_path(&column.path),
            "compact JSONL path '{}' is not in the supported RFC-0014 subset",
            column.path
        );
        Ok(column)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactJsonlOptions {
    pub columns: Vec<CompactJsonlColumn>,
    /// Values larger than this are copied to `_offload_`; zero disables it.
    pub offload_threshold: usize,
}

impl CompactJsonlOptions {
    pub fn validate(&self) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        for column in &self.columns {
            ensure!(
                column.name != "filename" && column.name != "data",
                "compact JSONL custom column '{}' conflicts with a required column",
                column.name
            );
            ensure!(
                names.insert(&column.name),
                "duplicate compact JSONL column '{}'",
                column.name
            );
            ensure!(
                valid_path(&column.path),
                "compact JSONL path '{}' is not in the supported RFC-0014 subset",
                column.path
            );
        }
        Ok(())
    }

    fn path_for(&self, name: &str, default: &'static str) -> &str {
        self.columns
            .iter()
            .find(|column| column.name == name)
            .map_or(default, |column| column.path.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactJsonlOffload {
    pub path: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactJsonlRecord {
    pub id: String,
    pub timestamp: String,
    pub filename: String,
}

pub struct CompactJsonlStore;

impl CompactJsonlStore {
    pub async fn records(input: impl AsRef<Path>) -> Result<Vec<CompactJsonlRecord>> {
        let dataset = Dataset::open(input.as_ref().to_string_lossy().as_ref()).await?;
        validate_dataset_schema(&dataset)?;
        let stream = dataset.scan().scan_in_order(true).try_into_stream().await?;
        let mut stream = stream;
        let mut out = Vec::new();
        while let Some(batch) = stream.try_next().await? {
            let ids = batch
                .column(batch.schema().index_of("id")?)
                .as_any()
                .downcast_ref::<StringArray>()
                .context("compact JSONL id must be Utf8")?;
            let timestamps = batch
                .column(batch.schema().index_of("timestamp")?)
                .as_any()
                .downcast_ref::<StringArray>()
                .context("compact JSONL timestamp must be Utf8")?;
            let filenames = batch
                .column(batch.schema().index_of("filename")?)
                .as_any()
                .downcast_ref::<StringArray>()
                .context("compact JSONL filename must be Utf8")?;
            for row in 0..batch.num_rows() {
                out.push(CompactJsonlRecord {
                    id: ids.value(row).into(),
                    timestamp: timestamps.value(row).into(),
                    filename: filenames.value(row).into(),
                });
            }
        }
        Ok(out)
    }

    /// Read one record for the Web explorer without assigning trajectory
    /// semantics to the compact row.
    pub async fn read_record(input: impl AsRef<Path>, id: &str) -> Result<Option<Value>> {
        let dataset = Dataset::open(input.as_ref().to_string_lossy().as_ref()).await?;
        let (_, offload_idx) = validate_dataset_schema(&dataset)?;
        let stream = dataset.scan().scan_in_order(true).try_into_stream().await?;
        let mut stream = stream;
        while let Some(batch) = stream.try_next().await? {
            let ids = batch
                .column(batch.schema().index_of("id")?)
                .as_any()
                .downcast_ref::<StringArray>()
                .context("compact JSONL id must be Utf8")?;
            let data = batch.column(batch.schema().index_of("data")?);
            let offloads = batch.column(offload_idx);
            for row in 0..batch.num_rows() {
                if ids.value(row) != id {
                    continue;
                }
                let inline = json_text_at(data.as_ref(), row)?
                    .filter(|value| serde_json::from_str::<Value>(value).ok() != Some(Value::Null));
                let raw = if let Some(value) = inline {
                    value
                } else {
                    let descriptor = json_text_at(offloads.as_ref(), row)?
                        .context("compact JSONL row has neither data nor offload")?;
                    let reference: CompactJsonlOffload = serde_json::from_str(&descriptor)?;
                    let bytes =
                        fs::read(input.as_ref().join(&reference.path).join(&reference.key))?;
                    ensure!(
                        blake3::hash(&bytes).to_hex().as_str() == reference.key,
                        "compact JSONL offload digest mismatch"
                    );
                    String::from_utf8(bytes)?
                };
                return Ok(Some(serde_json::from_str(&raw)?));
            }
        }
        Ok(None)
    }

    pub async fn import_path(
        input: impl AsRef<Path>,
        output: impl AsRef<Path>,
        options: &CompactJsonlOptions,
    ) -> Result<usize> {
        let input = input.as_ref();
        let output = output.as_ref();
        options.validate()?;
        let files = collect_jsonl(input)?;
        ensure!(
            !files.is_empty(),
            "compact JSONL input contains no .jsonl files"
        );
        if output.exists() {
            fs::remove_dir_all(output)
                .with_context(|| format!("replace compact JSONL output {}", output.display()))?;
        }
        fs::create_dir_all(output)?;
        let mut rows = Vec::new();
        for file in files {
            let relative_path = if input.is_dir() {
                file.strip_prefix(input).unwrap_or(&file)
            } else {
                file.file_name().map(Path::new).unwrap_or(&file)
            };
            let relative = relative_path
                .to_str()
                .context("compact JSONL filename is not UTF-8")?
                .replace('\\', "/");
            let mut reader = BufReader::new(File::open(&file)?);
            let first_row = rows.len();
            for line_no in 1usize.. {
                let mut raw = Vec::new();
                if reader.read_until(b'\n', &mut raw)? == 0 {
                    break;
                }
                let mut end = raw.len() - usize::from(raw.ends_with(b"\n"));
                end -= usize::from(raw[..end].ends_with(b"\r"));
                let json = &raw[..end];
                ensure!(
                    !json.iter().all(u8::is_ascii_whitespace),
                    "compact JSONL {}:{} is empty",
                    relative,
                    line_no
                );
                let value: Value = serde_json::from_slice(json)
                    .with_context(|| format!("parse compact JSONL {relative}:{line_no}"))?;
                ensure!(
                    value.is_object(),
                    "compact JSONL {relative}:{line_no} must be a JSON object"
                );
                rows.push((value, raw, relative.clone(), line_no));
            }
            ensure!(rows.len() > first_row, "compact JSONL {relative} is empty");
        }
        let schema = schema(options)?;
        let mut arrays: Vec<Arc<dyn Array>> = Vec::new();
        let required = |name: &str, default| -> Result<Vec<String>> {
            let path = options.path_for(name, default);
            rows.iter()
                .map(|(value, _, file, line)| {
                    path_value(value, path)
                        .and_then(scalar_string)
                        .filter(|value| !value.is_empty())
                        .with_context(|| {
                            format!("compact JSONL {file}:{line} requires scalar {name} at {path}")
                        })
                })
                .collect()
        };
        let ids = rows
            .iter()
            .map(|(value, _, file, line)| {
                path_value(value, options.path_for("id", "$.id"))
                    .and_then(scalar_string)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| format!("{file}#{line}"))
            })
            .collect::<Vec<_>>();
        let timestamps = required("timestamp", "$.timestamp")?;
        let mut unique_ids = std::collections::HashSet::new();
        for id in &ids {
            ensure!(unique_ids.insert(id), "duplicate compact JSONL id '{id}'");
        }
        let filenames: Vec<String> = rows.iter().map(|(_, _, file, _)| file.clone()).collect();
        let mut offloads = Vec::with_capacity(rows.len());
        let offload_dir = output.join("_offload");
        for (_, raw, _, _) in &rows {
            if options.offload_threshold > 0 && raw.len() >= options.offload_threshold {
                fs::create_dir_all(&offload_dir)?;
                let key = blake3::hash(raw).to_hex().to_string();
                let path = offload_dir.join(&key);
                if path.exists() {
                    ensure!(
                        fs::read(&path)? == *raw,
                        "compact JSONL offload digest collision or corruption for key {key}"
                    );
                } else {
                    let mut file = File::create(&path)?;
                    file.write_all(raw)?;
                    file.sync_all()?;
                }
                offloads.push(Some(CompactJsonlOffload {
                    path: "_offload".into(),
                    key,
                }));
            } else {
                offloads.push(None);
            }
        }
        arrays.push(Arc::new(StringArray::from(ids)));
        arrays.push(Arc::new(StringArray::from(timestamps)));
        arrays.push(Arc::new(StringArray::from(filenames)));
        let data = rows
            .iter()
            .zip(&offloads)
            .map(|((value, _, _, _), offload)| {
                let value = if offload.is_none() {
                    serde_json::to_string(value)?
                } else {
                    "null".into()
                };
                encode_json_bytes(&value)
            })
            .collect::<Result<Vec<_>>>()?;
        arrays.push(Arc::new(LargeBinaryArray::from(
            data.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )));
        for column in &options.columns {
            if matches!(column.name.as_str(), "id" | "timestamp") {
                continue;
            }
            let values = rows
                .iter()
                .map(|(v, _, _, _)| -> Result<Option<Vec<u8>>> {
                    path_value(v, &column.path)
                        .map(|x| {
                            let json = serde_json::to_string(x)?;
                            encode_json_bytes(&json)
                        })
                        .transpose()
                })
                .collect::<Result<Vec<_>>>()?;
            arrays.push(Arc::new(LargeBinaryArray::from(
                values.iter().map(|x| x.as_deref()).collect::<Vec<_>>(),
            )));
        }
        let offload_values = offloads
            .iter()
            .map(|value| -> Result<Option<Vec<u8>>> {
                value
                    .as_ref()
                    .map(|value| {
                        let json = serde_json::to_string(value)?;
                        encode_json_bytes(&json)
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        arrays.push(Arc::new(LargeBinaryArray::from(
            offload_values
                .iter()
                .map(|x| x.as_deref())
                .collect::<Vec<_>>(),
        )));
        arrays.push(Arc::new(LargeBinaryArray::from(
            rows.iter()
                .zip(&offloads)
                .map(|((_, raw, _, _), offload)| offload.is_none().then_some(raw.as_slice()))
                .collect::<Vec<_>>(),
        )));
        let batch = RecordBatch::try_new(schema.clone(), arrays)?;
        InsertBuilder::new(output.to_string_lossy().as_ref())
            .with_params(&WriteParams {
                mode: WriteMode::Create,
                ..Default::default()
            })
            .execute_stream(RecordBatchIterator::new(vec![Ok(batch)], schema))
            .await
            .context("write compact JSONL Lance dataset")?;
        Ok(rows.len())
    }

    pub async fn export_path(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<usize> {
        let input = input.as_ref();
        let output = output.as_ref();
        let input_root = fs::canonicalize(input).context("canonicalize compact JSONL input")?;
        let output_root = fs::canonicalize(output).unwrap_or_else(|_| {
            output
                .parent()
                .and_then(|parent| fs::canonicalize(parent).ok())
                .unwrap_or_else(|| PathBuf::from("."))
                .join(output.file_name().unwrap_or_default())
        });
        ensure!(
            output_root != input_root && !output_root.starts_with(&input_root),
            "compact JSONL export output must be outside the input dataset"
        );
        let dataset = Dataset::open(input.to_string_lossy().as_ref()).await?;
        let (raw_idx, offload_idx) = validate_dataset_schema(&dataset)?;
        if output.exists() {
            fs::remove_dir_all(output)
                .with_context(|| format!("replace compact JSONL export {}", output.display()))?;
        }
        fs::create_dir_all(output)?;
        let stream = dataset.scan().scan_in_order(true).try_into_stream().await?;
        let mut rows = 0usize;
        let mut stream = stream;
        while let Some(batch) = stream.try_next().await? {
            let filenames = batch
                .column(batch.schema().index_of("filename")?)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| anyhow!("filename must be Utf8"))?;
            let raw = batch
                .column(raw_idx)
                .as_any()
                .downcast_ref::<LargeBinaryArray>()
                .context("_raw_ must be LargeBinary")?;
            let offloads = batch.column(offload_idx);
            for i in 0..batch.num_rows() {
                let relative = Path::new(filenames.value(i));
                ensure!(
                    !relative.is_absolute()
                        && !relative
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir)),
                    "compact JSONL filename escapes output"
                );
                let file = output.join(relative);
                if let Some(parent) = file.parent() {
                    fs::create_dir_all(parent)?;
                }
                let offload = json_text_at(offloads.as_ref(), i)?;
                let bytes = match (raw.is_null(i), offload.as_deref()) {
                    (false, None) => raw.value(i).to_vec(),
                    (true, Some(offload)) => {
                        let reference: CompactJsonlOffload = serde_json::from_str(offload)?;
                        let path = Path::new(&reference.path);
                        ensure!(
                            !path.is_absolute()
                                && !path
                                    .components()
                                    .any(|c| matches!(c, std::path::Component::ParentDir)),
                            "compact JSONL offload path escapes dataset"
                        );
                        let bytes = fs::read(input.join(path).join(&reference.key))
                            .context("read compact JSONL offload")?;
                        ensure!(
                            blake3::hash(&bytes).to_hex().as_str() == reference.key,
                            "compact JSONL offload digest mismatch for key {}",
                            reference.key
                        );
                        bytes
                    }
                    _ => anyhow::bail!(
                        "compact JSONL row must contain exactly one of _raw_ or _offload_"
                    ),
                };
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(file)?;
                f.write_all(&bytes)?;
                rows = rows
                    .checked_add(1)
                    .context("compact JSONL row count overflow")?;
            }
        }
        Ok(rows)
    }
}

fn validate_dataset_schema(dataset: &Dataset) -> Result<(usize, usize)> {
    ensure!(
        dataset
            .schema()
            .metadata
            .get(FORMAT_KEY)
            .map(String::as_str)
            == Some(FORMAT_NAME),
        "input is not a {FORMAT_NAME} dataset"
    );
    let mappings = dataset
        .schema()
        .metadata
        .get("pchronicle.columns")
        .context("compact JSONL dataset has no column mappings")?;
    let options = CompactJsonlOptions {
        columns: serde_json::from_str(mappings)?,
        offload_threshold: 0,
    };
    options.validate()?;
    let schema: Schema = dataset.schema().into();
    for name in ["id", "timestamp", "filename"] {
        let field = schema.field_with_name(name)?;
        ensure!(
            field.data_type() == &DataType::Utf8 && !field.is_nullable(),
            "compact JSONL column '{name}' must be non-null Utf8"
        );
    }
    for name in ["data", OFFLOAD_COLUMN] {
        let field = schema.field_with_name(name)?;
        ensure!(
            lance_arrow::json::is_json_field(field),
            "compact JSONL column '{name}' must be JSONB"
        );
    }
    let raw = schema.field_with_name(RAW_COLUMN)?;
    ensure!(
        raw.data_type() == &DataType::LargeBinary,
        "compact JSONL column '_raw_' must be LargeBinary"
    );
    Ok((
        schema.index_of(RAW_COLUMN)?,
        schema.index_of(OFFLOAD_COLUMN)?,
    ))
}

fn schema(options: &CompactJsonlOptions) -> Result<Arc<Schema>> {
    let mut fields = vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("filename", DataType::Utf8, false),
        json_field("data", false),
    ];
    fields.extend(
        options
            .columns
            .iter()
            .filter(|column| !matches!(column.name.as_str(), "id" | "timestamp"))
            .map(|column| json_field(&column.name, true)),
    );
    fields.push(json_field(OFFLOAD_COLUMN, true));
    fields.push(Field::new(RAW_COLUMN, DataType::LargeBinary, true));
    Ok(Arc::new(Schema::new_with_metadata(
        fields,
        std::collections::HashMap::from([
            (FORMAT_KEY.into(), FORMAT_NAME.into()),
            (
                "pchronicle.columns".into(),
                serde_json::to_string(&options.columns)?,
            ),
        ]),
    )))
}

fn encode_json_bytes(value: &str) -> Result<Vec<u8>> {
    encode_json(value).map_err(|error| anyhow!("encode compact JSON value: {error}"))
}

fn collect_jsonl(input: &Path) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(input)
        .with_context(|| format!("inspect compact JSONL input {}", input.display()))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "compact JSONL input must not be a symbolic link"
    );
    if metadata.is_file() {
        ensure!(
            input
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("jsonl")),
            "compact JSONL input must be .jsonl"
        );
        return Ok(vec![input.to_path_buf()]);
    }
    ensure!(
        metadata.is_dir(),
        "compact JSONL input must be a file or directory"
    );
    let mut out = Vec::new();
    let mut stack = vec![input.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let p = entry.path();
            if file_type.is_dir() {
                stack.push(p);
            } else if file_type.is_file()
                && p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("jsonl"))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn valid_path(path: &str) -> bool {
    if path == "$" {
        return true;
    }
    let Some(mut rest) = path.strip_prefix("$.") else {
        return false;
    };
    while !rest.is_empty() {
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty()
            || !name.chars().enumerate().all(|(i, c)| {
                c == '_' || c.is_ascii_alphanumeric() && (i > 0 || !c.is_ascii_digit())
            })
        {
            return false;
        }
        rest = &rest[end..];
        if let Some(index) = rest.strip_prefix('[') {
            let Some(end) = index.find(']') else {
                return false;
            };
            if index[..end].is_empty() || !index[..end].bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            rest = &index[end + 1..];
        }
        if let Some(next) = rest.strip_prefix('.') {
            rest = next;
        } else if !rest.is_empty() {
            return false;
        }
    }
    true
}

fn path_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
    {
        if part.is_empty() {
            continue;
        }
        let (name, index) = part.split_once('[').map_or((part, None), |(name, index)| {
            (name, index.strip_suffix(']'))
        });
        if !name.is_empty() {
            current = current.get(name)?;
        }
        if let Some(index) = index {
            current = current.get(index.parse::<usize>().ok()?)?;
        }
    }
    Some(current)
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_text_at(column: &dyn Array, row: usize) -> Result<Option<String>> {
    if let Some(values) = column.as_any().downcast_ref::<LargeBinaryArray>() {
        return Ok((!values.is_null(row)).then(|| decode_json(values.value(row))));
    }
    if let Some(values) = column.as_any().downcast_ref::<StringArray>() {
        return Ok((!values.is_null(row)).then(|| values.value(row).to_owned()));
    }
    anyhow::bail!("_offload_ must be JSONB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compact_jsonl_roundtrip_preserves_rows_and_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let input = temp.path().join("in");
        let dataset = temp.path().join("data.lance");
        let output = temp.path().join("out");
        fs::create_dir_all(input.join("nested"))?;
        fs::write(
            input.join("nested/a.jsonl"),
            b"{\"id\":\"x\",\"timestamp\":1,\"user\":{\"id\":7}}\n",
        )?;
        let options = CompactJsonlOptions {
            columns: vec![CompactJsonlColumn::new("user_id", "$.user.id")?],
            offload_threshold: 1,
        };
        assert_eq!(
            CompactJsonlStore::import_path(&input, &dataset, &options).await?,
            1
        );
        assert_eq!(CompactJsonlStore::records(&dataset).await?[0].id, "x");
        assert_eq!(
            CompactJsonlStore::read_record(&dataset, "x").await?,
            Some(serde_json::json!({"id": "x", "timestamp": 1, "user": {"id": 7}}))
        );
        assert_eq!(CompactJsonlStore::export_path(&dataset, &output).await?, 1);
        assert_eq!(
            fs::read(output.join("nested/a.jsonl"))?,
            b"{\"id\":\"x\",\"timestamp\":1,\"user\":{\"id\":7}}\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn required_columns_can_be_remapped_and_final_newline_is_exact() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let input = temp.path().join("input.jsonl");
        let dataset = temp.path().join("data.lance");
        let output = temp.path().join("out");
        let raw = b"{\"meta\":{\"key\":7,\"time\":\"now\"}}";
        fs::write(&input, raw)?;
        let options = CompactJsonlOptions {
            columns: vec![
                CompactJsonlColumn::new("id", "$.meta.key")?,
                CompactJsonlColumn::new("timestamp", "$.meta.time")?,
            ],
            offload_threshold: 0,
        };
        CompactJsonlStore::import_path(&input, &dataset, &options).await?;
        CompactJsonlStore::export_path(&dataset, &output).await?;
        assert_eq!(fs::read(output.join("input.jsonl"))?, raw);
        Ok(())
    }

    #[tokio::test]
    async fn missing_required_timestamp_rejects_the_snapshot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let input = temp.path().join("input.jsonl");
        fs::write(&input, b"{\"id\":\"only\"}\n")?;
        let error = CompactJsonlStore::import_path(
            &input,
            temp.path().join("data.lance"),
            &CompactJsonlOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("requires scalar timestamp"));
        Ok(())
    }

    #[tokio::test]
    async fn missing_id_uses_source_line_and_preserves_input() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let input = temp.path().join("events.jsonl");
        let dataset = temp.path().join("data.lance");
        let output = temp.path().join("out");
        let raw = b"{\"timestamp\":1,\"event\":\"start\"}\n{\"timestamp\":2,\"event\":\"end\"}\n";
        fs::write(&input, raw)?;

        CompactJsonlStore::import_path(&input, &dataset, &CompactJsonlOptions::default()).await?;
        let records = CompactJsonlStore::records(&dataset).await?;
        assert_eq!(
            records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            ["events.jsonl#1", "events.jsonl#2"]
        );
        CompactJsonlStore::export_path(&dataset, &output).await?;
        assert_eq!(fs::read(output.join("events.jsonl"))?, raw);
        Ok(())
    }
}
