//! Per-session actor commands — one variant per I/O effect.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::markdown_trajectory::BlockHeader;
use crate::session_storage::CaptureRoute;

use super::super::types::CaptureInvocation;

/// Routing identity shared by every session command (`storage` lives on [`SessionActorDeps`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionScope {
    pub route: CaptureRoute,
    pub agent_id: String,
}

impl SessionScope {
    pub fn from_invocation(inv: &CaptureInvocation) -> Self {
        Self {
            route: inv.route.clone(),
            agent_id: inv.agent_id.clone(),
        }
    }
}

/// Commands handled by [`super::super::actors::session::CaptureSessionActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum SessionCommand {
    /// Append one record to Lance/sink and sync live markdown.
    PersistRecord {
        scope: SessionScope,
        /// JSON-encoded [`crate::record::CaptureRecord`].
        record_json: String,
    },
    /// Upsert streaming assistant draft in live markdown only (no sink append).
    UpsertDraft {
        scope: SessionScope,
        draft_json: String,
    },
    /// Drain mailbox before shutdown (no I/O).
    Flush,
}

impl SessionCommand {
    pub fn persist_record(scope: SessionScope, record_json: String) -> Self {
        Self::PersistRecord { scope, record_json }
    }

    pub fn upsert_draft(
        scope: SessionScope,
        call_id: impl Into<String>,
        header: BlockHeader,
        body: Vec<u8>,
    ) -> Result<Self> {
        let draft = DraftPayload {
            call_id: call_id.into(),
            type_name: header.type_name,
            length: header.length,
            fields: header.fields,
            body,
        };
        Ok(Self::UpsertDraft {
            scope,
            draft_json: serde_json::to_string(&draft)?,
        })
    }

    pub fn scope(&self) -> &SessionScope {
        match self {
            Self::PersistRecord { scope, .. } | Self::UpsertDraft { scope, .. } => scope,
            Self::Flush => panic!("Flush has no scope"),
        }
    }
}

/// JSON payload for draft upsert (avoids bincode + nested `serde_json::Value`).
#[derive(Serialize, Deserialize)]
pub(crate) struct DraftPayload {
    pub call_id: String,
    pub type_name: String,
    pub length: usize,
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
    pub body: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_call::CaptureCall;
    use crate::config::CaptureLevel;
    use crate::sink::llm_request_summary_record;

    #[test]
    fn session_command_bincode_roundtrip() {
        let scope = SessionScope {
            route: CaptureRoute {
                root_session: Some("run".into()),
                session_id: "s".into(),
                storage_session_id: "s".into(),
                subagent_id: None,
            },
            agent_id: "agent".into(),
        };
        let call = CaptureCall {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let rec = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "model",
            "/v1/chat/completions",
            12,
            "chat_completions",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let cmd = SessionCommand::persist_record(scope, serde_json::to_string(&rec).unwrap());
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: SessionCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, SessionCommand::PersistRecord { .. }));
    }
}
