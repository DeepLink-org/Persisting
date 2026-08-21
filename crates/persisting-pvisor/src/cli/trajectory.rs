//! pChronicle sidecar adapter for pVisor lifecycle and Gateway events.
//!
//! pVisor owns only the shared event contract and this lightweight control
//! client. The sidecar process owns Lance, DataFusion, and object-store code.

use std::path::Path;
use std::sync::{mpsc, Arc};

use async_trait::async_trait;
use persisting_events::EventRecord;
use persisting_events::{ChronicleControl, ChronicleServeProcessClient, TrajectoryAppendRequest};
use persisting_gateway::session::storage::CaptureRoute;
use persisting_gateway::sink::CallbackSink;

use crate::{EventAppendErrorKind, EventSink, TrajectoryEventSink};

type ChronicleSinks = (
    Arc<dyn TrajectoryEventSink>,
    Arc<dyn EventSink>,
    ChronicleWriter,
    Arc<dyn ChronicleControl>,
);

const APPEND_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, thiserror::Error)]
enum SidecarAppendError {
    #[error("pChronicle sidecar append queue is full")]
    Full,
    #[error("pChronicle sidecar append worker is closed")]
    Closed,
    #[error("pChronicle sidecar append failed: {0}")]
    Write(String),
}

struct AppendCommand {
    request: TrajectoryAppendRequest,
    ack: mpsc::SyncSender<Result<(), SidecarAppendError>>,
}

#[derive(Clone)]
struct SidecarAppendSender {
    tx: mpsc::SyncSender<AppendCommand>,
    storage: String,
}

impl SidecarAppendSender {
    fn append_durable(
        &self,
        agent_id: String,
        session_id: String,
        root_session_id: Option<String>,
        record: EventRecord,
    ) -> anyhow::Result<()> {
        let (ack, response) = mpsc::sync_channel(1);
        let command = AppendCommand {
            request: TrajectoryAppendRequest {
                storage: self.storage.clone(),
                agent_id,
                session_id,
                root_session_id,
                records: vec![record],
            },
            ack,
        };
        match self.tx.try_send(command) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => return Err(SidecarAppendError::Full.into()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(SidecarAppendError::Closed.into())
            }
        }
        response.recv().map_err(|_| SidecarAppendError::Closed)??;
        Ok(())
    }
}

struct ChronicleEventSink {
    tx: SidecarAppendSender,
    run_id: String,
    agent_id: String,
}

#[async_trait]
impl EventSink for ChronicleEventSink {
    async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
        let tx = self.tx.clone();
        let agent_id = self.agent_id.clone();
        let run_id = self.run_id.clone();
        let event = event.clone();
        tokio::task::spawn_blocking(move || {
            tx.append_durable(agent_id, run_id.clone(), Some(run_id), event)
        })
        .await
        .map_err(|error| anyhow::anyhow!("pChronicle append task failed: {error}"))??;
        Ok(())
    }

    fn classify_append_error(&self, error: &anyhow::Error) -> EventAppendErrorKind {
        match error.downcast_ref::<SidecarAppendError>() {
            Some(SidecarAppendError::Full | SidecarAppendError::Closed) => {
                EventAppendErrorKind::Rejected
            }
            Some(SidecarAppendError::Write(_)) | None => EventAppendErrorKind::Unknown,
        }
    }
}

pub struct ChronicleWriter {
    sender: Option<SidecarAppendSender>,
    worker: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl ChronicleWriter {
    pub fn finish(mut self) -> anyhow::Result<()> {
        self.sender.take();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("pChronicle append worker panicked"))?
    }
}

pub async fn chronicle_sink(
    storage: &Path,
    default_agent_id: &str,
    run_id: &str,
    binary: &Path,
) -> anyhow::Result<ChronicleSinks> {
    let storage = storage.display().to_string();
    let control: Arc<dyn ChronicleControl> =
        Arc::new(ChronicleServeProcessClient::spawn(binary, storage.clone()).await?);
    let (tx, rx) = mpsc::sync_channel::<AppendCommand>(APPEND_QUEUE_CAPACITY);
    let worker_control = Arc::clone(&control);
    let worker = std::thread::Builder::new()
        .name("pvisor-pchronicle-append".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            for command in rx {
                let outcome = runtime
                    .block_on(worker_control.append_trajectory(command.request))
                    .map(|_| ())
                    .map_err(|error| SidecarAppendError::Write(format!("{error:#}")));
                let _ = command.ack.send(outcome);
            }
            Ok(())
        })?;
    let sender = SidecarAppendSender {
        tx,
        storage: storage.clone(),
    };

    let trajectory_tx = sender.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: EventRecord| {
            trajectory_tx.append_durable(
                agent_id.to_string(),
                route.storage_session_id.clone(),
                route.append_root_session(),
                record,
            )
        },
    );
    let lifecycle = ChronicleEventSink {
        tx: sender.clone(),
        run_id: run_id.to_string(),
        agent_id: default_agent_id.to_string(),
    };
    Ok((
        Arc::new(callback),
        Arc::new(lifecycle),
        ChronicleWriter {
            sender: Some(sender),
            worker: Some(worker),
        },
        control,
    ))
}
