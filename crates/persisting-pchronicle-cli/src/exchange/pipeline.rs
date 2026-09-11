//! Generic multi-stage producer/consumer pipeline with bounded buffers.
//!
//! Stages communicate through `tokio::sync::mpsc` channels: a full buffer
//! applies backpressure to the upstream producer. Each stage reports through a
//! [`StageHandle`](super::progress::StageHandle).
//!
//! Import shape:
//! `discover (1) → fetch (N) →[8]→ parse (N) →[8]→ commit (1 task)`

use super::decode::{DecodeImportOutcome, DecodedImportSource, ImportedSource};
use super::progress::StageHandle;
use anyhow::{Result, anyhow};
use persisting_pchronicle::model::StorylineDocument;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Discover → fetch buffer (listing can run ahead of I/O).
pub(crate) const DISCOVER_TO_FETCH_BUFFER: usize = 64;
/// Fetch → parse buffer. Keep modest: each slot holds a full source payload.
pub(crate) const FETCH_TO_PARSE_BUFFER: usize = 8;
/// Parse → commit buffer. Small on purpose — decoded Storylines are heavy, and
/// remote commit (often S3 with concurrency 1) is the usual bottleneck; a large
/// backlog only burns RAM. Keep enough headroom that parse workers do not thrash
/// on every commit AIMD pause / full-batch flush.
pub(crate) const PARSE_TO_COMMIT_BUFFER: usize = 8;
/// Parallel fetch workers.
pub(crate) const FETCH_STAGE_CONCURRENCY: usize = 4;
/// Parallel parse workers.
pub(crate) const PARSE_STAGE_CONCURRENCY: usize = 4;

/// One item flowing out of the discover stage.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredItem {
    pub(crate) path: String,
    pub(crate) bytes: u64,
}

/// Bytes loaded for one discovered source.
#[derive(Debug)]
pub(crate) struct FetchedItem {
    pub(crate) path: String,
    pub(crate) relative_path: PathBuf,
    pub(crate) output_relative_path: Option<PathBuf>,
    pub(crate) bytes: Vec<u8>,
}

/// Decode result ready for the single-worker commit stage.
#[derive(Debug)]
pub(crate) enum ParsedItem {
    Imported {
        diagnostic_path: PathBuf,
        metadata: ImportedSource,
        storylines: Vec<StorylineDocument>,
        warnings: persisting_pchronicle::model::UnknownFieldImportWarnings,
    },
    Skipped {
        path: PathBuf,
        reason: String,
        bytes: u64,
    },
}

/// A bounded link between two stages (backpressure when full).
pub(crate) struct StageChannel<T> {
    pub(crate) tx: mpsc::Sender<Result<T>>,
    pub(crate) rx: mpsc::Receiver<Result<T>>,
}

impl<T> StageChannel<T> {
    pub(crate) fn bounded(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        Self { tx, rx }
    }
}

pub(crate) struct ParallelMapOptions {
    pub(crate) capacity: usize,
    pub(crate) workers: usize,
    pub(crate) outbound: StageHandle,
    pub(crate) downstream: &'static str,
    pub(crate) track_outbound_queue: bool,
}

