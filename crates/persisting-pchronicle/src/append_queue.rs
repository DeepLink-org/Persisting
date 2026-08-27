//! Bounded bridge from synchronous capture callbacks to the async Lance appender.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::Context;

use crate::formats::EventRecord;
use crate::layout::StoryCoords;
use crate::store::compact_sealed_event_segment;
use crate::store::{ObjectStoreManifestWriteMode, RawEventLanceAppender, raw_event_lance_path};

pub const DEFAULT_RAW_EVENT_QUEUE_CAPACITY: usize = 256;
pub const DEFAULT_RAW_EVENT_BATCH_SIZE: usize = 256;
pub const DEFAULT_RAW_EVENT_BATCH_DELAY: Duration = Duration::from_millis(2);
/// Bound tiny fragments in the live snapshot before fan-out reads exhaust
/// file descriptors. Compaction runs after durable waiters have been released.
pub const DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD: usize = 8;
pub const DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT: usize =
    DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD;
pub const DEFAULT_RAW_EVENT_HIERARCHY_FANOUT: usize = 8;
/// Maintenance is best-effort and recoverable through explicit `maintain`.
/// Bound its live backlog independently from durable capture admission.
pub const DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY: usize = 64;

#[derive(Debug)]
struct RawEventAppendJob {
    coords: StoryCoords,
    record: EventRecord,
    completion: Option<mpsc::SyncSender<anyhow::Result<()>>>,
}

#[derive(Debug)]
enum WriterMessage {
    Append(Box<RawEventAppendJob>),
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEventAppendOutcome {
    Accepted,
    Full,
    Unavailable,
}

#[derive(Debug)]
struct SenderState {
    tx: mpsc::SyncSender<WriterMessage>,
    accepting: AtomicBool,
    in_flight: AtomicUsize,
}

/// Cloneable, bounded sender suitable for synchronous capture callbacks.
///
/// `try_append` never waits for queue capacity. A full queue is reported to
/// the caller so capture can apply an explicit failure policy instead of
/// blocking an Agent or Gateway thread.
#[derive(Clone, Debug)]
pub struct RawEventAppendSender {
    state: Arc<SenderState>,
}

impl RawEventAppendSender {
    pub fn try_append(&self, coords: StoryCoords, record: EventRecord) -> RawEventAppendOutcome {
        self.enqueue(coords, record, None)
    }

    /// Append an event and wait until its Lance micro-batch is durably visible.
    ///
    /// Queue admission remains bounded and non-blocking. Once admitted, this
    /// method waits for the writer so an upstream WAL can acknowledge the event
    /// only after pChronicle has published the corresponding Lance segment.
    pub fn append_durable(
        &self,
        coords: StoryCoords,
        record: EventRecord,
    ) -> anyhow::Result<RawEventAppendOutcome> {
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        match self.enqueue(coords, record, Some(completion_tx)) {
            RawEventAppendOutcome::Accepted => completion_rx
                .recv()
                .context("await raw event append completion")?
                .map(|()| RawEventAppendOutcome::Accepted),
            rejection => Ok(rejection),
        }
    }

    /// Enqueue a batch before waiting for durability. This preserves the
    /// worker's micro-batching behavior for high-volume OTLP requests; calling
    /// `append_durable` once per record would serialize every Lance commit.
    ///
    /// Admission remains bounded and non-blocking. If the queue fills midway,
    /// already-admitted records are drained to completion before `Full` is
    /// returned, so callers never acknowledge an unfinished prefix.
    pub fn append_durable_batch(
        &self,
        entries: Vec<(StoryCoords, EventRecord)>,
    ) -> anyhow::Result<RawEventAppendOutcome> {
        let mut completions = Vec::with_capacity(entries.len());
        for (coords, record) in entries {
            let (completion_tx, completion_rx) = mpsc::sync_channel(1);
            match self.enqueue(coords, record, Some(completion_tx)) {
                RawEventAppendOutcome::Accepted => completions.push(completion_rx),
                rejection => {
                    for completion in completions {
                        let _ = completion.recv();
                    }
                    return Ok(rejection);
                }
            }
        }
        for completion in completions {
            completion
                .recv()
                .context("await raw event batch append completion")??;
        }
        Ok(RawEventAppendOutcome::Accepted)
    }

