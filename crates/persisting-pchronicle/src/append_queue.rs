//! Bounded bridge from synchronous capture callbacks to the async Lance appender.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::Context;
use thiserror::Error;

use crate::store::compact_sealed_event_segment;
use crate::{raw_event_lance_path, EventRecord, RawEventLanceAppender, StoryCoords};

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
    completion: Option<mpsc::SyncSender<Result<(), String>>>,
}

#[derive(Debug)]
enum WriterMessage {
    Append(Box<RawEventAppendJob>),
    Finish,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RawEventAppendQueueError {
    #[error("pChronicle append queue is full")]
    Full,
    #[error("pChronicle append queue is closed")]
    Closed,
    #[error("pChronicle append failed: {0}")]
    Write(String),
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
    pub fn try_append(
        &self,
        coords: StoryCoords,
        record: EventRecord,
    ) -> Result<(), RawEventAppendQueueError> {
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
    ) -> Result<(), RawEventAppendQueueError> {
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        self.enqueue(coords, record, Some(completion_tx))?;
        completion_rx
            .recv()
            .map_err(|_| RawEventAppendQueueError::Closed)?
            .map_err(RawEventAppendQueueError::Write)
    }

    fn enqueue(
        &self,
        coords: StoryCoords,
        record: EventRecord,
        completion: Option<mpsc::SyncSender<Result<(), String>>>,
    ) -> Result<(), RawEventAppendQueueError> {
        if !self.state.accepting.load(Ordering::SeqCst) {
            return Err(RawEventAppendQueueError::Closed);
        }

        self.state.in_flight.fetch_add(1, Ordering::SeqCst);
        if !self.state.accepting.load(Ordering::SeqCst) {
            self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(RawEventAppendQueueError::Closed);
        }

        let result = self
            .state
            .tx
            .try_send(WriterMessage::Append(Box::new(RawEventAppendJob {
                coords,
                record,
                completion,
            })))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => RawEventAppendQueueError::Full,
                mpsc::TrySendError::Disconnected(_) => RawEventAppendQueueError::Closed,
            });
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
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
            .map_err(|error| anyhow::anyhow!("pChronicle append worker closed: {error}"));
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

pub fn raw_event_append_queue_with_capacity(
    capacity: usize,
) -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)> {
    raw_event_append_queue_with_options(
        capacity,
        DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
        DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
        DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
    )
}

fn raw_event_append_queue_with_options(
    capacity: usize,
    compaction_threshold: usize,
    target_rows_per_fragment: usize,
    hierarchy_fanout: usize,
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
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("create pChronicle append worker runtime")?;
    let mut appender = RawEventLanceAppender::default();
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

        let mut completions = Vec::with_capacity(jobs.len());
        let mut entries = Vec::with_capacity(jobs.len());
        for job in jobs {
            match raw_event_lance_path(&job.coords) {
                Ok(path) => {
                    completions.push((path.to_string_lossy().into_owned(), job.completion));
                    entries.push((job.coords, job.record));
                }
                Err(error) => notify_completion(job.completion, Err(format!("{error:#}"))),
            }
        }
        if entries.is_empty() {
            continue;
        }
        let append_result = runtime
            .block_on(appender.append_event_batch_partitioned(&entries))
            .context("append event batch to pChronicle");
        match append_result {
            Ok(report) => {
                for (uri, completion) in completions {
                    let result = match report.outcome_for(&uri) {
                        Some(Ok(_)) => Ok(()),
                        Some(Err(error)) => Err(error.clone()),
                        None => Err(format!(
                            "pChronicle append returned no partition outcome for {uri}"
                        )),
                    };
                    if completion.is_none() {
                        if let Err(error) = &result {
                            tracing::warn!(
                                target: "persisting_pchronicle",
                                "non-durable raw-event append failed for {uri}: {error}"
                            );
                        }
                    }
                    notify_completion(completion, result);
                }
                match appender.seal_fragmented_segments(compaction_threshold) {
                    Ok(segments) => {
                        for segment in segments {
                            enqueue_maintenance(&maintenance_tx, segment);
                        }
                    }
                    Err(error) => {
                        // The append and its manifest publication are already
                        // durable. Keep maintenance best-effort so a compaction
                        // failure cannot turn a successful capture into a retry.
                        tracing::warn!(
                            target: "persisting_pchronicle",
                            "background raw-event segment sealing failed: {error:#}"
                        );
                    }
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                // Publish the terminal queue state before waking callers from
                // the failed batch. Once a durable append observes `Write`, a
                // subsequent append must deterministically observe `Closed`
                // instead of racing into the drain path and receiving the same
                // writer error again.
                stop_and_reject_pending(&rx, &state, &message);
                for (_, completion) in completions {
                    notify_completion(completion, Err(message.clone()));
                }
                return Err(error);
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

fn notify_completions(
    completions: &[Option<mpsc::SyncSender<Result<(), String>>>],
    result: Result<(), String>,
) {
    for completion in completions.iter().flatten() {
        let _ = completion.send(result.clone());
    }
}

fn notify_completion(
    completion: Option<mpsc::SyncSender<Result<(), String>>>,
    result: Result<(), String>,
) {
    if let Some(completion) = completion {
        let _ = completion.send(result);
    }
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

fn stop_and_reject_pending(rx: &mpsc::Receiver<WriterMessage>, state: &SenderState, message: &str) {
    state.accepting.store(false, Ordering::SeqCst);
    while state.in_flight.load(Ordering::SeqCst) != 0 {
        std::thread::yield_now();
    }
    while let Ok(message_in_queue) = rx.try_recv() {
        if let WriterMessage::Append(job) = message_in_queue {
            notify_completions(&[job.completion], Err(message.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    use crate::RawEventLanceStore;

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
    fn full_queue_is_reported_without_waiting() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let state = Arc::new(SenderState {
            tx,
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
        });
        let sender = RawEventAppendSender { state };
        let coords = StoryCoords::new("memory://queue", "agent", "session", None);
        let record = event();

        sender.try_append(coords.clone(), record.clone()).unwrap();
        assert_eq!(
            sender.try_append(coords, record),
            Err(RawEventAppendQueueError::Full)
        );
    }

    #[test]
    fn durable_append_returns_only_after_event_is_replayable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let (sender, worker) = raw_event_append_queue_with_capacity(1).unwrap();

        sender.append_durable(coords.clone(), event()).unwrap();

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

        assert!(matches!(
            sender.append_durable(invalid, event()),
            Err(RawEventAppendQueueError::Write(_))
        ));
        sender.append_durable(valid.clone(), event()).unwrap();
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
        let (sender, worker) = raw_event_append_queue_with_options(32, 2, 2, 2).unwrap();

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
}
