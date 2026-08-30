//! Query-only, format-shaped document tables.
//!
//! Each peripheral format is exposed as a deliberately small two-column
//! table: a stable `id` and the original format document in Lance JSONB
//! (`data`).  Keeping the JSON extension metadata on the Arrow field is
//! important: Lance's JSON UDFs can then address paths without first turning
//! the value back into text.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::datasource::MemTable;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use lance::deps::arrow_array::{LargeBinaryArray, RecordBatch, StringArray};
use lance::deps::arrow_schema::{Field, Schema as ArrowSchema, SchemaRef};
use lance_arrow::json::{encode_json, json_field};

use crate::format::DocumentFormat;

/// Names are intentionally SQL-safe.  Hyphenated `DocumentFormat::as_str`
/// values would require quoting (`dataset.openai-msg`) in every query.
pub(crate) fn table_name(format: DocumentFormat) -> Option<&'static str> {
    Some(match format {
        DocumentFormat::CanonicalEvent => "events",
        DocumentFormat::Atif => "atif",
        DocumentFormat::Storyline => "storyline",
        DocumentFormat::Actf => "actf",
        DocumentFormat::OpenaiMsg => "openai_msg",
        DocumentFormat::Codex => "codex",
        DocumentFormat::ClaudeCode => "claude",
        DocumentFormat::AgenticMd => "markdown",
        DocumentFormat::StorylineLance => return None,
    })
}

pub(crate) fn formats() -> impl Iterator<Item = DocumentFormat> {
    // Canonical Event already owns the stable `events` table (with its
    // denormalized columns and payload_json); registering another provider
    // under the same name would make catalog registration fail.
    [
        DocumentFormat::Atif,
        DocumentFormat::Storyline,
        DocumentFormat::Actf,
        DocumentFormat::OpenaiMsg,
        DocumentFormat::Codex,
        DocumentFormat::ClaudeCode,
        DocumentFormat::AgenticMd,
    ]
    .into_iter()
}

pub(crate) fn schema() -> SchemaRef {
    Arc::new(ArrowSchema::new(vec![
        Field::new("id", lance::deps::arrow_schema::DataType::Utf8, false),
        json_field("data", false),
    ]))
}

/// Build a DataFusion provider while preserving Lance's JSONB extension type.
pub(crate) fn provider(rows: &[(String, String)]) -> Result<Arc<dyn TableProvider>> {
    let ids = StringArray::from(rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>());
    let data = rows
        .iter()
        .map(|(_, value)| encode_json(value).map_err(|error| anyhow::anyhow!(error.to_string())))
        .collect::<Result<Vec<_>>>()?;
    let data = LargeBinaryArray::from(data.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let batch = RecordBatch::try_new(schema(), vec![Arc::new(ids), Arc::new(data)])
        .context("build format virtual table batch")?;
    Ok(Arc::new(MemTable::try_new(schema(), vec![vec![batch]])?))
}

/// Lazy file-backed provider.  Registering a virtual table must not parse a
/// whole directory; decoding is deferred until the table is actually scanned.
#[derive(Debug)]
pub(crate) struct FileProvider {
    format: DocumentFormat,
    manifest: Arc<super::LocalQueryManifest>,
    max_file_bytes: u64,
}

impl FileProvider {
    pub(crate) fn new(
        format: DocumentFormat,
        manifest: Arc<super::LocalQueryManifest>,
        max_file_bytes: u64,
    ) -> Self {
        Self {
            format,
            manifest,
            max_file_bytes,
        }
    }
}

#[async_trait]
impl TableProvider for FileProvider {
    fn schema(&self) -> SchemaRef {
        schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        // The format virtual table has only two columns and no file column;
        // evaluate ordinary DataFusion filters above the materialized scan.
        let rows = super::document_source::virtual_rows_for_files(
            self.format,
            &self.manifest,
            self.max_file_bytes,
            None,
        )
        .map_err(|error| DataFusionError::Execution(error.to_string()))?;
        let table =
            provider(&rows).map_err(|error| DataFusionError::Execution(error.to_string()))?;
        table.scan(state, projection, filters, limit).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|_| TableProviderFilterPushDown::Inexact)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_table_names_use_query_friendly_aliases() {
        assert_eq!(table_name(DocumentFormat::CanonicalEvent), Some("events"));
        assert_eq!(table_name(DocumentFormat::AgenticMd), Some("markdown"));
        assert_eq!(table_name(DocumentFormat::ClaudeCode), Some("claude"));
        assert_eq!(table_name(DocumentFormat::Atif), Some("atif"));
    }
}
