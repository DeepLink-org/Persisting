//! Per-story actor commands — one variant per I/O effect.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::super::story::{Story, StoryContext};
use super::CaptureAck;
use crate::markdown_trajectory::BlockHeader;

/// Routing identity for every story command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StoryScope {
    pub context: StoryContext,
}

impl StoryScope {
    pub fn from_context(ctx: &super::super::CallContext) -> Self {
        Self {
            context: ctx.story.clone(),
        }
    }

    pub fn route(&self) -> &crate::session_storage::CaptureRoute {
        &self.context.route
    }

    pub fn agent_id(&self) -> &str {
        &self.context.agent_id
    }
}

/// Commands handled by [`super::super::actors::StoryActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoryCommand {
    /// Append one record to Lance/sink and sync live markdown.
    PersistRecord {
        scope: StoryScope,
        /// JSON-encoded [`crate::record::CaptureRecord`].
        record_json: String,
    },
    /// Upsert streaming assistant draft in live markdown only (no sink append).
    UpsertDraft {
        scope: StoryScope,
        draft_json: String,
    },
    /// Drain mailbox before shutdown (no I/O).
    Flush,
    /// Read-model snapshot (no I/O).
    Snapshot { scope: StoryScope },
    /// Read-model snapshot without scope (shutdown / persist).
    LocalSnapshot,
}

/// Reply from [`super::super::actors::StoryActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum StoryReply {
    Ack(CaptureAck),
    Snapshot { story: Story },
    LocalSnapshot {
        storage_session_id: String,
        story: Story,
    },
}

impl StoryCommand {
    pub fn persist_record(scope: StoryScope, record_json: String) -> Self {
        Self::PersistRecord { scope, record_json }
    }

    pub fn upsert_draft(
        scope: StoryScope,
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

    pub fn scope(&self) -> &StoryScope {
        match self {
            Self::PersistRecord { scope, .. }
            | Self::UpsertDraft { scope, .. }
            | Self::Snapshot { scope } => scope,
            Self::Flush | Self::LocalSnapshot => panic!("command has no scope"),
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
    use crate::config::CaptureLevel;
    use crate::session_storage::CaptureRoute;
    use crate::sink::llm_request_summary_record;
    use crate::Call;

    #[test]
    fn story_command_bincode_roundtrip() {
        let scope = StoryScope {
            context: StoryContext::from_route(
                CaptureRoute {
                    root_session: Some("run".into()),
                    session_id: "s".into(),
                    storage_session_id: "s".into(),
                    subagent_id: None,
                },
                "agent",
            ),
        };
        let call = Call {
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
        let cmd = StoryCommand::persist_record(scope, serde_json::to_string(&rec).unwrap());
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: StoryCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, StoryCommand::PersistRecord { .. }));
    }
}
