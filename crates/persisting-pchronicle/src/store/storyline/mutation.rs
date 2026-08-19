use super::*;

#[derive(Default)]
pub(super) struct StorylineChunkState {
    pub(super) all_document_ids: HashSet<String>,
    pub(super) pending: Option<StorylineDocument>,
}

pub(super) struct StorylineStreamChunk {
    pub(super) document_ids: HashSet<String>,
    pub(super) runs: Vec<StoryRunRow>,
    pub(super) steps: Vec<StoryStepRow>,
    pub(super) tool_calls: Vec<StoryToolCallRow>,
}

pub(super) fn next_storyline_stream_chunk<I>(
    iterator: &mut I,
    state: &mut StorylineChunkState,
    next_storage_ordinal: &mut i64,
    options: StorylineContentOptions,
) -> Result<Option<StorylineStreamChunk>>
where
    I: Iterator<Item = Result<StorylineDocument>>,
{
    let mut document_ids = HashSet::with_capacity(STREAM_IMPORT_STORIES);
    let mut runs = Vec::with_capacity(STREAM_IMPORT_STORIES);
    let mut steps = Vec::new();
    let mut tool_calls = Vec::new();
    let mut chunk_rows = 0usize;
    let mut chunk_bytes = 0usize;
    while runs.len() < STREAM_IMPORT_STORIES {
        let story = match state.pending.take() {
            Some(story) => story,
            None => {
                let Some(story) = iterator.next() else {
                    break;
                };
                story?
            }
        };
        let document_bytes = serialized_document_bytes(&story)?;
        let mut tables = split_storyline(&story)?;
        let document_rows = 1usize
            .checked_add(tables.steps.len())
            .and_then(|rows| rows.checked_add(tables.tool_calls.len()))
            .context("Storyline document row count overflow")?;
        enforce_limit(
            "max_document_rows",
            document_rows,
            options.max_document_rows,
        )?;
        enforce_limit(
            "max_document_bytes",
            document_bytes,
            options.max_document_bytes,
        )?;
        enforce_limit("max_chunk_rows", document_rows, options.max_chunk_rows)?;
        enforce_limit("max_chunk_bytes", document_bytes, options.max_chunk_bytes)?;
        let document_id = tables.run.document_id.clone();
        if state.all_document_ids.contains(&document_id) {
            anyhow::bail!("duplicate document_id '{document_id}' in Storyline stream");
        }
        let next_document_count = state
            .all_document_ids
            .len()
            .checked_add(1)
            .context("Storyline import document count overflow")?;
        enforce_limit(
            "max_import_documents",
            next_document_count,
            options.max_import_documents,
        )?;
        let next_chunk_rows = chunk_rows
            .checked_add(document_rows)
            .context("Storyline chunk row count overflow")?;
        let next_chunk_bytes = chunk_bytes
            .checked_add(document_bytes)
            .context("Storyline chunk byte count overflow")?;
        if !runs.is_empty()
            && (exceeds_limit(next_chunk_rows, options.max_chunk_rows)
                || exceeds_limit(next_chunk_bytes, options.max_chunk_bytes))
        {
            state.pending = Some(story);
            break;
        }
        tables.run.storage_ordinal = *next_storage_ordinal;
        *next_storage_ordinal = next_storage_ordinal
            .checked_add(1)
            .context("Storyline storage ordinal overflow")?;
        state.all_document_ids.insert(document_id.clone());
        document_ids.insert(document_id);
        runs.push(tables.run);
        steps.extend(tables.steps);
        tool_calls.extend(tables.tool_calls);
        chunk_rows = next_chunk_rows;
        chunk_bytes = next_chunk_bytes;
    }
    if runs.is_empty() {
        return Ok(None);
    }
    sort_rows(&mut runs, &mut steps, &mut tool_calls);
    Ok(Some(StorylineStreamChunk {
        document_ids,
        runs,
        steps,
        tool_calls,
    }))
}

fn enforce_limit(name: &str, actual: usize, limit: Option<usize>) -> Result<()> {
    if let Some(limit) = limit {
        anyhow::ensure!(
            actual <= limit,
            "Storyline {name} exceeded: actual {actual}, limit {limit}"
        );
    }
    Ok(())
}

fn exceeds_limit(actual: usize, limit: Option<usize>) -> bool {
    limit.is_some_and(|limit| actual > limit)
}

fn serialized_document_bytes(story: &StorylineDocument) -> Result<usize> {
    #[derive(Default)]
    struct CountingWriter(usize);

    impl std::io::Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(buffer.len())
                .ok_or_else(|| std::io::Error::other("Storyline serialized byte count overflow"))?;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, story).context("measure serialized Storyline document")?;
    Ok(writer.0)
}

struct EncodedBatchIterator<T> {
    rows: std::sync::Arc<[T]>,
    offset: usize,
    emitted_empty: bool,
    encode: fn(&[T]) -> Result<RecordBatch>,
}

impl<T> EncodedBatchIterator<T> {
    fn new(rows: Vec<T>, encode: fn(&[T]) -> Result<RecordBatch>) -> Self {
        Self {
            rows: rows.into(),
            offset: 0,
            emitted_empty: false,
            encode,
        }
    }
}

