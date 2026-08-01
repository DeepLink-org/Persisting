//! Public SQL query engine over Lance or ATIF sources.

use std::path::Path;

use anyhow::{Context, Result};
use datafusion::arrow::json::LineDelimitedWriter;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::dataframe::DataFrame;
use datafusion::prelude::SessionContext;
use datafusion::sql::parser::{DFParser, Statement as DataFusionStatement};
use datafusion::sql::sqlparser::ast::Statement as SqlStatement;

use super::{AtifDataSource, LanceStorylineStore, StorylineDataSource};

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
        let store = LanceStorylineStore::open(root).await?;
        let source = StorylineDataSource::from_store(&store).await?;
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
