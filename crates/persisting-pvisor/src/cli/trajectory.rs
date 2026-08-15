use std::path::Path;
use std::sync::Arc;

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use async_trait::async_trait;
#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use persisting_gateway::record::CaptureRecord;
#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use persisting_gateway::session::storage::CaptureRoute;
#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use persisting_gateway::sink::CallbackSink;
#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use persisting_pchronicle::{
    raw_event_append_queue, EventRecord, RawEventAppendSender, RawEventAppendWorker, StoryCoords,
};

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
use crate::EventAppendErrorKind;
use crate::{EventSink, TrajectoryEventSink};

type ChronicleSinks = (
    Arc<dyn TrajectoryEventSink>,
    Arc<dyn EventSink>,
    ChronicleWriter,
);

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
struct ChronicleEventSink {
    tx: RawEventAppendSender,
    storage: String,
    run_id: String,
    agent_id: String,
}

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
#[async_trait]
impl EventSink for ChronicleEventSink {
    async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
        let tx = self.tx.clone();
        let coords = StoryCoords::new(
            self.storage.clone(),
            self.agent_id.clone(),
            self.run_id.clone(),
            Some(self.run_id.clone()),
        );
        let event = event.clone();
        tokio::task::spawn_blocking(move || tx.append_durable(coords, event))
            .await
            .map_err(|error| anyhow::anyhow!("pChronicle append task failed: {error}"))??;
        Ok(())
    }

    fn classify_append_error(&self, error: &anyhow::Error) -> EventAppendErrorKind {
        match error.downcast_ref::<persisting_pchronicle::RawEventAppendQueueError>() {
            Some(
                persisting_pchronicle::RawEventAppendQueueError::Full
                | persisting_pchronicle::RawEventAppendQueueError::Closed,
            ) => EventAppendErrorKind::Rejected,
            Some(persisting_pchronicle::RawEventAppendQueueError::Write(_)) | None => {
                EventAppendErrorKind::Unknown
            }
        }
    }
}

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
pub struct ChronicleWriter {
    worker: RawEventAppendWorker,
}

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
impl ChronicleWriter {
    pub fn finish(self) -> anyhow::Result<()> {
        self.worker.finish()
    }
}

#[cfg(not(any(feature = "lance-chronicle", feature = "local-lance-chronicle")))]
pub struct ChronicleWriter;

#[cfg(not(any(feature = "lance-chronicle", feature = "local-lance-chronicle")))]
impl ChronicleWriter {
    pub fn finish(self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(any(feature = "lance-chronicle", feature = "local-lance-chronicle"))]
pub fn chronicle_sink(
    storage: &Path,
    default_agent_id: &str,
    run_id: &str,
) -> anyhow::Result<ChronicleSinks> {
    let storage = storage.display().to_string();
    let (tx, worker) = raw_event_append_queue()?;

    let trajectory_tx = tx.clone();
    let callback_storage = storage.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: CaptureRecord| {
            trajectory_tx.append_durable(
                StoryCoords::new(
                    callback_storage.clone(),
                    agent_id,
                    route.storage_session_id.clone(),
                    route.append_root_session(),
                ),
                record,
            )?;
            Ok(())
        },
    );
    let lifecycle = ChronicleEventSink {
        tx,
        storage: storage.clone(),
        run_id: run_id.to_string(),
        agent_id: default_agent_id.to_string(),
    };
    Ok((
        Arc::new(callback),
        Arc::new(lifecycle),
        ChronicleWriter { worker },
    ))
}

#[cfg(not(any(feature = "lance-chronicle", feature = "local-lance-chronicle")))]
pub fn chronicle_sink(
    _storage: &Path,
    _default_agent_id: &str,
    _run_id: &str,
) -> anyhow::Result<ChronicleSinks> {
    anyhow::bail!(
        "Lance trajectory capture is not part of the lightweight pVisor build; rebuild with `--features lance-chronicle`"
    )
}
