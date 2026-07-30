//! Run/story routing and per-call capture context.

use serde::{Deserialize, Serialize};

use crate::config::CaptureLevel;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::session_storage::CaptureRoute;

use super::call::Call;
use super::ids::{RunId, StoryId};

/// Routing + identity for one story (one agent narrative line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryContext {
    pub run_id: Option<RunId>,
    pub story_id: StoryId,
    pub route: CaptureRoute,
    pub agent_id: String,
}

impl StoryContext {
    pub fn from_route(route: CaptureRoute, agent_id: impl Into<String>) -> Self {
        let story_id = StoryId::from_seq_key(route.seq_key());
        let run_id = route.root_session.clone().map(RunId);
        Self {
            run_id,
            story_id,
            route,
            agent_id: agent_id.into(),
        }
    }

    pub fn story_id(&self) -> &StoryId {
        &self.story_id
    }
}

/// Full capture context for one proxied LLM call (story + call + wire metadata).
#[derive(Clone)]
pub struct CallContext {
    pub story: StoryContext,
    pub call: Call,
    pub request_headers: Vec<(String, String)>,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: ProviderKind,
    pub protocol: ProtocolKind,
    pub debug_on: bool,
    /// Client socket address (`ip:port`) from `ConnectInfo`, when known.
    pub client_peer: Option<String>,
    /// Best-effort process / machine metadata for the peer.
    pub client_meta: Option<crate::session_client::SessionClientMeta>,
    /// HTTP version string, e.g. `HTTP/1.1`.
    pub http_version: Option<String>,
    /// Upstream request URL when known (proxy path).
    pub upstream_url: Option<String>,
}

impl CallContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        route: CaptureRoute,
        agent_id: impl Into<String>,
        call: Call,
        request_headers: Vec<(String, String)>,
        level: CaptureLevel,
        client_model: impl Into<String>,
        upstream_model: impl Into<String>,
        provider: ProviderKind,
        protocol: ProtocolKind,
        debug_on: bool,
    ) -> Self {
        Self {
            story: StoryContext::from_route(route, agent_id),
            call,
            request_headers,
            level,
            client_model: client_model.into(),
            upstream_model: upstream_model.into(),
            provider,
            protocol,
            debug_on,
            client_peer: None,
            client_meta: None,
            http_version: None,
            upstream_url: None,
        }
    }

    pub fn attach_client(
        &mut self,
        peer: impl std::fmt::Display,
        meta: Option<crate::session_client::SessionClientMeta>,
    ) {
        self.client_peer = Some(peer.to_string());
        self.client_meta = meta;
    }

    pub fn attach_http_version(&mut self, version: impl Into<String>) {
        self.http_version = Some(version.into());
    }

    pub fn attach_upstream_url(&mut self, url: impl Into<String>) {
        self.upstream_url = Some(url.into());
    }

    pub fn story_id(&self) -> &StoryId {
        self.story.story_id()
    }

    pub fn run_id(&self) -> Option<&RunId> {
        self.story.run_id.as_ref()
    }

    pub fn route(&self) -> &CaptureRoute {
        &self.story.route
    }

    pub fn agent_id(&self) -> &str {
        &self.story.agent_id
    }
}
