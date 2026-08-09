use std::path::Path;
#[cfg(feature = "lance-chronicle")]
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(feature = "lance-chronicle")]
use std::time::Duration;

#[cfg(feature = "lance-chronicle")]
use anyhow::Context;
#[cfg(feature = "lance-chronicle")]
use async_trait::async_trait;
#[cfg(feature = "lance-chronicle")]
use persisting_gateway::record::CaptureRecord;
#[cfg(feature = "lance-chronicle")]
use persisting_gateway::session::storage::CaptureRoute;
#[cfg(feature = "lance-chronicle")]
use persisting_gateway::sink::CallbackSink;
#[cfg(feature = "lance-chronicle")]
use persisting_pchronicle::{EventRecord, RawEventLanceAppender, StoryCoords};

use crate::{EventSink, TrajectoryEventSink};

type ChronicleSinks = (
    Arc<dyn TrajectoryEventSink>,
    Arc<dyn EventSink>,
    ChronicleWriter,
);

#[cfg(feature = "lance-chronicle")]
struct AppendJob {
    storage_session_id: String,
    root_session_id: Option<String>,
    agent_id: String,
    record: EventRecord,
}

#[cfg(feature = "lance-chronicle")]
const CHRONICLE_APPEND_BATCH_SIZE: usize = 256;
#[cfg(feature = "lance-chronicle")]
const CHRONICLE_APPEND_BATCH_DELAY: Duration = Duration::from_millis(2);

#[cfg(feature = "lance-chronicle")]
struct ChronicleEventSink {
    tx: mpsc::SyncSender<AppendJob>,
    run_id: String,
    agent_id: String,
}

#[cfg(feature = "lance-chronicle")]
#[async_trait]
impl EventSink for ChronicleEventSink {
    async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
        self.tx
            .send(AppendJob {
                storage_session_id: self.run_id.clone(),
                root_session_id: Some(self.run_id.clone()),
                agent_id: self.agent_id.clone(),
                record: event.clone(),
            })
            .map_err(|error| anyhow::anyhow!("pChronicle writer closed: {error}"))
    }
}

#[cfg(feature = "lance-chronicle")]
pub struct ChronicleWriter {
    join: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

#[cfg(feature = "lance-chronicle")]
impl ChronicleWriter {
    pub fn finish(mut self) -> anyhow::Result<()> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join()
            .map_err(|_| anyhow::anyhow!("pChronicle writer thread panicked"))??;
        Ok(())
    }
}

#[cfg(not(feature = "lance-chronicle"))]
pub struct ChronicleWriter;

#[cfg(not(feature = "lance-chronicle"))]
impl ChronicleWriter {
    pub fn finish(self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(feature = "lance-chronicle")]
pub fn chronicle_sink(
    storage: &Path,
    default_agent_id: &str,
    run_id: &str,
) -> anyhow::Result<ChronicleSinks> {
    let storage = storage.to_path_buf();
    let (tx, rx) = mpsc::sync_channel::<AppendJob>(256);
    let join = std::thread::spawn(move || -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("pChronicle writer runtime")?;
        let mut appender = RawEventLanceAppender::default();
        while let Ok(first) = rx.recv() {
            let mut jobs = Vec::with_capacity(CHRONICLE_APPEND_BATCH_SIZE);
            jobs.push(first);
            while jobs.len() < CHRONICLE_APPEND_BATCH_SIZE {
                match rx.recv_timeout(CHRONICLE_APPEND_BATCH_DELAY) {
                    Ok(job) => jobs.push(job),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            let entries = jobs
                .into_iter()
                .map(|job| {
                    (
                        StoryCoords::new(
                            storage.display().to_string(),
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
                .context("append Gateway event batch to pChronicle")?;
        }
        // Closing an append-only writer never runs indexing, compaction, or
        // vacuum. Operators invoke maintenance explicitly outside capture.
        let _reports = appender.finish();
        Ok(())
    });

    let trajectory_tx = tx.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: CaptureRecord| {
            trajectory_tx
                .send(AppendJob {
                    storage_session_id: route.storage_session_id.clone(),
                    root_session_id: route.append_root_session(),
                    agent_id: agent_id.to_string(),
                    record,
                })
                .map_err(|error| anyhow::anyhow!("pChronicle writer closed: {error}"))
        },
    );
    let lifecycle = ChronicleEventSink {
        tx,
        run_id: run_id.to_string(),
        agent_id: default_agent_id.to_string(),
    };
    Ok((
        Arc::new(callback),
        Arc::new(lifecycle),
        ChronicleWriter { join: Some(join) },
    ))
}

#[cfg(not(feature = "lance-chronicle"))]
pub fn chronicle_sink(
    _storage: &Path,
    _default_agent_id: &str,
    _run_id: &str,
) -> anyhow::Result<ChronicleSinks> {
    anyhow::bail!(
        "Lance trajectory capture is not part of the lightweight pVisor build; rebuild with `--features lance-chronicle`"
    )
}
