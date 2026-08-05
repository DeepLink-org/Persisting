//! DataFusion datasource for ATIF JSON and JSONL inputs.
//!
//! File-backed ATIF inputs are validated with a bounded-memory pass and exposed
//! as repeatable DataFusion `StreamingTable`s. Every scan reopens the input,
//! normalizes one trajectory at a time through Storyline, and emits bounded
//! Arrow batches using the same schemas as the three-table Lance store.
//!
//! Explicit in-memory constructors retain `MemTable` behavior because their
//! callers have already materialized the complete input.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use datafusion::catalog::streaming::StreamingTable;
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::error::DataFusionError;
use datafusion::execution::TaskContext;
use datafusion::physical_plan::stream::RecordBatchReceiverStreamBuilder;
use datafusion::physical_plan::streaming::PartitionStream;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::prelude::SessionContext;
use lance::deps::arrow_array::{RecordBatch, RecordBatchIterator, RecordBatchReader};
use lance::deps::arrow_schema::{ArrowError, SchemaRef};

use crate::convert::atif_to_storyline;
use crate::{AtifTrajectory, StoryRunRow, StoryStepRow, StoryToolCallRow};

use super::{
    story_runs_arrow_schema, story_runs_to_batch, story_steps_arrow_schema, story_steps_to_batch,
    story_tool_calls_arrow_schema, story_tool_calls_to_batch, StorylineDataFusionTableNames,
    StorylineTableKind,
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
    runs: Arc<dyn TableProvider>,
    steps: Arc<dyn TableProvider>,
    tool_calls: Arc<dyn TableProvider>,
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
        let path = path.as_ref().to_path_buf();
        let stats = inspect_atif_input(&path)?;
        Ok(Self {
            runs: streaming_table(&path, StorylineTableKind::Runs, options.batch_size)?,
            steps: streaming_table(&path, StorylineTableKind::Steps, options.batch_size)?,
            tool_calls: streaming_table(&path, StorylineTableKind::ToolCalls, options.batch_size)?,
            document_count: stats.document_count,
            step_count: stats.step_count,
            tool_call_count: stats.tool_call_count,
        })
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

/// Bounded-memory ATIF reader.
///
/// JSONL/NDJSON inputs are decoded one non-empty line at a time. Directories
/// are traversed in stable path order and only the current file is open. A
/// regular `.json` file may contain one object or an array and is buffered per
/// file for compatibility; large corpora should use NDJSON.
pub struct AtifReader {
    files: std::vec::IntoIter<PathBuf>,
    current: Option<AtifFileReader>,
}

enum AtifFileReader {
    Lines {
        path: PathBuf,
        lines: Lines<BufReader<File>>,
        line_number: usize,
    },
    Documents(std::vec::IntoIter<AtifTrajectory>),
}

impl AtifReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let files = if path.is_file() {
            vec![path.to_path_buf()]
        } else if path.is_dir() {
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
            files
        } else {
            anyhow::bail!("ATIF datasource path does not exist: {}", path.display());
        };
        Ok(Self {
            files: files.into_iter(),
            current: None,
        })
    }

    fn open_file(path: PathBuf) -> Result<AtifFileReader> {
        match path.extension().and_then(|value| value.to_str()) {
            Some("jsonl" | "ndjson") => {
                let file = File::open(&path)
                    .with_context(|| format!("open ATIF datasource {}", path.display()))?;
                Ok(AtifFileReader::Lines {
                    path,
                    lines: BufReader::new(file).lines(),
                    line_number: 0,
                })
            }
            _ => {
                let input = std::fs::read_to_string(&path)
                    .with_context(|| format!("read ATIF datasource {}", path.display()))?;
                let documents = parse_documents(&input)
                    .with_context(|| format!("parse ATIF datasource {}", path.display()))?;
                Ok(AtifFileReader::Documents(documents.into_iter()))
            }
        }
    }
}

