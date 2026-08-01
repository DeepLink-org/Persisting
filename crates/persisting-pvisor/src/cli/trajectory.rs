use std::path::Path;
use std::sync::{mpsc, Arc};

use anyhow::Context;
use persisting_gateway::record::CaptureRecord;
use persisting_gateway::session_storage::CaptureRoute;
use persisting_gateway::sink::CallbackSink;
use persisting_pchronicle::{LanceEventStore, StoryCoords, StructuredStore};

use crate::TrajectoryEventSink;

struct AppendJob {
    route: CaptureRoute,
    agent_id: String,
    record: CaptureRecord,
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
) -> (Arc<dyn TrajectoryEventSink>, ChronicleWriter) {
    let storage = storage.to_path_buf();
    let (tx, rx) = mpsc::sync_channel::<AppendJob>(256);
    let join = std::thread::spawn(move || -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("pChronicle writer runtime")?;
        let store = LanceEventStore;
        while let Ok(job) = rx.recv() {
            let root_session = job.route.append_root_session();
            let session = StoryCoords::new(
                storage.display().to_string(),
                job.agent_id,
                job.route.storage_session_id,
                root_session,
            );
            runtime
                .block_on(store.append_events(&session, &[job.record]))
                .context("append Gateway event to pChronicle")?;
        }
        Ok(())
    });

    let callback = CallbackSink::new(default_agent_id, move |route, agent_id, record| {
        tx.send(AppendJob {
            route: route.clone(),
            agent_id: agent_id.to_string(),
            record,
        })
        .map_err(|error| anyhow::anyhow!("pChronicle writer closed: {error}"))
    });
    (Arc::new(callback), ChronicleWriter { join: Some(join) })
}
