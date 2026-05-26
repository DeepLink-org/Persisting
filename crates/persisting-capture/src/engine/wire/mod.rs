//! Actor wire protocol — all cross-actor messages live here.
//!
//! Domain types ([`super::types`]) never cross the mailbox; only wire commands do.
//! JSON-in-bincode for payloads that contain [`serde_json::Value`].

mod headers;
mod registry;
mod session;

pub(crate) use headers::headers_to_header_map;
pub(crate) use registry::{
    registry_enrich, registry_main_route, RegistryCommand, RegistryReply, REGISTRY_ACTOR_NAME,
};
pub(crate) use session::{DraftPayload, SessionCommand, SessionScope};

/// Unified ask/tell acknowledgement for session actors.
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