impl Iterator for AtifReader {
    type Item = Result<AtifTrajectory>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(current) = &mut self.current {
                match current {
                    AtifFileReader::Documents(documents) => {
                        if let Some(document) = documents.next() {
                            return Some(Ok(document));
                        }
                    }
                    AtifFileReader::Lines {
                        path,
                        lines,
                        line_number,
                    } => {
                        for line in lines.by_ref() {
                            *line_number += 1;
                            let line = match line {
                                Ok(line) => line,
                                Err(error) => {
                                    return Some(Err(error).with_context(|| {
                                        format!(
                                            "read ATIF datasource {} line {}",
                                            path.display(),
                                            line_number
                                        )
                                    }));
                                }
                            };
                            if line.trim().is_empty() {
                                continue;
                            }
                            return Some(
                                AtifTrajectory::from_json_str(line.trim())
                                    .map_err(anyhow::Error::from)
                                    .with_context(|| {
                                        format!(
                                            "parse ATIF datasource {} line {}",
                                            path.display(),
                                            line_number
                                        )
                                    }),
                            );
                        }
                    }
                }
                self.current = None;
            }

            let path = self.files.next()?;
            match Self::open_file(path) {
                Ok(reader) => self.current = Some(reader),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct AtifInputStats {
    pub document_count: usize,
    pub step_count: usize,
    pub tool_call_count: usize,
}

pub(crate) struct AtifTableStreams {
    pub runs: Box<dyn RecordBatchReader + Send>,
    pub steps: Box<dyn RecordBatchReader + Send>,
    pub tool_calls: Box<dyn RecordBatchReader + Send>,
    pub completion: std::thread::JoinHandle<Result<AtifInputStats>>,
}

pub(crate) fn inspect_atif_input(path: &Path) -> Result<AtifInputStats> {
    let mut stats = AtifInputStats::default();
    // Retain only logical keys, not trajectory payloads. This preserves the
    // three-table uniqueness contract while keeping message/tool content out
    // of memory for file-backed inputs.
    let mut session_ids = HashSet::new();
    for trajectory in AtifReader::open(path)? {
        let trajectory = trajectory?;
        trajectory.validate().map_err(anyhow::Error::from)?;
        let story = atif_to_storyline(&trajectory).map_err(anyhow::Error::from)?;
        let tables = crate::split_storyline(&story).map_err(anyhow::Error::from)?;
        if !session_ids.insert(tables.run.session_id.clone()) {
            anyhow::bail!("duplicate ATIF session_id '{}'", tables.run.session_id);
        }
        stats.document_count += 1;
        stats.step_count += tables.steps.len();
        stats.tool_call_count += tables.tool_calls.len();
    }
    if stats.document_count == 0 {
        anyhow::bail!("ATIF datasource requires at least one trajectory");
    }
    Ok(stats)
}

#[derive(Debug)]
struct AtifPartitionStream {
    path: PathBuf,
    kind: StorylineTableKind,
    schema: SchemaRef,
    batch_size: usize,
}

impl PartitionStream for AtifPartitionStream {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<TaskContext>) -> SendableRecordBatchStream {
        let mut builder = RecordBatchReceiverStreamBuilder::new(self.schema.clone(), 2);
        let tx = builder.tx();
        let path = self.path.clone();
        let kind = self.kind;
        let batch_size = self.batch_size;
        builder.spawn_blocking(move || {
            let reader = AtifReader::open(path).map_err(datafusion_error)?;
            for batch in AtifBatchIterator::new(reader, kind, batch_size) {
                if tx.blocking_send(batch.map_err(datafusion_error)).is_err() {
                    break;
                }
            }
            Ok(())
        });
        builder.build()
    }
}

struct AtifBatchIterator {
    reader: AtifReader,
    kind: StorylineTableKind,
    batch_size: usize,
    runs: VecDeque<StoryRunRow>,
    steps: VecDeque<StoryStepRow>,
    tool_calls: VecDeque<StoryToolCallRow>,
    finished: bool,
}

pub(crate) fn atif_table_streams(path: &Path, batch_size: usize) -> Result<AtifTableStreams> {
    let reader = AtifReader::open(path)?;
    let (run_tx, run_rx) = std::sync::mpsc::sync_channel(2);
    let (step_tx, step_rx) = std::sync::mpsc::sync_channel(2);
    let (tool_call_tx, tool_call_rx) = std::sync::mpsc::sync_channel(2);
    let completion = std::thread::spawn(move || {
        let mut stats = AtifInputStats::default();
        let mut session_ids = HashSet::new();
        let mut runs = Vec::with_capacity(batch_size);
        let mut steps = Vec::with_capacity(batch_size);
        let mut tool_calls = Vec::with_capacity(batch_size);
        for trajectory in reader {
            let trajectory = trajectory?;
            let story = atif_to_storyline(&trajectory).map_err(anyhow::Error::from)?;
            let tables = crate::split_storyline(&story).map_err(anyhow::Error::from)?;
            if !session_ids.insert(tables.run.session_id.clone()) {
                anyhow::bail!("duplicate ATIF session_id '{}'", tables.run.session_id);
            }
            stats.document_count += 1;
            stats.step_count += tables.steps.len();
            stats.tool_call_count += tables.tool_calls.len();
            runs.push(tables.run);
            steps.extend(tables.steps);
            tool_calls.extend(tables.tool_calls);
            flush_rows(&mut runs, batch_size, false, story_runs_to_batch, &run_tx)?;
            flush_rows(
                &mut steps,
                batch_size,
                false,
                story_steps_to_batch,
                &step_tx,
            )?;
            flush_rows(
                &mut tool_calls,
                batch_size,
                false,
                story_tool_calls_to_batch,
                &tool_call_tx,
            )?;
        }
        anyhow::ensure!(
            stats.document_count > 0,
            "ATIF datasource requires at least one trajectory"
        );
        flush_rows(&mut runs, batch_size, true, story_runs_to_batch, &run_tx)?;
        flush_rows(&mut steps, batch_size, true, story_steps_to_batch, &step_tx)?;
        flush_rows(
            &mut tool_calls,
            batch_size,
            true,
            story_tool_calls_to_batch,
            &tool_call_tx,
        )?;
        if stats.step_count == 0 {
            step_tx
                .send(Ok(story_steps_to_batch(&[])?))
                .map_err(|_| anyhow::anyhow!("ATIF steps stream consumer closed"))?;
        }
        if stats.tool_call_count == 0 {
            tool_call_tx
                .send(Ok(story_tool_calls_to_batch(&[])?))
                .map_err(|_| anyhow::anyhow!("ATIF tool_calls stream consumer closed"))?;
        }
        Ok(stats)
    });
    Ok(AtifTableStreams {
        runs: Box::new(RecordBatchIterator::new(run_rx, story_runs_arrow_schema())),
        steps: Box::new(RecordBatchIterator::new(
            step_rx,
            story_steps_arrow_schema(),
        )),
        tool_calls: Box::new(RecordBatchIterator::new(
            tool_call_rx,
            story_tool_calls_arrow_schema(),
        )),
        completion,
    })
}

fn flush_rows<T>(
    rows: &mut Vec<T>,
    batch_size: usize,
    flush_remainder: bool,
    encode: fn(&[T]) -> Result<RecordBatch>,
    sender: &std::sync::mpsc::SyncSender<std::result::Result<RecordBatch, ArrowError>>,
) -> Result<()> {
    while rows.len() >= batch_size || (flush_remainder && !rows.is_empty()) {
        let count = rows.len().min(batch_size);
        let remainder = rows.split_off(count);
        let batch_rows = std::mem::replace(rows, remainder);
        sender
            .send(Ok(encode(&batch_rows)?))
            .map_err(|_| anyhow::anyhow!("ATIF table stream consumer closed"))?;
    }
    Ok(())
}

impl AtifBatchIterator {
    fn new(reader: AtifReader, kind: StorylineTableKind, batch_size: usize) -> Self {
        Self {
            reader,
            kind,
            batch_size,
            runs: VecDeque::new(),
            steps: VecDeque::new(),
            tool_calls: VecDeque::new(),
            finished: false,
        }
    }

