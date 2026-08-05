//! Public SQL query engine over Lance or ATIF sources.

use std::path::Path;

use anyhow::{Context, Result};
use datafusion::arrow::json::LineDelimitedWriter;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::dataframe::DataFrame;
use datafusion::prelude::{CsvReadOptions, JsonReadOptions, SessionContext};
use datafusion::sql::parser::{DFParser, Statement as DataFusionStatement};
use datafusion::sql::sqlparser::ast::Statement as SqlStatement;

use super::{AtifDataSource, StorylineDataSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChronicleQueryBackend {
    Lance {
        generation: String,
    },
    Atif {
        documents: usize,
        steps: usize,
        tool_calls: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTableFormat {
    Csv,
    /// One JSON array containing zero or more objects.
    Json,
    /// Newline-delimited JSON (`.jsonl` or `.ndjson`).
    JsonLines,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTableSpec {
    pub name: String,
    pub format: ExternalTableFormat,
    pub path: String,
}

impl ExternalTableSpec {
    pub fn new(
        name: impl Into<String>,
        format: ExternalTableFormat,
        path: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            format,
            path: path.into(),
        }
    }
}

/// Read-only SQL engine exposing the same normalized tables for Lance and ATIF.
pub struct ChronicleQueryEngine {
    context: SessionContext,
    backend: ChronicleQueryBackend,
}

impl std::fmt::Debug for ChronicleQueryEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChronicleQueryEngine")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl ChronicleQueryEngine {
    pub async fn open_lance(root: impl AsRef<Path>) -> Result<Self> {
        let source = StorylineDataSource::open(root).await?;
        Self::from_lance_source(source)
    }

    /// Open a Lance store from a local path or object-store URI such as S3.
    pub async fn open_lance_uri(root: impl AsRef<str>) -> Result<Self> {
        let source = StorylineDataSource::open_uri(root).await?;
        Self::from_lance_source(source)
    }

    pub fn open_atif(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_atif_source(AtifDataSource::open(path)?)
    }

    pub fn from_lance_source(source: StorylineDataSource) -> Result<Self> {
        let generation = source.generation().to_string();
        let context = source.session_context()?;
        Ok(Self {
            context,
            backend: ChronicleQueryBackend::Lance { generation },
        })
    }

    pub fn from_atif_source(source: AtifDataSource) -> Result<Self> {
        let backend = ChronicleQueryBackend::Atif {
            documents: source.document_count(),
            steps: source.step_count(),
            tool_calls: source.tool_call_count(),
        };
        let context = source.session_context()?;
        Ok(Self { context, backend })
    }

    pub fn context(&self) -> &SessionContext {
        &self.context
    }

    pub fn backend(&self) -> &ChronicleQueryBackend {
        &self.backend
    }

    /// Register a read-only file source in the same DataFusion context as the
    /// normalized `runs`, `steps`, and `tool_calls` tables.
    pub async fn register_external_table(&self, spec: &ExternalTableSpec) -> Result<()> {
        validate_external_table_spec(spec)?;
        anyhow::ensure!(
            !self
                .context
                .table_exist(spec.name.as_str())
                .context("inspect DataFusion table catalog")?,
            "DataFusion table '{}' is already registered",
            spec.name
        );
        match spec.format {
            ExternalTableFormat::Csv => {
                self.context
                    .register_csv(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        CsvReadOptions::new(),
                    )
                    .await
            }
            ExternalTableFormat::Json => {
                self.context
                    .register_json(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        JsonReadOptions::default().newline_delimited(false),
                    )
                    .await
            }
            ExternalTableFormat::JsonLines => {
                let extension = json_lines_extension(&spec.path);
                self.context
                    .register_json(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        JsonReadOptions::default().file_extension(extension),
                    )
                    .await
            }
        }
        .with_context(|| {
            format!(
                "register external DataFusion table '{}' from {}",
                spec.name, spec.path
            )
        })
    }

    /// Build a lazy DataFusion DataFrame for callers that need plan inspection
    /// or further DataFrame transformations.
    pub async fn dataframe(&self, sql: &str) -> Result<DataFrame> {
        ensure_read_only_query(sql)?;
        self.context
            .sql(sql)
            .await
            .with_context(|| format!("plan pChronicle SQL: {sql}"))
    }

    /// Execute SQL and collect Arrow record batches.
    pub async fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        self.dataframe(sql)
            .await?
            .collect()
            .await
            .with_context(|| format!("execute pChronicle SQL: {sql}"))
    }

    /// Execute SQL and encode result rows as JSONL.
    pub async fn query_jsonl(&self, sql: &str) -> Result<String> {
        let batches = self.query(sql).await?;
        let mut output = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut output);
            for batch in &batches {
                writer
                    .write(batch)
                    .context("encode SQL result batch as JSONL")?;
            }
            writer.finish().context("finish SQL JSONL output")?;
        }
        String::from_utf8(output).context("DataFusion JSONL output is not UTF-8")
    }
}

fn validate_external_table_spec(spec: &ExternalTableSpec) -> Result<()> {
    let mut characters = spec.name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    anyhow::ensure!(
        valid_start && valid_rest,
        "external table name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
        spec.name
    );
    anyhow::ensure!(
        !spec.path.trim().is_empty(),
        "external table '{}' path must not be empty",
        spec.name
    );
    Ok(())
}

fn json_lines_extension(path: &str) -> &str {
    let path_without_query = path.split(['?', '#']).next().unwrap_or(path);
    if path_without_query.ends_with(".ndjson") {
        ".ndjson"
    } else {
        ".jsonl"
    }
}

fn ensure_read_only_query(sql: &str) -> Result<()> {
    let statements = DFParser::parse_sql(sql).context("parse pChronicle SQL")?;
    anyhow::ensure!(
        statements.len() == 1,
        "pChronicle query accepts exactly one SQL statement"
    );
    anyhow::ensure!(
        is_read_only_statement(&statements[0]),
        "pChronicle query only accepts SELECT/VALUES/DESCRIBE/EXPLAIN statements"
    );
    Ok(())
}

fn is_read_only_statement(statement: &DataFusionStatement) -> bool {
    match statement {
        DataFusionStatement::Statement(statement) => is_read_only_sql_statement(statement),
        DataFusionStatement::Explain(explain) => is_read_only_statement(&explain.statement),
        DataFusionStatement::CreateExternalTable(_)
        | DataFusionStatement::CopyTo(_)
        | DataFusionStatement::Reset(_) => false,
    }
}

fn is_read_only_sql_statement(statement: &SqlStatement) -> bool {
    match statement {
        SqlStatement::Query(_) | SqlStatement::ExplainTable { .. } => true,
        SqlStatement::Explain { statement, .. } => is_read_only_sql_statement(statement),
        _ => false,
    }
}
