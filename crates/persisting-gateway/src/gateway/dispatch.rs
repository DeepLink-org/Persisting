//! Gateway sink adapter for the OverlayNet proxy server.

use async_trait::async_trait;
use axum::extract::Request;
use axum::response::Response;
use axum::Router;
use bytes::Bytes;
use persisting_overlaynet::policy::{DenyReason, NetworkPolicy};
use persisting_overlaynet::server::{OverlayRequestContext, OverlayServerState, OverlaySink};

use super::common::effective_config;
use super::llm_capture::llm_capture;
use super::state::GatewayState;
use crate::config::ProxyConfig;
use crate::runtime::debug::{self, is_debug_enabled};
use crate::session::storage::resolve_capture_route;

#[derive(Clone)]
pub(crate) struct GatewayRequestContext {
    config: std::sync::Arc<ProxyConfig>,
    debug_on: bool,
}

pub(crate) fn build_router(state: GatewayState) -> Router {
    let server = OverlayServerState::new(
        state.client.clone(),
        state.control_controller.clone(),
        state.clone(),
        state.active_requests.clone(),
    );
    persisting_overlaynet::server::build_router(server)
}

#[async_trait]
impl OverlaySink for GatewayState {
    type RequestContext = GatewayRequestContext;

    fn request_context(
        &self,
        request: &Request,
    ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
        let route = resolve_capture_route(
            request.headers(),
            &Bytes::new(),
            &self.config.session_header,
            self.storage.as_path(),
        );
        let config = effective_config(self, &route);
        let debug_on = is_debug_enabled(&config, self.storage.as_path());
        Ok(OverlayRequestContext {
            policy: NetworkPolicy::from_config(config.as_ref())?,
            run_id: route.root_session,
            storyline_id: Some(route.session_id.clone()),
            session_id: route.session_id,
            sink: GatewayRequestContext { config, debug_on },
        })
    }

    async fn handle(
        &self,
        request: Request,
        peer: std::net::SocketAddr,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Response> {
        llm_capture(
            self.clone(),
            request,
            peer,
            std::sync::Arc::clone(&context.sink.config),
            context.sink.debug_on,
        )
        .await
    }

    fn accepts(&self, request: &Request) -> bool {
        crate::protocol::ProtocolKind::from_path(request.uri().path())
            != crate::protocol::ProtocolKind::Unknown
    }

    fn on_denied(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        host: &str,
        reason: &DenyReason,
    ) {
        if context.sink.debug_on {
            debug::log_network_denied(
                self.storage.as_path(),
                host,
                context.policy.mode_str(),
                reason.as_str(),
                &context.session_id,
            );
        }
    }

    fn on_dispatch(
        &self,
        context: &OverlayRequestContext<Self::RequestContext>,
        request: &Request,
        target: &str,
    ) {
        if context.sink.debug_on {
            debug::log_dispatch(
                self.storage.as_path(),
                request.method().as_str(),
                &request.uri().to_string(),
                &context.session_id,
                target,
            );
        }
    }
}
