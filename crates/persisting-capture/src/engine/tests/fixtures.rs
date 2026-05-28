use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::config::CaptureLevel;
use crate::engine::{CallContext, CaptureEngine, CompleteEvent, DraftEvent, Event, RequestEvent};
use crate::markdown_trajectory::session_markdown_write_path_for_key;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::record::CaptureRecord;
use crate::session_index::SessionIndexStore;
use crate::session_storage::CaptureRoute;
use crate::sink::CaptureSink;
use crate::Call;

pub(crate) struct RecordingSink {
    records: Mutex<Vec<CaptureRecord>>,
    next_seq: Mutex<HashMap<String, u64>>,
}

impl RecordingSink {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(Vec::new()),
            next_seq: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn drain(&self) -> Vec<CaptureRecord> {
        self.records.lock().unwrap().drain(..).collect()
    }
}

impl CaptureSink for RecordingSink {
    fn append(
        &self,
        route: &CaptureRoute,
        _agent_id: &str,
        record: &mut CaptureRecord,
    ) -> anyhow::Result<()> {
        let mut guard = self.next_seq.lock().unwrap();
        let seq = guard.entry(route.seq_key()).or_insert(0);
        record.seq = *seq;
        *seq += 1;
        drop(guard);
        self.records.lock().unwrap().push(record.clone());
        Ok(())
    }

    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        Some(
            self.next_seq
                .lock()
                .unwrap()
                .get(&route.seq_key())
                .copied()
                .unwrap_or(0),
        )
    }
}

pub(crate) fn test_context() -> CallContext {
    CallContext::new(
        CaptureRoute {
            root_session: Some("run-test".into()),
            session_id: "sess-1".into(),
            storage_session_id: "sess-1".into(),
            subagent_id: None,
        },
        "agent-1",
        Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        },
        Vec::new(),
        CaptureLevel::Dialogue,
        "deepseek-chat",
        "deepseek-chat",
        ProviderKind::OpenAi,
        ProtocolKind::ChatCompletions,
        false,
    )
}

pub(crate) async fn test_engine(
    sink: Arc<RecordingSink>,
    storage: &std::path::Path,
    stream_markdown: bool,
) -> CaptureEngine {
    let index = SessionIndexStore::open(storage).unwrap().clone_handle();
    CaptureEngine::new(
        sink,
        index,
        Arc::new(storage.to_path_buf()),
        stream_markdown,
    )
    .await
    .unwrap()
}

pub(crate) async fn flush_engine(engine: &CaptureEngine) {
    engine.flush().await.unwrap();
}
