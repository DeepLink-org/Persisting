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
            callback_tx.append_durable(
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

#[cfg(test)]
mod tests {
    use persisting_gateway::record::CaptureRecord;
    use persisting_pchronicle::{RawEventLanceStore, StructuredStore};

    use super::*;

    #[test]
    fn gateway_sink_returns_after_lance_event_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let dataset_uri = dir.path().join("dataset").to_string_lossy().into_owned();
        let (sink, writer) = gateway_capture_sink(&dataset_uri, "agent");
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "session".into(),
            storage_session_id: "session".into(),
            subagent_id: None,
        };
        let mut record = CaptureRecord {
            identity: Default::default(),
            seq: 0,
            source: "gateway".into(),
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
            payload: serde_json::Value::Null,
        };

        sink.append(&route, "agent", &mut record).unwrap();

        let coords = StoryCoords::new(dataset_uri, "agent", "session", Some("run".into()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let replay = runtime
            .block_on(RawEventLanceStore.replay(&coords, 0, None))
            .unwrap();
        assert_eq!(replay.records.len(), 1);
        writer.finish().unwrap();
    }
}
