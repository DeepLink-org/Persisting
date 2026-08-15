//! Bridge Gateway's synchronous capture sink to pChronicle's async Lance writer.

use std::sync::Arc;

use persisting_gateway::record::CaptureRecord;
use persisting_gateway::session::storage::CaptureRoute;
use persisting_gateway::sink::{CallbackSink, CaptureEventSink};
use persisting_pchronicle::{raw_event_append_queue, RawEventAppendWorker, StoryCoords};

pub(crate) struct GatewayCaptureWriter {
    worker: RawEventAppendWorker,
}

impl GatewayCaptureWriter {
    pub(crate) fn finish(self) -> anyhow::Result<()> {
        self.worker.finish()
    }
}

pub(crate) fn gateway_capture_sink(
    dataset_uri: &str,
    default_agent_id: &str,
) -> (Arc<dyn CaptureEventSink>, GatewayCaptureWriter) {
    let dataset_uri = dataset_uri.to_string();
    let (tx, worker) = raw_event_append_queue()
        .expect("pChronicle Gateway append worker must start before serving requests");

    let callback_tx = tx.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: CaptureRecord| {
            callback_tx.try_append(
                StoryCoords::new(
                    dataset_uri.clone(),
                    agent_id,
                    route.storage_session_id.clone(),
                    route.append_root_session(),
                ),
                record,
            )?;
            Ok(())
        },
    );
    (Arc::new(callback), GatewayCaptureWriter { worker })
}
