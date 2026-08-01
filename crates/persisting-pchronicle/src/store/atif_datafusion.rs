//! DataFusion datasource for ATIF JSON and JSONL inputs.
//!
//! ATIF documents are parsed once, normalized through Storyline, converted to
//! the same Arrow schemas as the three-table Lance store, and retained in
//! DataFusion `MemTable`s for repeated SQL queries.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::prelude::SessionContext;
use lance::deps::arrow_array::RecordBatch;
use lance::deps::arrow_schema::SchemaRef;

use crate::convert::atif_to_storyline;
use crate::AtifTrajectory;

use super::{
    story_runs_arrow_schema, story_runs_to_batch, story_steps_arrow_schema, story_steps_to_batch,
    story_tool_calls_arrow_schema, story_tool_calls_to_batch, StorylineDataFusionTableNames,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtifDataSourceOptions {
    /// Maximum rows per Arrow batch/partition.
    pub batch_size: usize,
}

impl Default for AtifDataSourceOptions {
    fn default() -> Self {
        Self { batch_size: 8192 }
    }
}

#[derive(Debug)]
pub struct AtifDataSource {
    runs: Arc<MemTable>,
    steps: Arc<MemTable>,
    tool_calls: Arc<MemTable>,
    document_count: usize,
    step_count: usize,
    tool_call_count: usize,
}

impl AtifDataSource {
    /// Open a single ATIF JSON/JSONL file, a directory of such files, or a
    /// directory containing ATIF JSON, JSONL, or NDJSON documents.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, AtifDataSourceOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: AtifDataSourceOptions,
    ) -> Result<Self> {
        validate_options(options)?;
        let trajectories = load_path(path.as_ref())?;
        Self::from_trajectories_with_options(&trajectories, options)
    }

    /// Parse a single ATIF object, an array of objects, or JSONL/NDJSON.
    pub fn from_json(input: &str) -> Result<Self> {
        Self::from_json_with_options(input, AtifDataSourceOptions::default())
    }

    pub fn from_json_with_options(input: &str, options: AtifDataSourceOptions) -> Result<Self> {
        validate_options(options)?;
        let trajectories = parse_documents(input)?;
        Self::from_trajectories_with_options(&trajectories, options)
    }

    pub fn from_trajectories(trajectories: &[AtifTrajectory]) -> Result<Self> {
        Self::from_trajectories_with_options(trajectories, AtifDataSourceOptions::default())
    }

    pub fn from_trajectories_with_options(
        trajectories: &[AtifTrajectory],
        options: AtifDataSourceOptions,
    ) -> Result<Self> {
        validate_options(options)?;
        if trajectories.is_empty() {
            anyhow::bail!("ATIF datasource requires at least one trajectory");
        }

        let mut session_ids = HashSet::with_capacity(trajectories.len());
        let mut runs = Vec::with_capacity(trajectories.len());
        let mut steps = Vec::new();
        let mut tool_calls = Vec::new();
        for trajectory in trajectories {
            trajectory.validate().map_err(anyhow::Error::from)?;
            let story = atif_to_storyline(trajectory).map_err(anyhow::Error::from)?;
            let tables = crate::split_storyline(&story).map_err(anyhow::Error::from)?;
            if !session_ids.insert(tables.run.session_id.clone()) {
                anyhow::bail!("duplicate ATIF session_id '{}'", tables.run.session_id);
            }
            runs.push(tables.run);
            steps.extend(tables.steps);
            tool_calls.extend(tables.tool_calls);
        }
        runs.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        steps.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
        });
        tool_calls.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
                .then(a.call_index.cmp(&b.call_index))
        });

        let document_count = runs.len();
        let step_count = steps.len();
        let tool_call_count = tool_calls.len();
        Ok(Self {
            runs: Arc::new(mem_table(
                story_runs_arrow_schema(),
                &runs,
                options.batch_size,
                story_runs_to_batch,
            )?),
            steps: Arc::new(mem_table(
                story_steps_arrow_schema(),
                &steps,
                options.batch_size,
                story_steps_to_batch,
            )?),
            tool_calls: Arc::new(mem_table(
                story_tool_calls_arrow_schema(),
                &tool_calls,
                options.batch_size,
                story_tool_calls_to_batch,
            )?),
            document_count,
            step_count,
            tool_call_count,
        })
    }

    pub fn document_count(&self) -> usize {
        self.document_count
    }

    pub fn step_count(&self) -> usize {
        self.step_count
    }

    pub fn tool_call_count(&self) -> usize {
        self.tool_call_count
    }

    pub fn register(&self, context: &SessionContext) -> Result<()> {
        self.register_as(context, &StorylineDataFusionTableNames::default())
    }

    pub fn register_as(
        &self,
        context: &SessionContext,
        names: &StorylineDataFusionTableNames,
    ) -> Result<()> {
        validate_table_names(names)?;
        register(context, &names.runs, self.runs.clone())?;
        register(context, &names.steps, self.steps.clone())?;
        register(context, &names.tool_calls, self.tool_calls.clone())?;
        Ok(())
    }

    pub fn session_context(&self) -> Result<SessionContext> {
        let context = SessionContext::new();
        self.register(&context)?;
        Ok(context)
    }
}

