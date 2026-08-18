//! Bridge Gateway's synchronous capture sink to pChronicle's async Lance writer.

use std::sync::Arc;

use persisting_gateway::record::EventRecord;
use persisting_gateway::session::storage::CaptureRoute;
use persisting_gateway::sink::{CallbackSink, CaptureEventSink};
use persisting_pchronicle::storage::{
    raw_event_append_queue, RawEventAppendOutcome, RawEventAppendSender, RawEventAppendWorker,
    StoryCoords,
};

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
) -> anyhow::Result<(Arc<dyn CaptureEventSink>, GatewayCaptureWriter)> {
    gateway_capture_sink_with_factory(dataset_uri, default_agent_id, raw_event_append_queue)
}

fn gateway_capture_sink_with_factory<F>(
    dataset_uri: &str,
    default_agent_id: &str,
    queue_factory: F,
) -> anyhow::Result<(Arc<dyn CaptureEventSink>, GatewayCaptureWriter)>
where
    F: FnOnce() -> anyhow::Result<(RawEventAppendSender, RawEventAppendWorker)>,
{
    let dataset_uri = dataset_uri.to_string();
    let (tx, worker) = queue_factory()?;

    let callback_tx = tx.clone();
    let callback = CallbackSink::new(
        default_agent_id,
        move |route: &CaptureRoute, agent_id, record: EventRecord| {
            let outcome = callback_tx.append_durable(
                StoryCoords::new(
                    dataset_uri.clone(),
                    agent_id,
                    route.storage_session_id.clone(),
                    route.append_root_session(),
                ),
                record,
            )?;
            map_append_outcome(outcome)
        },
    );
    Ok((Arc::new(callback), GatewayCaptureWriter { worker }))
}

fn map_append_outcome(outcome: RawEventAppendOutcome) -> anyhow::Result<()> {
    match outcome {
        RawEventAppendOutcome::Accepted => Ok(()),
        RawEventAppendOutcome::Full => anyhow::bail!("pChronicle append capacity exhausted"),
        RawEventAppendOutcome::Unavailable => {
            anyhow::bail!("pChronicle append queue is unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use persisting_gateway::record::EventRecord;
    use persisting_pchronicle::storage::{RawEventAppendOutcome, RawEventLanceStore};

    use super::*;

    #[test]
    fn gateway_sink_returns_after_lance_event_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let dataset_uri = dir.path().join("dataset").to_string_lossy().into_owned();
        let (sink, writer) = gateway_capture_sink(&dataset_uri, "agent").unwrap();
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "session".into(),
            storage_session_id: "session".into(),
            subagent_id: None,
        };
        let mut record = EventRecord {
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

    #[test]
    fn gateway_maps_only_explicit_append_rejections() {
        map_append_outcome(RawEventAppendOutcome::Accepted).unwrap();
        assert!(map_append_outcome(RawEventAppendOutcome::Full)
            .unwrap_err()
            .to_string()
            .contains("capacity"));
        assert!(map_append_outcome(RawEventAppendOutcome::Unavailable)
            .unwrap_err()
            .to_string()
            .contains("unavailable"));
    }

    #[test]
    fn gateway_sink_preserves_append_source_chain_and_rejected_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let dataset_uri = dir.path().join("not-a-directory");
        std::fs::write(&dataset_uri, b"file").unwrap();
        let (sink, writer) =
            gateway_capture_sink(dataset_uri.to_string_lossy().as_ref(), "agent").unwrap();
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "session".into(),
            storage_session_id: "session".into(),
            subagent_id: None,
        };
        let mut record = EventRecord {
            identity: Default::default(),
            seq: 99,
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

        let error = sink.append(&route, "agent", &mut record).unwrap_err();

        assert!(
            error.chain().count() >= 2,
            "missing source chain: {error:#}"
        );
        assert_eq!(sink.peek_next_seq(&route), Some(0));
        writer.finish().unwrap();
    }

    #[test]
    fn gateway_queue_start_failure_preserves_source_chain() {
        let result = gateway_capture_sink_with_factory("memory://capture", "agent", || {
            Err(
                anyhow::Error::new(std::io::Error::other("spawn-failure-sentinel"))
                    .context("start injected append worker"),
            )
        });
        let error = match result {
            Ok(_) => panic!("injected queue startup failure unexpectedly succeeded"),
            Err(error) => error,
        };
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains("start injected append worker"),
            "{rendered}"
        );
        assert!(rendered.contains("spawn-failure-sentinel"), "{rendered}");
    }
}
