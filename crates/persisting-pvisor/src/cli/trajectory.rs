use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use persisting_gateway::record::CaptureRecord;
use persisting_gateway::session_storage::CaptureRoute;
use persisting_gateway::sink::CallbackSink;
use persisting_pchronicle::{
    EventRecord, LanceMaintenanceOptions, RawEventLanceAppender, StoryCoords,
};

use crate::{EventSink, TrajectoryEventSink};

struct AppendJob {
    storage_session_id: String,
    root_session_id: Option<String>,
    agent_id: String,
    record: EventRecord,
}

const CHRONICLE_APPEND_BATCH_SIZE: usize = 256;
const CHRONICLE_APPEND_BATCH_DELAY: Duration = Duration::from_millis(2);

struct ChronicleEventSink {
    tx: mpsc::SyncSender<AppendJob>,
    run_id: String,
    agent_id: String,
}

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

pub struct ChronicleWriter {
    join: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

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

pub fn chronicle_sink(
    storage: &Path,
    default_agent_id: &str,
    run_id: &str,
) -> (
    Arc<dyn TrajectoryEventSink>,
    Arc<dyn EventSink>,
    ChronicleWriter,
) {
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
        runtime
            .block_on(appender.finish(&LanceMaintenanceOptions::default()))
            .context("maintain pChronicle event datasets")?;
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
    (
        Arc::new(callback),
        Arc::new(lifecycle),
        ChronicleWriter { join: Some(join) },
    )
}
