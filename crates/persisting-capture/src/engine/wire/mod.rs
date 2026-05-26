//! Actor wire protocol — all cross-actor messages live here.

mod headers;
mod run;
mod story;

pub(crate) use headers::{headers_to_header_map, headers_to_vec};
pub(crate) use run::{run_enrich, run_main_route, RunCommand, RunReply, RUN_ACTOR_NAME};
pub(crate) use story::{DraftPayload, StoryCommand, StoryReply, StoryScope};

/// Unified ask/tell acknowledgement for story actors.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CaptureAck {
    pub ok: bool,
    pub error: Option<String>,
}

impl CaptureAck {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
        }
    }

    pub fn into_result(self) -> anyhow::Result<()> {
        if self.ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!(self
                .error
                .unwrap_or_else(|| "capture command failed".into())))
        }
    }
}
