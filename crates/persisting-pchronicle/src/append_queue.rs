//! Bounded bridge from synchronous capture callbacks to the async Lance appender.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::Context;
use thiserror::Error;

use crate::{EventRecord, RawEventLanceAppender, StoryCoords};

pub const DEFAULT_RAW_EVENT_QUEUE_CAPACITY: usize = 256;
pub const DEFAULT_RAW_EVENT_BATCH_SIZE: usize = 256;
pub const DEFAULT_RAW_EVENT_BATCH_DELAY: Duration = Duration::from_millis(2);

#[derive(Debug)]
struct RawEventAppendJob {
    coords: StoryCoords,
    record: EventRecord,
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
    if capacity == 0 {
        anyhow::bail!("pChronicle append queue capacity must be greater than zero");
    }

    let (tx, rx) = mpsc::sync_channel::<WriterMessage>(capacity);
    let state = Arc::new(SenderState {
        tx,
        accepting: AtomicBool::new(true),
        in_flight: AtomicUsize::new(0),
    });
    let join = std::thread::Builder::new()
        .name("pchronicle-append".to_string())
        .spawn(move || run_append_worker(rx))
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

fn run_append_worker(rx: mpsc::Receiver<WriterMessage>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create pChronicle append worker runtime")?;
    let mut appender = RawEventLanceAppender::default();
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

        let entries = jobs
            .into_iter()
            .map(|job| (job.coords, job.record))
            .collect::<Vec<_>>();
        runtime
            .block_on(appender.append_event_batch(&entries))
            .context("append event batch to pChronicle")?;
    }

    // Capture is append-only. Indexing, compaction, and vacuum remain explicit
    // pChronicle maintenance operations.
    let _reports = appender.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
}
