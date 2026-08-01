//! Generic explicit-proxy server and request dispatch.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use persisting_control::ControlController;
use persisting_proto::{NetworkAccessRequest, NetworkTransport, RunId, StorylineId};

use crate::forward::{
    handle_connect_authorized, is_forward_proxy_request, transparent_forward_authorized,
};
use crate::policy::{authorize_egress, forbidden_response, DenyReason, NetworkPolicy};
#[derive(Clone)]
pub struct OverlayRequestContext<T> {
    pub policy: NetworkPolicy,
    pub run_id: Option<String>,
    pub storyline_id: Option<String>,
    pub session_id: String,
    pub sink: T,
}

/// Application sink attached to the generic OverlayNet proxy data plane.
///
/// A sink decides which absolute-URI requests it consumes. Requests it does
/// not accept remain transparent proxy traffic. Relative gateway requests are
/// always delivered to the configured sink. OverlayNet is independent of any
/// concrete sink; an implementation may also compose several downstream sinks.
#[async_trait]
pub trait OverlaySink: Clone + Send + Sync + 'static {
    type RequestContext: Clone + Send + Sync + 'static;

    fn request_context(
        &self,
        request: &Request,
    ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>>;

    async fn handle(
        &self,
        request: Request,
        peer: SocketAddr,
        context: &OverlayRequestContext<Self::RequestContext>,
    ) -> anyhow::Result<Response>;

    fn accepts(&self, _request: &Request) -> bool {
        false
    }

    fn on_denied(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _host: &str,
        _reason: &DenyReason,
    ) {
    }

    fn on_dispatch(
        &self,
        _context: &OverlayRequestContext<Self::RequestContext>,
        _request: &Request,
        _target: &str,
    ) {
    }
}

#[derive(Clone)]
pub struct OverlayServerState<S> {
    client: reqwest::Client,
    control_controller: Arc<dyn ControlController>,
    sink: S,
    active_requests: Arc<AtomicUsize>,
}

impl<S> OverlayServerState<S>
where
    S: OverlaySink,
{
    pub fn new(
        client: reqwest::Client,
        control_controller: Arc<dyn ControlController>,
        sink: S,
        active_requests: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            client,
            control_controller,
            sink,
            active_requests,
        }
    }
}

pub fn build_router<S>(state: OverlayServerState<S>) -> Router
where
    S: OverlaySink,
{
    Router::new()
        .fallback(any(proxy_handler::<S>))
        .with_state(state)
}

async fn proxy_handler<S>(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<OverlayServerState<S>>,
    request: Request,
) -> Response
where
    S: OverlaySink,
{
    let _active_request = ActiveRequestGuard::new(Arc::clone(&state.active_requests));
    let response = dispatch(&state, request, peer).await;
    match response {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!("overlaynet proxy error: {error:#}");
            (
                StatusCode::BAD_GATEWAY,
                format!("persisting-overlaynet: {error:#}"),
            )
                .into_response()
        }
    }
}

async fn dispatch<S>(
    state: &OverlayServerState<S>,
    request: Request,
    peer: SocketAddr,
) -> anyhow::Result<Response>
where
    S: OverlaySink,
{
    let context = state.sink.request_context(&request)?;

    if request.method() == Method::CONNECT {
        let authority = request
            .uri()
            .authority()
            .map(|authority| authority.to_string())
            .unwrap_or_default();
        let host = crate::policy::host_from_authority(&authority);
        if let Err(reason) = authorize(
            state,
            &context,
            &host,
            request.uri().port_u16(),
            NetworkTransport::TcpTunnel,
        ) {
            state.sink.on_denied(&context, &host, &reason);
            let (status, message) = forbidden_response(&host, &reason);
            return Ok((status, message).into_response());
        }
        state.sink.on_dispatch(&context, &request, "connect");
        return Ok(handle_connect_authorized(request).await);
    }

    if is_forward_proxy_request(request.method(), request.uri()) {
        let host = request.uri().host().map(str::to_string).unwrap_or_default();
        let transport = if request.uri().scheme_str() == Some("https") {
            NetworkTransport::Https
        } else {
            NetworkTransport::Http
        };
        if let Err(reason) = authorize(state, &context, &host, request.uri().port_u16(), transport)
        {
            state.sink.on_denied(&context, &host, &reason);
            let (status, message) = forbidden_response(&host, &reason);
            return Ok((status, message).into_response());
        }
        if state.sink.accepts(&request) {
            state.sink.on_dispatch(&context, &request, "sink");
            return state.sink.handle(request, peer, &context).await;
        }
        state.sink.on_dispatch(&context, &request, "forward");
        return transparent_forward_authorized(&state.client, request)
            .await
            .map(IntoResponse::into_response);
    }

    state.sink.on_dispatch(&context, &request, "sink");
    state.sink.handle(request, peer, &context).await
}

fn authorize<S>(
    state: &OverlayServerState<S>,
    context: &OverlayRequestContext<S::RequestContext>,
    host: &str,
    port: Option<u16>,
    transport: NetworkTransport,
) -> Result<(), DenyReason>
where
    S: OverlaySink,
{
    authorize_egress(
        state.control_controller.as_ref(),
        &context.policy,
        &NetworkAccessRequest {
            run_id: context.run_id.clone().map(RunId),
            attempt_id: None,
            storyline_id: context.storyline_id.clone().map(StorylineId),
            host: host.to_string(),
            port,
            transport,
        },
    )
}

struct ActiveRequestGuard {
    counter: Arc<AtomicUsize>,
}

impl ActiveRequestGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct EventSink;

    #[async_trait]
    impl OverlaySink for EventSink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            unreachable!("selection does not require a request context")
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!("selection does not dispatch the request")
        }

        fn accepts(&self, request: &Request) -> bool {
            request.uri().path().starts_with("/events/")
        }
    }

    #[test]
    fn active_request_guard_decrements_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let _guard = ActiveRequestGuard::new(Arc::clone(&counter));
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn overlaynet_accepts_non_gateway_sink_implementations() {
        let accepted = Request::builder()
            .uri("http://collector.example/events/agent")
            .body(axum::body::Body::empty())
            .expect("valid request");
        let forwarded = Request::builder()
            .uri("http://collector.example/health")
            .body(axum::body::Body::empty())
            .expect("valid request");

        assert!(EventSink.accepts(&accepted));
        assert!(!EventSink.accepts(&forwarded));
    }
}