fn validate_options(options: AtifDataSourceOptions) -> Result<()> {
    if options.batch_size == 0 {
        anyhow::bail!("ATIF datasource batch_size must be greater than zero");
    }
    Ok(())
}

fn mem_table<T>(
    schema: SchemaRef,
    rows: &[T],
    batch_size: usize,
    encode: fn(&[T]) -> Result<RecordBatch>,
) -> Result<MemTable> {
    let partitions = if rows.is_empty() {
        vec![Vec::new()]
    } else {
        rows.chunks(batch_size)
            .map(|chunk| encode(chunk).map(|batch| vec![batch]))
            .collect::<Result<Vec<_>>>()?
    };
    MemTable::try_new(schema, partitions).context("build ATIF DataFusion MemTable")
}

fn register(context: &SessionContext, name: &str, provider: Arc<dyn TableProvider>) -> Result<()> {
    context
        .register_table(name, provider)
        .with_context(|| format!("register ATIF DataFusion table '{name}'"))?;
    Ok(())
}

fn validate_table_names(names: &StorylineDataFusionTableNames) -> Result<()> {
    let values = [&names.runs, &names.steps, &names.tool_calls];
    if values.iter().any(|name| name.trim().is_empty()) {
        anyhow::bail!("DataFusion table names must not be empty");
    }
    if names.runs == names.steps
        || names.runs == names.tool_calls
        || names.steps == names.tool_calls
    {
        anyhow::bail!("DataFusion table names must be distinct");
    }
    Ok(())
}

fn load_path(path: &Path) -> Result<Vec<AtifTrajectory>> {
    if path.is_file() {
        let input = std::fs::read_to_string(path)
            .with_context(|| format!("read ATIF datasource {}", path.display()))?;
        return parse_documents(&input)
            .with_context(|| format!("parse ATIF datasource {}", path.display()));
    }
    if !path.is_dir() {
        anyhow::bail!("ATIF datasource path does not exist: {}", path.display());
    }
    let mut files = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| {
        matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("json" | "jsonl" | "ndjson")
        )
    });
    files.sort();
    if files.is_empty() {
        anyhow::bail!(
            "ATIF datasource directory contains no JSON files: {}",
            path.display()
        );
    }
    let mut trajectories = Vec::new();
    for file in files {
        trajectories.extend(load_path(&file)?);
    }
    Ok(trajectories)
}

fn parse_documents(input: &str) -> Result<Vec<AtifTrajectory>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("ATIF input is empty");
    }
    if let Ok(trajectory) = serde_json::from_str::<AtifTrajectory>(trimmed) {
        trajectory.validate().map_err(anyhow::Error::from)?;
        return Ok(vec![trajectory]);
    }
    if let Ok(trajectories) = serde_json::from_str::<Vec<AtifTrajectory>>(trimmed) {
        for trajectory in &trajectories {
            trajectory.validate().map_err(anyhow::Error::from)?;
        }
        return Ok(trajectories);
    }
    trimmed
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            AtifTrajectory::from_json_str(line)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("parse ATIF JSONL line {}", index + 1))
        })
        .collect()
}