/// Send into a bounded stage channel, surfacing backpressure on the progress line.
///
/// When `track_inbound_queue` is true, `inbound`'s `queue_depth` is incremented on a
/// successful enqueue so the UI shows the real channel length. Commit uses batch
/// fill instead, so parse→commit passes `false`.
pub(crate) async fn send_with_flow_control<T: Send>(
    tx: &mpsc::Sender<Result<T>>,
    item: Result<T>,
    sender: &StageHandle,
    inbound: &StageHandle,
    downstream: &'static str,
    track_inbound_queue: bool,
) -> bool {
    match tx.try_reserve() {
        Ok(permit) => {
            permit.send(item);
            if track_inbound_queue {
                inbound.queue_push();
            }
            true
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            sender.enter_flow_wait(format!("pending→{downstream}"));
            let ok = tx.send(item).await.is_ok();
            sender.leave_flow_wait();
            if ok && track_inbound_queue {
                inbound.queue_push();
            }
            ok
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// Spawn a source stage that only produces items (no upstream).
///
/// `downstream` labels the next stage for backpressure UI (e.g. `"fetch"`).
pub(crate) fn spawn_source_stage<T, F, Fut>(
    capacity: usize,
    progress: StageHandle,
    body: F,
) -> (mpsc::Receiver<Result<T>>, JoinHandle<()>)
where
    T: Send + 'static,
    F: FnOnce(mpsc::Sender<Result<T>>, StageHandle) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let StageChannel { tx, rx } = StageChannel::bounded(capacity);
    let handle = progress.clone();
    let join = tokio::spawn(async move {
        body(tx, handle).await;
    });
    (rx, join)
}

/// Spawn a 1:1 map stage: recv `In` → process → send `Out`.
#[cfg(test)]
pub(crate) fn spawn_map_stage<In, Out, F, Fut>(
    mut rx: mpsc::Receiver<Result<In>>,
    capacity: usize,
    progress: StageHandle,
    outbound: StageHandle,
    downstream: &'static str,
    mut map: F,
) -> (mpsc::Receiver<Result<Out>>, JoinHandle<()>)
where
    In: Send + 'static,
    Out: Send + 'static,
    F: FnMut(In, StageHandle) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Out>> + Send + 'static,
{
    let StageChannel { tx, rx: out_rx } = StageChannel::bounded(capacity);
    let join = tokio::spawn(async move {
        loop {
            progress.enter_upstream_wait();
            let item = rx.recv().await;
            progress.leave_upstream_wait();
            let Some(item) = item else {
                break;
            };
            progress.queue_pop();
            match item {
                Ok(input) => match map(input, progress.clone()).await {
                    Ok(output) => {
                        if !send_with_flow_control(
                            &tx,
                            Ok(output),
                            &progress,
                            &outbound,
                            downstream,
                            true,
                        )
                        .await
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        progress.record_error(format!("{error:#}"));
                        let _ = send_with_flow_control(
                            &tx,
                            Err(error),
                            &progress,
                            &outbound,
                            downstream,
                            true,
                        )
                        .await;
                        return;
                    }
                },
                Err(error) => {
                    progress.record_error(format!("{error:#}"));
                    let _ = send_with_flow_control(
                        &tx,
                        Err(error),
                        &progress,
                        &outbound,
                        downstream,
                        true,
                    )
                    .await;
                    return;
                }
            }
        }
    });
    (out_rx, join)
}

/// Spawn a bounded parallel map stage (multi-worker).
pub(crate) fn spawn_parallel_map_stage<In, Out, F, Fut>(
    mut rx: mpsc::Receiver<Result<In>>,
    progress: StageHandle,
    options: ParallelMapOptions,
    map: F,
) -> (mpsc::Receiver<Result<Out>>, JoinHandle<()>)
where
    In: Send + 'static,
    Out: Send + 'static,
    F: Fn(In, StageHandle) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Out>> + Send + 'static,
{
    let StageChannel { tx, rx: out_rx } = StageChannel::bounded(options.capacity);
    let workers = options.workers.max(1);
    let outbound = options.outbound;
    let downstream = options.downstream;
    let track_outbound_queue = options.track_outbound_queue;
    let join = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut pending = BTreeMap::new();
        let mut next_input = 0usize;
        let mut next_output = 0usize;
        let mut input_closed = false;
        loop {
            while !input_closed && tasks.len() < workers {
                progress.enter_upstream_wait();
                let received = rx.recv().await;
                progress.leave_upstream_wait();
                match received {
                    Some(item) => {
                        progress.queue_pop();
                        let sequence = next_input;
                        next_input += 1;
                        let progress = progress.clone();
                        let map = map.clone();
                        if item.is_err() {
                            input_closed = true;
                        }
                        tasks.spawn(async move {
                            let result = match item {
                                Ok(input) => map(input, progress.clone()).await,
                                Err(error) => Err(error),
                            };
                            if let Err(error) = &result {
                                progress.record_error(format!("{error:#}"));
                            }
                            (sequence, result)
                        });
                    }
                    None => input_closed = true,
                }
            }
            if tasks.is_empty() {
                break;
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let (sequence, result) = match joined {
                Ok(result) => result,
                Err(error) => {
                    progress.record_error(format!("parallel map worker failed: {error}"));
                    return;
                }
            };
            pending.insert(sequence, result);
            while let Some(result) = pending.remove(&next_output) {
                if !send_with_flow_control(
                    &tx,
                    result,
                    &progress,
                    &outbound,
                    downstream,
                    track_outbound_queue,
                )
                .await
                {
                    return;
                }
                next_output += 1;
            }
        }
    });
    (out_rx, join)
}

fn spawn_parse_stage(
    fetched_rx: mpsc::Receiver<Result<FetchedItem>>,
    requested_format: crate::ExchangeFormat,
    suggested_format: Option<crate::ExchangeFormat>,
    parse: StageHandle,
    commit: StageHandle,
    _unknown_field_warnings: Arc<
        tokio::sync::Mutex<persisting_pchronicle::model::UnknownFieldImportWarnings>,
    >,
    soft_skip_parse_errors: bool,
) -> (mpsc::Receiver<Result<ParsedItem>>, JoinHandle<()>) {
    spawn_parallel_map_stage(
        fetched_rx,
        parse,
        ParallelMapOptions {
            capacity: PARSE_TO_COMMIT_BUFFER,
            workers: PARSE_STAGE_CONCURRENCY,
            outbound: commit,
            downstream: "commit",
            track_outbound_queue: false,
        },
        move |fetched, parse| {
            async move {
                let name = fetched.path.clone();
                parse.set_current(name.clone());
                let mut warnings =
                    persisting_pchronicle::model::UnknownFieldImportWarnings::default();
                let parse_result = super::decode::decode_import_source(
                    requested_format,
                    suggested_format,
                    crate::ImportOutputFormat::Storyline,
                    Some(std::path::Path::new(&fetched.path)),
                    Some(&fetched.relative_path),
                    fetched.output_relative_path.as_deref(),
                    &fetched.bytes,
                    &mut warnings,
                );
                match parse_result {
                    Ok(DecodeImportOutcome::Imported(DecodedImportSource {
                        diagnostic_path,
                        metadata,
                        storylines,
                    })) => {
                        let bytes = metadata.input_bytes as u64;
                        if storylines.is_empty() {
                            parse.record_empty(1, bytes);
                        } else {
                            parse.record(1, bytes);
                        }
                        Ok(ParsedItem::Imported {
                            diagnostic_path,
                            metadata,
                            storylines,
                            warnings,
                        })
                    }
                    Ok(DecodeImportOutcome::Skipped { path, reason }) => {
                        parse.record_skipped(1, fetched.bytes.len() as u64);
                        Ok(ParsedItem::Skipped { path, reason })
                    }
                    Err(error) if soft_skip_parse_errors => {
                        // Directory imports skip unreadable files so the rest of
                        // the tree can continue; commit worker logs the reason.
                        parse.record_error(format!("{error:#}"));
                        Ok(ParsedItem::Skipped {
                            path: PathBuf::from(&name),
                            reason: format!("{error:#}"),
                        })
                    }
                    Err(error) => Err(error),
                }
            }
        },
    )
}

/// Wire helpers for import: discover → fetch → parse (commit is a separate task).
pub(crate) struct ImportPipelineHandles {
    pub(crate) parsed_rx: mpsc::Receiver<Result<ParsedItem>>,
    pub(crate) joins: Vec<JoinHandle<()>>,
    pub(crate) unknown_field_warnings:
        Arc<tokio::sync::Mutex<persisting_pchronicle::model::UnknownFieldImportWarnings>>,
}

pub(crate) struct ImportPipelineConfig {
    pub(crate) max_input_bytes: usize,
    pub(crate) requested_format: crate::ExchangeFormat,
    pub(crate) suggested_format: Option<crate::ExchangeFormat>,
    pub(crate) discover: StageHandle,
    pub(crate) fetch: StageHandle,
    pub(crate) parse: StageHandle,
    pub(crate) commit: StageHandle,
    /// Relative source paths already completed or failed in a prior run.
    pub(crate) skip_paths: Arc<HashSet<String>>,
    /// Directory imports skip unreadable files; an explicit file must fail.
    pub(crate) soft_skip_parse_errors: bool,
}

/// Build discover→fetch→parse for a prelisted candidate set.
pub(crate) fn spawn_candidates_fetch_pipeline(
    candidates: Vec<super::decode::ImportFileCandidate>,
    config: ImportPipelineConfig,
) -> ImportPipelineHandles {
    let ImportPipelineConfig {
        max_input_bytes,
        requested_format,
        suggested_format,
        discover,
        fetch,
        parse,
        commit,
        skip_paths,
        soft_skip_parse_errors,
    } = config;
    let unknown_field_warnings = Arc::new(tokio::sync::Mutex::new(
        persisting_pchronicle::model::UnknownFieldImportWarnings::default(),
    ));
    let total = candidates.len() as u64;
    discover.set_total_items(total);
    fetch.set_queue_cap(DISCOVER_TO_FETCH_BUFFER as u64);
    parse.set_queue_cap(FETCH_TO_PARSE_BUFFER as u64);
    // Commit queue shows batch fill, configured in the commit task.
    let fetch_for_discover = fetch.clone();
    let (discovered_rx, discover_join) = spawn_source_stage(
        DISCOVER_TO_FETCH_BUFFER,
        discover.clone(),
        move |tx, discover| async move {
            for candidate in candidates {
                let path = candidate.relative_path.to_string_lossy().into_owned();
                if skip_paths.contains(&path) {
                    discover.record_skipped(1, candidate.size_hint);
                    continue;
                }
                let bytes = candidate.size_hint;
                // Discover totals were already set via set_discovered; only
                // refresh the activity label while feeding the fetch stage.
                discover.set_current(path.clone());
                let item = DiscoveredItem { path, bytes };
                if !send_with_flow_control(
                    &tx,
                    Ok((item, candidate)),
                    &discover,
                    &fetch_for_discover,
                    "reading",
                    true,
                )
                .await
                {
                    return;
                }
            }
            discover.clear_current();
        },
    );

    let parse_for_fetch = parse.clone();
    let (fetched_rx, fetch_join) = spawn_parallel_map_stage(
        discovered_rx,
        fetch,
        ParallelMapOptions {
            capacity: FETCH_TO_PARSE_BUFFER,
            workers: FETCH_STAGE_CONCURRENCY,
            outbound: parse_for_fetch,
            downstream: "parsing",
            track_outbound_queue: true,
        },
        move |(item, candidate), fetch| async move {
            fetch.set_current(item.path.clone());
            let label = format!("import source {}", item.path);
            let bytes =
                super::decode::load_import_candidate_bytes(&candidate, max_input_bytes, &label)
                    .await?;
            let fetched = FetchedItem {
                path: item.path,
                relative_path: candidate.relative_path,
                output_relative_path: candidate.output_relative_path,
                bytes,
            };
            fetch.record(1, fetched.bytes.len() as u64);
            Ok(fetched)
        },
    );

    let (parsed_rx, parse_join) = spawn_parse_stage(
        fetched_rx,
        requested_format,
        suggested_format,
        parse,
        commit,
        Arc::clone(&unknown_field_warnings),
        soft_skip_parse_errors,
    );

    ImportPipelineHandles {
        parsed_rx,
        joins: vec![discover_join, fetch_join, parse_join],
        unknown_field_warnings,
    }
}

/// Build discover→fetch→parse for an object-store (or local tree) location.
pub(crate) fn spawn_location_fetch_pipeline(
    location: persisting_pchronicle::storage::DatasetLocation,
    config: ImportPipelineConfig,
) -> ImportPipelineHandles {
    let ImportPipelineConfig {
        max_input_bytes,
        requested_format,
        suggested_format,
        discover,
        fetch,
        parse,
        commit,
        skip_paths,
        soft_skip_parse_errors,
    } = config;
    let unknown_field_warnings = Arc::new(tokio::sync::Mutex::new(
        persisting_pchronicle::model::UnknownFieldImportWarnings::default(),
    ));
    fetch.set_queue_cap(DISCOVER_TO_FETCH_BUFFER as u64);
    parse.set_queue_cap(FETCH_TO_PARSE_BUFFER as u64);
    // Commit queue shows batch fill, configured in the commit task.
    let remote_root = location.as_str().to_owned();
    let fetch_for_discover = fetch.clone();
    let (discovered_rx, discover_join) = spawn_source_stage(
        DISCOVER_TO_FETCH_BUFFER,
        discover.clone(),
        move |tx, discover| async move {
            let list_result = location
                .for_each_importable_json_object_event(
                    persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES,
                    |event| {
                        let tx = tx.clone();
                        let discover = discover.clone();
                        let fetch = fetch_for_discover.clone();
                        let skip_paths = Arc::clone(&skip_paths);
                        async move {
                            match event {
                                persisting_pchronicle::storage::ImportableObjectEvent::Scanning {
                                    prefix,
                                } => {
                                    let label = if prefix.is_empty() {
                                        "/".to_owned()
                                    } else {
                                        format!("{prefix}/")
                                    };
                                    discover.set_current(label);
                                    Ok(())
                                }
                                persisting_pchronicle::storage::ImportableObjectEvent::File {
                                    key,
                                    size,
                                    ..
                                } => {
                                    if skip_paths.contains(&key) {
                                        discover.record_skipped(1, size);
                                        return Ok(());
                                    }
                                    discover.set_current(key.clone());
                                    discover.record(1, size);
                                    if !send_with_flow_control(
                                        &tx,
                                        Ok(DiscoveredItem {
                                            path: key,
                                            bytes: size,
                                        }),
                                        &discover,
                                        &fetch,
                                        "reading",
                                        true,
                                    )
                                    .await
                                    {
                                        return Ok(());
                                    }
                                    Ok(())
                                }
                            }
                        }
                    },
                )
                .await;
            if let Err(error) = list_result {
                discover.record_error(format!("{error:#}"));
                if tx.send(Err(error)).await.is_ok() {
                    fetch_for_discover.queue_push();
                }
                return;
            }
            discover.clear_current();
        },
    );

    let remote_root_for_fetch = remote_root;
    let parse_for_fetch = parse.clone();
    let (fetched_rx, fetch_join) = spawn_parallel_map_stage(
        discovered_rx,
        fetch,
        ParallelMapOptions {
            capacity: FETCH_TO_PARSE_BUFFER,
            workers: FETCH_STAGE_CONCURRENCY,
            outbound: parse_for_fetch,
            downstream: "parsing",
            track_outbound_queue: true,
        },
        move |item, fetch| {
            let remote_root = remote_root_for_fetch.clone();
            async move {
                fetch.set_current(item.path.clone());
                let relative_path = PathBuf::from(&item.path);
                let candidate = super::decode::ImportFileCandidate {
                    path: relative_path.clone(),
                    output_relative_path: Some(relative_path.clone()),
                    relative_path: relative_path.clone(),
                    content: None,
                    remote_root: Some(remote_root),
                };
                let label = format!("import source {}", item.path);
                let bytes =
                    super::decode::load_import_candidate_bytes(&candidate, max_input_bytes, &label)
                        .await?;
                fetch.record(1, bytes.len() as u64);
                Ok(FetchedItem {
                    path: item.path,
                    relative_path,
                    output_relative_path: candidate.output_relative_path,
                    bytes,
                })
            }
        },
    );

    let (parsed_rx, parse_join) = spawn_parse_stage(
        fetched_rx,
        requested_format,
        suggested_format,
        parse,
        commit,
        Arc::clone(&unknown_field_warnings),
        soft_skip_parse_errors,
    );

    ImportPipelineHandles {
        parsed_rx,
        joins: vec![discover_join, fetch_join, parse_join],
        unknown_field_warnings,
    }
}

pub(crate) async fn join_pipeline_stages(joins: Vec<JoinHandle<()>>) -> Result<()> {
    for join in joins {
        match join.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => return Err(anyhow!("pipeline stage task failed: {error}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::progress::{CliProgress, StageId};

    #[tokio::test]
    async fn map_stage_applies_backpressure_and_transforms() {
        let progress = CliProgress::new(false);
        let (rx, join) =
            spawn_source_stage(1, progress.stage(StageId::Discover), |tx, _| async move {
                for i in 0..5u64 {
                    tx.send(Ok(i)).await.unwrap();
                }
            });
        let (mut out_rx, map_join) = spawn_map_stage(
            rx,
            1,
            progress.stage(StageId::Fetch),
            progress.stage(StageId::Parse),
            "parsing",
            |n, _| async move { Ok(n * 10) },
        );
        let mut got = Vec::new();
        while let Some(item) = out_rx.recv().await {
            got.push(item.unwrap());
        }
        join_pipeline_stages(vec![join, map_join]).await.unwrap();
        assert_eq!(got, vec![0, 10, 20, 30, 40]);
    }

    #[tokio::test]
    async fn parallel_map_stage_uses_multiple_workers() {
        let progress = CliProgress::new(false);
        let (rx, join) =
            spawn_source_stage(8, progress.stage(StageId::Discover), |tx, _| async move {
                for i in 0..8u64 {
                    tx.send(Ok(i)).await.unwrap();
                }
            });
        let (mut out_rx, map_join) = spawn_parallel_map_stage(
            rx,
            progress.stage(StageId::Fetch),
            ParallelMapOptions {
                capacity: FETCH_TO_PARSE_BUFFER,
                workers: 4,
                outbound: progress.stage(StageId::Parse),
                downstream: "parsing",
                track_outbound_queue: true,
            },
            |n, _| async move {
                tokio::time::sleep(std::time::Duration::from_millis(40 - n * 5)).await;
                Ok(n)
            },
        );
        let mut got = Vec::new();
        while let Some(item) = out_rx.recv().await {
            got.push(item.unwrap());
        }
        join_pipeline_stages(vec![join, map_join]).await.unwrap();
        assert_eq!(got, (0..8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn map_stage_records_error_on_failure() {
        let progress = CliProgress::new(false);
        let fetch = progress.stage(StageId::Fetch);
        let (rx, join) =
            spawn_source_stage(2, progress.stage(StageId::Discover), |tx, _| async move {
                let _ = tx.send(Ok(1u64)).await;
            });
        let (mut out_rx, map_join) = spawn_map_stage(
            rx,
            2,
            fetch.clone(),
            progress.stage(StageId::Parse),
            "parsing",
            |_n, _| async move { Err::<u64, _>(anyhow!("boom")) },
        );
        let err = out_rx.recv().await.unwrap().unwrap_err();
        assert!(format!("{err:#}").contains("boom"));
        join_pipeline_stages(vec![join, map_join]).await.unwrap();
    }

    #[test]
    fn buffer_constants_match_import_shape() {
        assert_eq!(FETCH_TO_PARSE_BUFFER, 8);
        assert_eq!(PARSE_TO_COMMIT_BUFFER, 8);
        assert_eq!(FETCH_STAGE_CONCURRENCY, 4);
        assert_eq!(PARSE_STAGE_CONCURRENCY, 4);
        assert_eq!(StageId::Discover.noun(), "listing");
        assert_eq!(StageId::Fetch.verb(), "reading");
        assert_eq!(StageId::Parse.verb(), "parsing");
        assert_eq!(StageId::Commit.noun(), "commit");
    }
}