    fn enqueue(
        &self,
        coords: StoryCoords,
        record: EventRecord,
        completion: Option<mpsc::SyncSender<anyhow::Result<()>>>,
    ) -> RawEventAppendOutcome {
        if !self.state.accepting.load(Ordering::SeqCst) {
            return RawEventAppendOutcome::Unavailable;
        }

        self.state.in_flight.fetch_add(1, Ordering::SeqCst);
        if !self.state.accepting.load(Ordering::SeqCst) {
            self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
            return RawEventAppendOutcome::Unavailable;
        }

        let outcome =
            match self
                .state
                .tx
                .try_send(WriterMessage::Append(Box::new(RawEventAppendJob {
                    coords,
                    record,
                    completion,
                }))) {
                Ok(()) => RawEventAppendOutcome::Accepted,
                Err(mpsc::TrySendError::Full(_)) => RawEventAppendOutcome::Full,
                Err(mpsc::TrySendError::Disconnected(_)) => RawEventAppendOutcome::Unavailable,
            };
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        outcome
    }
}

/// Owner of the append worker. Call `finish` after all capture sinks have
/// stopped using their cloned senders.
pub struct RawEventAppendWorker {
    state: Arc<SenderState>,
    join: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl RawEventAppendWorker {
    pub fn finish(mut self) -> anyhow::Result<()> {
        self.state.accepting.store(false, Ordering::SeqCst);
        while self.state.in_flight.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }

        let finish_signal = self
            .state
            .tx
            .send(WriterMessage::Finish)
            .map_err(anyhow::Error::new)
            .context("finish raw event append worker");
        let Some(join) = self.join.take() else {
            return finish_signal;
        };
        let worker_result = join
            .join()
            .map_err(|_| anyhow::anyhow!("pChronicle append worker thread panicked"))?;
        match (finish_signal, worker_result) {
            (_, Err(worker_error)) => Err(worker_error),
            (Err(signal_error), Ok(())) => Err(signal_error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

pub fn raw_event_append_queue() -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)> {
    raw_event_append_queue_with_capacity(DEFAULT_RAW_EVENT_QUEUE_CAPACITY)
}

pub fn raw_event_append_queue_with_manifest_write_mode(
    manifest_write_mode: ObjectStoreManifestWriteMode,
) -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)> {
    raw_event_append_queue_with_options(
        DEFAULT_RAW_EVENT_QUEUE_CAPACITY,
        DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
        DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
        DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
        manifest_write_mode,
    )
}

pub fn raw_event_append_queue_with_capacity(
    capacity: usize,
) -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)> {
    raw_event_append_queue_with_options(
        capacity,
        DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
        DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
        DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
        ObjectStoreManifestWriteMode::Conditional,
    )
}

fn raw_event_append_queue_with_options(
    capacity: usize,
    compaction_threshold: usize,
    target_rows_per_fragment: usize,
    hierarchy_fanout: usize,
    manifest_write_mode: ObjectStoreManifestWriteMode,
) -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)> {
    if capacity == 0 {
        anyhow::bail!("pChronicle append queue capacity must be greater than zero");
    }
    anyhow::ensure!(
        compaction_threshold > 1,
        "pChronicle compaction threshold must be greater than one"
    );
    anyhow::ensure!(
        target_rows_per_fragment > 0,
        "pChronicle compaction target rows must be greater than zero"
    );
    anyhow::ensure!(
        hierarchy_fanout > 1,
        "pChronicle compaction hierarchy fanout must be greater than one"
    );

    let (tx, rx) = mpsc::sync_channel::<WriterMessage>(capacity);
    let state = Arc::new(SenderState {
        tx,
        accepting: AtomicBool::new(true),
        in_flight: AtomicUsize::new(0),
    });
    let join = std::thread::Builder::new()
        .name("pchronicle-append".to_string())
        .spawn({
            let worker_state = Arc::clone(&state);
            move || {
                run_append_worker(
                    rx,
                    worker_state,
                    compaction_threshold,
                    target_rows_per_fragment,
                    hierarchy_fanout,
                    manifest_write_mode,
                )
            }
        })
        .context("spawn pChronicle append worker")?;

    Ok((
        RawEventAppendSender {
            state: Arc::clone(&state),
        },
        RawEventAppendWorker {
            state,
            join: Some(join),
        },
    ))
}

fn run_append_worker(
    rx: mpsc::Receiver<WriterMessage>,
    state: Arc<SenderState>,
    compaction_threshold: usize,
    target_rows_per_fragment: usize,
    hierarchy_fanout: usize,
    manifest_write_mode: ObjectStoreManifestWriteMode,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("create pChronicle append worker runtime")?;
    let mut appender =
        RawEventLanceAppender::default().with_object_store_manifest_write_mode(manifest_write_mode);
    let (maintenance_tx, mut maintenance_rx) =
        tokio::sync::mpsc::channel(DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY);
    let maintenance_task = runtime.spawn(async move {
        while let Some(segment) = maintenance_rx.recv().await {
            if let Err(error) =
                compact_sealed_event_segment(segment, target_rows_per_fragment, hierarchy_fanout)
                    .await
            {
                tracing::warn!(
                    target: "persisting_pchronicle",
                    "background raw-event compaction failed: {error:#}"
                );
            }
        }
    });
    let mut finishing = false;

    while !finishing {
        let first = match rx.recv() {
            Ok(WriterMessage::Append(job)) => *job,
            Ok(WriterMessage::Finish) | Err(_) => break,
        };
        let mut jobs = Vec::with_capacity(DEFAULT_RAW_EVENT_BATCH_SIZE);
        jobs.push(first);
        while jobs.len() < DEFAULT_RAW_EVENT_BATCH_SIZE {
            match rx.recv_timeout(DEFAULT_RAW_EVENT_BATCH_DELAY) {
                Ok(WriterMessage::Append(job)) => jobs.push(*job),
                Ok(WriterMessage::Finish) => {
                    finishing = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    finishing = true;
                    break;
                }
            }
        }

        let mut completions = BTreeMap::<String, Vec<mpsc::SyncSender<anyhow::Result<()>>>>::new();
        let mut entries = Vec::with_capacity(jobs.len());
        for job in jobs {
            match raw_event_lance_path(&job.coords) {
                Ok(path) => {
                    let uri = path.to_string_lossy().into_owned();
                    if let Some(completion) = job.completion {
                        completions.entry(uri).or_default().push(completion);
                    } else {
                        completions.entry(uri).or_default();
                    }
                    entries.push((job.coords, job.record));
                }
                Err(error) => {
                    if let Some(error) = notify_completion(job.completion, Err(error)) {
                        tracing::warn!(
                            target: "persisting_pchronicle",
                            "non-durable raw-event append path failed: {error:#}"
                        );
                    }
                }
            }
        }
        if entries.is_empty() {
            continue;
        }
        let append_result = runtime
            .block_on(appender.append_event_batch_partitioned(&entries))
            .context("append event batch to pChronicle");
        match append_result {
            Ok(mut report) => {
                for (uri, partition_completions) in completions {
                    let result = match report.take_outcome(&uri) {
                        Some(Ok(_)) => Ok(()),
                        Some(Err(error)) => Err(error.context("append raw event partition")),
                        None => Err(anyhow::anyhow!(
                            "pChronicle append returned no partition outcome"
                        )
                        .context(format!("append raw event partition {uri}"))),
                    };
                    if let Some(error) =
                        notify_partition_completions(&uri, partition_completions, result)
                    {
                        tracing::warn!(
                            target: "persisting_pchronicle",
                            "non-durable raw-event append failed for {uri}: {error:#}"
                        );
                    }
                }
            }
            Err(error) => {
                return Err(complete_terminal_append_failure(
                    &rx,
                    &state,
                    completions,
                    error,
                ));
            }
        }

        match appender.seal_fragmented_segments(compaction_threshold) {
            Ok(segments) => {
                for segment in segments {
                    enqueue_maintenance(&maintenance_tx, segment);
                }
            }
            Err(error) => {
                // The append and its manifest publication are already durable.
                // Keep maintenance best-effort so a compaction failure cannot
                // turn a successful capture into a retry.
                tracing::warn!(
                    target: "persisting_pchronicle",
                    "background raw-event segment sealing failed: {error:#}"
                );
            }
        }
    }

    // Seal the final partial segments after producers have stopped. This keeps
    // post-shutdown readers from inheriting a tail of tiny live fragments.
    if let Ok(segments) = appender.seal_fragmented_segments(2) {
        for segment in segments {
            enqueue_maintenance(&maintenance_tx, segment);
        }
    }
    drop(maintenance_tx);
    if let Err(error) = runtime.block_on(maintenance_task) {
        tracing::warn!(
            target: "persisting_pchronicle",
            "raw-event maintenance worker failed: {error}"
        );
    }

    // Full compaction across sealed segments, indexing, and vacuum remain
    // explicit pChronicle maintenance operations.
    let _reports = appender.finish();
    Ok(())
}

fn notify_completion(
    completion: Option<mpsc::SyncSender<anyhow::Result<()>>>,
    result: anyhow::Result<()>,
) -> Option<anyhow::Error> {
    match (completion, result) {
        (Some(completion), Ok(())) => {
            let _ = completion.send(Ok(()));
            None
        }
        (Some(completion), Err(error)) => send_owned_completion_error(&completion, error),
        (None, Ok(())) => None,
        (None, Err(error)) => Some(error),
    }
}

fn notify_partition_completions(
    uri: &str,
    completions: Vec<mpsc::SyncSender<anyhow::Result<()>>>,
    result: anyhow::Result<()>,
) -> Option<anyhow::Error> {
    match result {
        Ok(()) => {
            for completion in completions {
                let _ = completion.send(Ok(()));
            }
            None
        }
        Err(error) => {
            let mut original = Some(error);
            for completion in completions {
                if let Some(error) = original.take() {
                    original = send_owned_completion_error(&completion, error);
                } else {
                    let _ = completion.send(Err(partition_append_failure(uri)));
                }
            }
            original
        }
    }
}

fn send_owned_completion_error(
    completion: &mpsc::SyncSender<anyhow::Result<()>>,
    error: anyhow::Error,
) -> Option<anyhow::Error> {
    match completion.send(Err(error)) {
        Ok(()) => None,
        Err(send_error) => send_error.0.err(),
    }
}

fn partition_append_failure(uri: &str) -> anyhow::Error {
    anyhow::anyhow!("partition micro-batch failed; another waiter owns the storage failure")
        .context(format!("append raw event partition {uri}"))
}

fn enqueue_maintenance(
    sender: &tokio::sync::mpsc::Sender<crate::store::SealedEventSegment>,
    segment: crate::store::SealedEventSegment,
) {
    match sender.try_send(segment) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => tracing::warn!(
            target: "persisting_pchronicle",
            "raw-event maintenance backlog is full; explicit maintenance will recover layout"
        ),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => tracing::warn!(
            target: "persisting_pchronicle",
            "raw-event maintenance worker stopped unexpectedly"
        ),
    }
}