    fn pending_len(&self) -> usize {
        match self.kind {
            StorylineTableKind::Runs => self.runs.len(),
            StorylineTableKind::Steps => self.steps.len(),
            StorylineTableKind::ToolCalls => self.tool_calls.len(),
        }
    }

    fn push_trajectory(&mut self, trajectory: AtifTrajectory) -> Result<()> {
        let story = atif_to_storyline(&trajectory).map_err(anyhow::Error::from)?;
        let tables = crate::split_storyline(&story).map_err(anyhow::Error::from)?;
        match self.kind {
            StorylineTableKind::Runs => self.runs.push_back(tables.run),
            StorylineTableKind::Steps => self.steps.extend(tables.steps),
            StorylineTableKind::ToolCalls => self.tool_calls.extend(tables.tool_calls),
        }
        Ok(())
    }

    fn encode_pending(&mut self) -> Result<RecordBatch> {
        let count = self.pending_len().min(self.batch_size);
        match self.kind {
            StorylineTableKind::Runs => {
                let rows = self.runs.drain(..count).collect::<Vec<_>>();
                story_runs_to_batch(&rows)
            }
            StorylineTableKind::Steps => {
                let rows = self.steps.drain(..count).collect::<Vec<_>>();
                story_steps_to_batch(&rows)
            }
            StorylineTableKind::ToolCalls => {
                let rows = self.tool_calls.drain(..count).collect::<Vec<_>>();
                story_tool_calls_to_batch(&rows)
            }
        }
    }
}

impl Iterator for AtifBatchIterator {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.finished && self.pending_len() < self.batch_size {
            match self.reader.next() {
                Some(Ok(trajectory)) => {
                    if let Err(error) = self.push_trajectory(trajectory) {
                        self.finished = true;
                        return Some(Err(error));
                    }
                }
                Some(Err(error)) => {
                    self.finished = true;
                    return Some(Err(error));
                }
                None => self.finished = true,
            }
        }
        if self.pending_len() > 0 {
            Some(self.encode_pending())
        } else {
            None
        }
    }
}