impl<T> Iterator for EncodedBatchIterator<T> {
    type Item = std::result::Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rows.is_empty() {
            if self.emitted_empty {
                return None;
            }
            self.emitted_empty = true;
            return Some(
                (self.encode)(&[]).map_err(|error| ArrowError::ComputeError(error.to_string())),
            );
        }
        if self.offset >= self.rows.len() {
            return None;
        }
        let end = (self.offset + WRITE_BATCH_ROWS).min(self.rows.len());
        let result = (self.encode)(&self.rows[self.offset..end])
            .map_err(|error| ArrowError::ComputeError(error.to_string()));
        self.offset = end;
        Some(result)
    }
}

fn encode_rows<T>(
    rows: Vec<T>,
    encode: fn(&[T]) -> Result<RecordBatch>,
) -> Result<Vec<RecordBatch>> {
    EncodedBatchIterator::new(rows, encode)
        .map(|batch| batch.map_err(anyhow::Error::from))
        .collect()
}

pub(super) struct ExternalizedStorylineBatches {
    pub(super) runs: Vec<RecordBatch>,
    pub(super) steps: Vec<RecordBatch>,
    pub(super) tool_calls: Vec<RecordBatch>,
    pub(super) pending: PendingContent,
}

pub(super) fn externalize_rows(
    runs: Vec<StoryRunRow>,
    steps: Vec<StoryStepRow>,
    tool_calls: Vec<StoryToolCallRow>,
    options: StorylineContentOptions,
) -> Result<ExternalizedStorylineBatches> {
    let mut pending = PendingContent::default();
    let runs = externalize_batches(
        encode_rows(runs, story_runs_to_batch)?,
        StorylineTableKind::Runs,
        options,
        &mut pending,
    )?;
    let steps = externalize_batches(
        encode_rows(steps, story_steps_to_batch)?,
        StorylineTableKind::Steps,
        options,
        &mut pending,
    )?;
    let tool_calls = externalize_batches(
        encode_rows(tool_calls, story_tool_calls_to_batch)?,
        StorylineTableKind::ToolCalls,
        options,
        &mut pending,
    )?;
    Ok(ExternalizedStorylineBatches {
        runs,
        steps,
        tool_calls,
        pending,
    })
}

fn batch_reader(
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
) -> RecordBatchIterator<impl Iterator<Item = std::result::Result<RecordBatch, ArrowError>>> {
    RecordBatchIterator::new(batches.into_iter().map(Ok), schema)
}

pub(super) async fn write_batches(
    path: &Path,
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
    indexes: &[(&str, IndexType)],
) -> Result<u64> {
    write_record_batch_reader(path, Box::new(batch_reader(batches, schema)), indexes).await
}

pub(super) async fn replace_table_batches(
    path: &Path,
    snapshot_version: u64,
    predicate: &str,
    merge_keys: &[&str],
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
) -> Result<u64> {
    let mut dataset = open_table_version(path, snapshot_version).await?;
    let latest_version = latest_table_version(path).await?;
    if latest_version != snapshot_version {
        dataset.restore().await.with_context(|| {
            format!(
                "restore committed Storyline table version {} for {}",
                snapshot_version,
                path.display()
            )
        })?;
    }
    let has_rows = batches.iter().any(|batch| batch.num_rows() > 0);
    if has_rows {
        let delete_missing =
            WhenNotMatchedBySource::delete_if(&dataset, predicate).with_context(|| {
                format!("build Storyline replacement filter for {}", path.display())
            })?;
        let mut merge = MergeInsertBuilder::try_new(
            Arc::new(dataset),
            merge_keys.iter().map(|key| (*key).to_string()).collect(),
        )?;
        merge
            .when_matched(WhenMatched::UpdateAll)
            .when_not_matched(WhenNotMatched::InsertAll)
            .when_not_matched_by_source(delete_missing);
        let (updated, _) = merge
            .try_build()?
            .execute_reader(batch_reader(batches, schema))
            .await
            .with_context(|| format!("merge replacement rows into {}", path.display()))?;
        return Ok(updated.version_id());
    } else {
        // An empty source still means "remove this session's former rows".
        dataset
            .delete(predicate)
            .await
            .with_context(|| format!("delete empty Storyline region from {}", path.display()))?;
    }
    Ok(dataset.version_id())
}

async fn write_record_batch_reader(
    path: &Path,
    reader: Box<dyn RecordBatchReader + Send>,
    indexes: &[(&str, IndexType)],
) -> Result<u64> {
    let uri = path.to_string_lossy().into_owned();
    let mut dataset = InsertBuilder::new(&uri)
        .with_params(&WriteParams {
            mode: WriteMode::Create,
            ..Default::default()
        })
        .execute_stream(reader)
        .await
        .with_context(|| format!("stream ATIF into Storyline table {}", path.display()))?;
    if dataset.count_rows(None).await? > 0 {
        for (column, index_type) in indexes {
            let builtin = match index_type {
                IndexType::Bitmap => BuiltinIndexType::Bitmap,
                _ => BuiltinIndexType::BTree,
            };
            let _admission = super::super::index_build_gate::acquire().await;
            dataset
                .create_index(
                    &[*column],
                    *index_type,
                    Some(format!("pchronicle_{column}_idx")),
                    &ScalarIndexParams::for_builtin(builtin),
                    false,
                )
                .await
                .with_context(|| {
                    format!(
                        "create {:?} index on {}.{}",
                        index_type,
                        path.display(),
                        column
                    )
                })?;
        }
    }
    Ok(dataset.version_id())
}
