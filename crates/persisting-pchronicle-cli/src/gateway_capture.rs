//! Bridge Gateway's synchronous capture sink to pChronicle's async Lance writer.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use persisting_gateway::record::CaptureRecord;
use persisting_gateway::session::storage::CaptureRoute;
use persisting_gateway::sink::{CallbackSink, CaptureEventSink};
use persisting_pchronicle::{EventRecord, RawEventLanceAppender, StoryCoords};

const APPEND_BATCH_SIZE: usize = 256;
const APPEND_BATCH_DELAY: Duration = Duration::from_millis(2);

struct AppendJob {
    storage_session_id: String,
    root_session_id: Option<String>,
    agent_id: String,
    record: EventRecord,
}

enum WriterMessage {
    Append(Box<AppendJob>),
    Finish,
}

pub(crate) struct GatewayCaptureWriter {
    finish_tx: Option<mpsc::SyncSender<WriterMessage>>,
    join: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl GatewayCaptureWriter {
    pub(crate) fn finish(mut self) -> anyhow::Result<()> {
        let finish_signal = if let Some(tx) = self.finish_tx.take() {
            tx.send(WriterMessage::Finish)
                .map_err(|error| anyhow::anyhow!("pChronicle Gateway writer closed: {error}"))
        } else {
            Ok(())
        };
        let Some(join) = self.join.take() else {
            return finish_signal;
        };
        join.join()
            .map_err(|_| anyhow::anyhow!("pChronicle Gateway writer thread panicked"))??;
        finish_signal
    }
}

pub(crate) fn gateway_capture_sink(
    dataset_uri: &str,
    default_agent_id: &str,
) -> (Arc<dyn CaptureEventSink>, GatewayCaptureWriter) {
    let dataset_uri = dataset_uri.to_string();
    let (tx, rx) = mpsc::sync_channel::<WriterMessage>(256);
    let join = std::thread::spawn(move || -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("create pChronicle Gateway writer runtime")?;
        let mut appender = RawEventLanceAppender::default();
        let mut finishing = false;
        while !finishing {
            let first = match rx.recv() {
                Ok(WriterMessage::Append(job)) => *job,
                Ok(WriterMessage::Finish) | Err(_) => break,
            };
            let mut jobs = Vec::with_capacity(APPEND_BATCH_SIZE);
            jobs.push(first);
            while jobs.len() < APPEND_BATCH_SIZE {
                match rx.recv_timeout(APPEND_BATCH_DELAY) {
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
                .map(|job| {
                    (
                        StoryCoords::new(
                            dataset_uri.clone(),
                            job.agent_id,
                            job.storage_session_id,
                            job.root_session_id,
                        ),
                        job.record,
                    )
                })
                .collect::<Vec<_>>();
            runtime
                .block_on(appender.append_event_batch(&entries))
                .context("append Gateway event batch to pChronicle Dataset")?;
        }
        // Capture is append-only. Indexing, compaction, and vacuum remain
        // explicit pChronicle maintenance operations.
        let _reports = appender.finish();
        Ok(())
    });

    let callback_tx = tx.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: CaptureRecord| {
            callback_tx
                .send(WriterMessage::Append(Box::new(AppendJob {
                    storage_session_id: route.storage_session_id.clone(),
                    root_session_id: route.append_root_session(),
                    agent_id: agent_id.to_string(),
                    record,
                })))
                .map_err(|error| anyhow::anyhow!("pChronicle Gateway writer closed: {error}"))
        },
    );
    (
        Arc::new(callback),
        GatewayCaptureWriter {
            finish_tx: Some(tx),
            join: Some(join),
        },
    )
}