fn streaming_table(
    path: &Path,
    kind: StorylineTableKind,
    batch_size: usize,
) -> Result<Arc<dyn TableProvider>> {
    let schema = match kind {
        StorylineTableKind::Runs => story_runs_arrow_schema(),
        StorylineTableKind::Steps => story_steps_arrow_schema(),
        StorylineTableKind::ToolCalls => story_tool_calls_arrow_schema(),
    };
    let partition: Arc<dyn PartitionStream> = Arc::new(AtifPartitionStream {
        path: path.to_path_buf(),
        kind,
        schema: schema.clone(),
        batch_size,
    });
    Ok(Arc::new(
        StreamingTable::try_new(schema, vec![partition])
            .context("build streaming ATIF DataFusion table")?,
    ))
}

fn datafusion_error(error: anyhow::Error) -> DataFusionError {
    DataFusionError::Execution(format!("{error:#}"))
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

/// Load and validate ATIF documents for callers that partition work before
/// building a query engine.
pub fn load_atif_trajectories(path: impl AsRef<Path>) -> Result<Vec<AtifTrajectory>> {
    AtifReader::open(path)?.collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif")
    }

    #[test]
    fn streaming_step_batches_respect_the_configured_bound() {
        let reader = AtifReader::open(fixture_root()).unwrap();
        let batches = AtifBatchIterator::new(reader, StorylineTableKind::Steps, 3)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(batches.iter().all(|batch| batch.num_rows() <= 3));
        assert_eq!(
            batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
            118
        );
    }
}