fn complete_terminal_append_failure(
    rx: &mpsc::Receiver<WriterMessage>,
    state: &SenderState,
    completions: BTreeMap<String, Vec<mpsc::SyncSender<anyhow::Result<()>>>>,
    error: anyhow::Error,
) -> anyhow::Error {
    stop_and_reject_pending(rx, state);
    let canonical_uri = completions
        .iter()
        .find(|(_, waiters)| !waiters.is_empty())
        .map(|(uri, _)| uri.clone());
    let mut original = Some(error);
    for (uri, partition_completions) in completions {
        for completion in partition_completions {
            if let Some(error) = original.take() {
                original = send_owned_completion_error(&completion, error);
            } else {
                let _ = completion.send(Err(partition_append_failure(&uri)));
            }
        }
    }

    original.unwrap_or_else(|| {
        let worker_failure =
            anyhow::anyhow!("pChronicle append worker stopped after a terminal storage failure");
        match canonical_uri {
            Some(uri) => worker_failure.context(format!("append raw event partition {uri}")),
            None => worker_failure,
        }
    })
}

fn stop_and_reject_pending(rx: &mpsc::Receiver<WriterMessage>, state: &SenderState) {
    state.accepting.store(false, Ordering::SeqCst);
    while state.in_flight.load(Ordering::SeqCst) != 0 {
        std::thread::yield_now();
    }
    while let Ok(message_in_queue) = rx.try_recv() {
        if let WriterMessage::Append(job) = message_in_queue {
            let _ = notify_completion(
                job.completion,
                Err(anyhow::anyhow!(
                    "pChronicle append worker stopped before acknowledging the event"
                )),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeMap;

    use crate::store::RawEventLanceStore;

    fn event() -> EventRecord {
        EventRecord {
            identity: Default::default(),
            seq: 1,
            source: "test".into(),
            kind: "test".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: Value::Null,
        }
    }

    #[test]
    fn full_and_closed_are_expected_append_outcomes() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let sender = RawEventAppendSender {
            state: Arc::clone(&state),
        };
        let coords = StoryCoords::new("memory://queue", "agent", "session", None);
        assert_eq!(
            sender.try_append(coords.clone(), event()),
            RawEventAppendOutcome::Accepted
        );
        assert_eq!(
            sender.try_append(coords.clone(), event()),
            RawEventAppendOutcome::Full
        );
        state.accepting.store(false, Ordering::SeqCst);
        assert_eq!(
            sender.try_append(coords, event()),
            RawEventAppendOutcome::Unavailable
        );
    }

    #[test]
    fn disconnected_queue_is_an_unavailable_append_outcome() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let sender = RawEventAppendSender { state };

        assert_eq!(
            sender.try_append(
                StoryCoords::new("memory://queue", "agent", "session", None),
                event(),
            ),
            RawEventAppendOutcome::Unavailable
        );
    }

    #[test]
    fn durable_append_returns_only_after_event_is_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (sender, worker) = raw_event_append_queue_with_capacity(1).unwrap();

        assert_eq!(
            sender.append_durable(coords.clone(), event()).unwrap(),
            RawEventAppendOutcome::Accepted
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        assert_eq!(replay.records.len(), 1);
        worker.finish().unwrap();
    }

    #[test]
    fn single_writer_manifest_mode_publishes_object_store_events() {
        let storage = format!(
            "shared-memory://append-single-writer-{}/dataset",
            uuid::Uuid::new_v4()
        );
        let coords = StoryCoords::new(storage, "agent", "session", None);
        let (sender, worker) = raw_event_append_queue_with_manifest_write_mode(
            ObjectStoreManifestWriteMode::SingleWriter,
        )
        .unwrap();

        assert_eq!(
            sender.append_durable(coords.clone(), event()).unwrap(),
            RawEventAppendOutcome::Accepted
        );
        worker.finish().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        assert_eq!(replay.records.len(), 1);
    }

    #[test]
    fn durable_append_isolates_one_partition_failure() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_storage = dir.path().join("not-a-directory");
        std::fs::write(&invalid_storage, b"file").unwrap();
        let invalid = StoryCoords::new(invalid_storage.to_string_lossy(), "agent", "session", None);
        let valid = StoryCoords::new(
            dir.path().join("valid").to_string_lossy(),
            "agent",
            "session",
            None,
        );
        let (sender, worker) = raw_event_append_queue_with_capacity(1).unwrap();

        let error = sender.append_durable(invalid, event()).unwrap_err();
        assert!(
            error.chain().count() >= 2,
            "missing source chain: {error:#}"
        );
        assert_eq!(
            sender.append_durable(valid.clone(), event()).unwrap(),
            RawEventAppendOutcome::Accepted
        );
        worker.finish().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&valid, 0, None))
            .unwrap();
        assert_eq!(replay.records.len(), 1);
    }

    #[test]
    fn append_worker_bounds_live_small_fragments() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (sender, worker) = raw_event_append_queue().unwrap();

        for _ in 0..(DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD * 3) {
            sender.append_durable(coords.clone(), event()).unwrap();
        }
        worker.finish().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let layout = runtime
            .block_on(RawEventLanceStore.layout_stats(&coords))
            .unwrap();
        assert_eq!(
            layout.visible_rows,
            (DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD * 3) as u64
        );
        assert_eq!(
            layout.visible_segments, 3,
            "unexpected sealed layout: {layout:?}"
        );
        assert_eq!(
            layout.visible_fragments, 3,
            "sealed segments were not compacted: {layout:?}"
        );
    }

    #[test]
    fn append_worker_compacts_sealed_segments_across_multiple_levels() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (sender, worker) = raw_event_append_queue_with_options(
            32,
            2,
            2,
            2,
            ObjectStoreManifestWriteMode::Conditional,
        )
        .unwrap();

        // 8 rows become four L0 segments, two L1 segments, and finally one L2
        // segment. Each merge preserves append order and total row count.
        for seq in 0..8 {
            let mut record = event();
            record.seq = seq;
            sender.append_durable(coords.clone(), record).unwrap();
        }
        worker.finish().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let layout = runtime
            .block_on(RawEventLanceStore.layout_stats(&coords))
            .unwrap();
        assert_eq!(layout.visible_rows, 8);
        assert_eq!(layout.max_segment_level, 2);
        assert_eq!(layout.sealed_segments, 1);
        assert_eq!(
            layout.visible_segments, 1,
            "hierarchy did not carry: {layout:?}"
        );
        assert_eq!(layout.visible_fragments, 1);
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        let sequences = replay
            .records
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequences, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn multiple_durable_waiters_preserve_same_partition_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (tx, rx) = mpsc::sync_channel(4);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let sender = RawEventAppendSender {
            state: Arc::clone(&state),
        };
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let (second_tx, second_rx) = mpsc::sync_channel(1);
        for (seq, completion) in [(1, Some(first_tx)), (2, Some(second_tx)), (3, None)] {
            let mut record = event();
            record.seq = seq;
            assert_eq!(
                sender.enqueue(coords.clone(), record, completion),
                RawEventAppendOutcome::Accepted
            );
        }

        let worker_state = Arc::clone(&state);
        let join = std::thread::spawn(move || {
            run_append_worker(
                rx,
                worker_state,
                DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
                DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
                DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
                ObjectStoreManifestWriteMode::Conditional,
            )
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };

        first_rx.recv().unwrap().unwrap();
        second_rx.recv().unwrap().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let layout = runtime
            .block_on(RawEventLanceStore.layout_stats(&coords))
            .unwrap();
        assert_eq!(layout.visible_rows, 3);
        assert_eq!(
            layout.visible_fragments, 1,
            "one same-partition queue micro-batch must make one Lance commit"
        );
        worker.finish().unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        let sequences = replay
            .records
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);
    }

    #[test]
    fn failed_partition_batch_rejects_later_waiters_without_a_sequence_gap() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (tx, rx) = mpsc::sync_channel(4);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let sender = RawEventAppendSender {
            state: Arc::clone(&state),
        };
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let (second_tx, second_rx) = mpsc::sync_channel(1);
        let mut invalid = event();
        invalid.seq = 1;
        invalid.identity.timestamp_unix_ms = Some(1_000);
        invalid.timestamp = Some("1970-01-01T00:00:02Z".into());
        let mut second = event();
        second.seq = 2;
        assert_eq!(
            sender.enqueue(coords.clone(), invalid, Some(first_tx)),
            RawEventAppendOutcome::Accepted
        );
        assert_eq!(
            sender.enqueue(coords.clone(), second, Some(second_tx)),
            RawEventAppendOutcome::Accepted
        );

        let worker_state = Arc::clone(&state);
        let join = std::thread::spawn(move || {
            run_append_worker(
                rx,
                worker_state,
                DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
                DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
                DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
                ObjectStoreManifestWriteMode::Conditional,
            )
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };

        let first_error = first_rx.recv().unwrap().unwrap_err();
        assert!(
            format!("{first_error:#}").contains("timestamp"),
            "{first_error:#}"
        );
        let second_error = second_rx.recv().unwrap().unwrap_err();
        assert!(
            format!("{second_error:#}").contains("event partition"),
            "{second_error:#}"
        );

        let mut third = event();
        third.seq = 3;
        assert_eq!(
            sender.append_durable(coords.clone(), third).unwrap(),
            RawEventAppendOutcome::Accepted
        );
        worker.finish().unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        let sequences = replay
            .records
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![3]);
    }

    #[test]
    fn partition_failure_transfers_original_error_after_selected_receiver_is_dropped() {
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        drop(dropped_rx);
        let (live_tx, live_rx) = mpsc::sync_channel(1);
        let source_error = anyhow::Error::new(std::io::Error::other("partition-source-sentinel"))
            .context("append event partition");

        let unclaimed = notify_partition_completions(
            "memory://partition/events.lance",
            vec![dropped_tx, live_tx],
            Err(source_error),
        );

        assert!(unclaimed.is_none());
        let error = live_rx.recv().unwrap().unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("append event partition"), "{rendered}");
        assert!(rendered.contains("partition-source-sentinel"), "{rendered}");
    }

    #[test]
    fn terminal_append_failure_reaches_waiter_and_worker_finish() {
        let (tx, rx) = mpsc::sync_channel(2);
        tx.send(WriterMessage::Finish).unwrap();
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let uri = "memory://terminal/events.lance".to_string();
        let completions = BTreeMap::from([(uri.clone(), vec![completion_tx])]);
        let worker_state = Arc::clone(&state);
        let join = std::thread::spawn(move || -> anyhow::Result<()> {
            Err(complete_terminal_append_failure(
                &rx,
                &worker_state,
                completions,
                anyhow::Error::new(std::io::Error::other("terminal-source-sentinel"))
                    .context("append raw event batch"),
            ))
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };

        let waiter_error = completion_rx.recv().unwrap().unwrap_err();
        let waiter_rendered = format!("{waiter_error:#}");
        assert!(waiter_rendered.contains("terminal-source-sentinel"));

        let finish_error = worker.finish().unwrap_err();
        let finish_rendered = format!("{finish_error:#}");
        assert!(finish_rendered.contains("terminal storage failure"));
        assert!(finish_rendered.contains(&uri));
    }

    #[test]
    fn terminal_failure_returns_original_when_all_completion_receivers_are_dropped() {
        let (tx, rx) = mpsc::sync_channel(2);
        tx.send(WriterMessage::Finish).unwrap();
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        drop(completion_rx);
        let completions = BTreeMap::from([(
            "memory://terminal/events.lance".to_string(),
            vec![completion_tx],
        )]);
        let worker_state = Arc::clone(&state);
        let join = std::thread::spawn(move || -> anyhow::Result<()> {
            Err(complete_terminal_append_failure(
                &rx,
                &worker_state,
                completions,
                anyhow::Error::new(std::io::Error::other("dropped-terminal-source-sentinel"))
                    .context("append raw event batch"),
            ))
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };

        let error = worker.finish().unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("append raw event batch"), "{rendered}");
        assert!(
            rendered.contains("dropped-terminal-source-sentinel"),
            "{rendered}"
        );
    }

    #[test]
    fn worker_panic_is_reported_by_finish() {
        let (tx, rx) = mpsc::sync_channel(1);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let join = std::thread::spawn(move || -> anyhow::Result<()> {
            drop(rx);
            panic!("injected append worker panic");
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };

        let error = worker.finish().unwrap_err();
        assert!(error.to_string().contains("worker thread panicked"));
    }

    #[test]
    fn worker_close_failure_keeps_the_channel_source() {
        let (tx, rx) = mpsc::sync_channel(1);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let join = std::thread::spawn(move || -> anyhow::Result<()> {
            drop(rx);
            closed_tx.send(()).unwrap();
            Ok(())
        });
        let worker = RawEventAppendWorker {
            state,
            join: Some(join),
        };
        closed_rx.recv().unwrap();

        let error = worker.finish().unwrap_err();
        assert!(
            error.chain().count() >= 2,
            "missing source chain: {error:#}"
        );
    }
}
