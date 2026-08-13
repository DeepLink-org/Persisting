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
use persisting_control::{NetworkAccessRequest, NetworkTransport, RunId, StorylineId};

use crate::bandwidth::{throttle_body, BandwidthRegistry, BandwidthSession};
use crate::forward::{
    handle_connect_authorized, is_forward_proxy_request, parse_connect_target,
    transparent_forward_authorized,
};
use crate::interception::InterceptionMetrics;
use crate::policy::{forbidden_response, DenyReason, NetworkPolicy};
use crate::resolver::{authorize_target, AuthorizedTarget, TargetAuthorizationError};
#[derive(Clone)]
pub struct OverlayRequestContext<T> {
    pub policy: NetworkPolicy,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
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
    control_controller: Arc<dyn ControlController>,
    sink: S,
    active_requests: Arc<AtomicUsize>,
    interception_metrics: InterceptionMetrics,
    bandwidth_registry: BandwidthRegistry,
}

impl<S> OverlayServerState<S>
where
    S: OverlaySink,
{
    pub fn new(
        control_controller: Arc<dyn ControlController>,
        sink: S,
        active_requests: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            control_controller,
            sink,
            active_requests,
            interception_metrics: InterceptionMetrics::default(),
            bandwidth_registry: BandwidthRegistry::default(),
        }
    }

    pub fn with_interception_metrics(mut self, metrics: InterceptionMetrics) -> Self {
        self.interception_metrics = metrics;
        self
    }

    pub fn with_bandwidth_registry(mut self, registry: BandwidthRegistry) -> Self {
        self.bandwidth_registry = registry;
        self
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
    state.interception_metrics.request_seen();
    let response = dispatch(&state, request, peer).await;
    match response {
        Ok(response) => response,
        Err(error) => {
            state.interception_metrics.failure();
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
        state.interception_metrics.connect_request();
        let authority = request
            .uri()
            .authority()
            .map(|authority| authority.to_string())
            .unwrap_or_default();
        let target = match parse_connect_target(&authority) {
            Ok(target) => target,
            Err(error) => {
                state.interception_metrics.failure();
                return Ok((
                    StatusCode::BAD_REQUEST,
                    format!("persisting-overlaynet: {error}"),
                )
                    .into_response());
            }
        };
        let host = crate::policy::host_from_authority(&target.host);
        let authorized = match authorize(
            state,
            &context,
            &host,
            Some(target.port),
            NetworkTransport::TcpTunnel,
        )
        .await
        {
            Ok(target) => target,
            Err(TargetAuthorizationError::Denied(reason)) => {
                state.interception_metrics.policy_denied();
                state.sink.on_denied(&context, &host, &reason);
                let (status, message) = forbidden_response(&host, &reason);
                return Ok((status, message).into_response());
            }
            Err(TargetAuthorizationError::Resolve(error)) => return Err(error),
        };
        state.interception_metrics.policy_allowed();
        state.sink.on_dispatch(&context, &request, "connect");
        let bandwidth =
            bandwidth_session(state, &context, &host, Some(target.port), Some(&authorized)).await;
        return handle_connect_authorized(request, target, &authorized, bandwidth).await;
    }

    if is_forward_proxy_request(request.method(), request.uri()) {
        state.interception_metrics.absolute_http_request();
        let host = request.uri().host().map(str::to_string).unwrap_or_default();
        let transport = if request.uri().scheme_str() == Some("https") {
            NetworkTransport::Https
        } else {
            NetworkTransport::Http
        };
        let port = request.uri().port_u16().or(match transport {
            NetworkTransport::Https => Some(443),
            NetworkTransport::Http => Some(80),
            NetworkTransport::TcpTunnel => None,
        });
        let authorized = match authorize(state, &context, &host, port, transport).await {
            Ok(target) => target,
            Err(TargetAuthorizationError::Denied(reason)) => {
                state.interception_metrics.policy_denied();
                state.sink.on_denied(&context, &host, &reason);
                let (status, message) = forbidden_response(&host, &reason);
                return Ok((status, message).into_response());
            }
            Err(TargetAuthorizationError::Resolve(error)) => return Err(error),
        };
        state.interception_metrics.policy_allowed();
        let bandwidth = bandwidth_session(state, &context, &host, port, Some(&authorized)).await;
        if state.sink.accepts(&request) {
            state.interception_metrics.sink_request();
            state.sink.on_dispatch(&context, &request, "sink");
            let request = throttle_request(request, bandwidth.clone());
            let response = state.sink.handle(request, peer, &context).await?;
            return Ok(throttle_response(response, bandwidth));
        }
        state.sink.on_dispatch(&context, &request, "forward");
        return transparent_forward_authorized(request, &authorized, bandwidth)
            .await
            .map(IntoResponse::into_response);
    }

    state.interception_metrics.sink_request();
    state.sink.on_dispatch(&context, &request, "sink");
    let bandwidth = bandwidth_session(state, &context, "", None, None).await;
    let request = throttle_request(request, bandwidth.clone());
    let response = state.sink.handle(request, peer, &context).await?;
    Ok(throttle_response(response, bandwidth))
}

async fn bandwidth_session<S>(
    state: &OverlayServerState<S>,
    context: &OverlayRequestContext<S::RequestContext>,
    host: &str,
    port: Option<u16>,
    authorized: Option<&AuthorizedTarget>,
) -> BandwidthSession
where
    S: OverlaySink,
{
    state
        .bandwidth_registry
        .session(context.policy.matching_limits(
            host,
            port,
            authorized.map_or(&[], |target| target.addresses.as_slice()),
        ))
        .await
}

fn throttle_request(request: Request, bandwidth: BandwidthSession) -> Request {
    let (parts, body) = request.into_parts();
    Request::from_parts(parts, throttle_body(body, bandwidth))
}

fn throttle_response(response: Response, bandwidth: BandwidthSession) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, throttle_body(body, bandwidth))
}

async fn authorize<S>(
    state: &OverlayServerState<S>,
    context: &OverlayRequestContext<S::RequestContext>,
    host: &str,
    port: Option<u16>,
    transport: NetworkTransport,
) -> Result<crate::resolver::AuthorizedTarget, TargetAuthorizationError>
where
    S: OverlaySink,
{
    authorize_target(
        state.control_controller.as_ref(),
        &context.policy,
        NetworkAccessRequest {
            run_id: context.run_id.clone().map(RunId),
            attempt_id: context
                .attempt_id
                .clone()
                .map(persisting_control::AttemptId),
            storyline_id: context.storyline_id.clone().map(StorylineId),
            host: host.to_string(),
            port,
            transport,
            resolved_ip: None,
        },
    )
    .await
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
    use crate::policy::{NetworkConfig, NetworkMode};
    use persisting_control::PolicyControlController;

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

    #[derive(Clone)]
    struct PolicySink;

    #[async_trait]
    impl OverlaySink for PolicySink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            Ok(OverlayRequestContext {
                policy: NetworkPolicy::compile(&NetworkConfig {
                    mode: NetworkMode::NoNetwork,
                    allowed_hosts: Vec::new(),
                    rules: Vec::new(),
                    deny_rules: Vec::new(),
                    limits: Vec::new(),
                })?,
                run_id: Some("run-1".into()),
                attempt_id: Some("attempt-1".into()),
                storyline_id: None,
                session_id: "session-1".into(),
                sink: (),
            })
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!("denied requests do not reach the sink")
        }
    }

    #[test]
    fn denied_proxy_requests_update_interception_metrics() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .method(Method::CONNECT)
                .uri("example.com:443")
                .body(axum::body::Body::empty())
                .unwrap();

            metrics.request_seen();
            let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.requests_seen, 1);
            assert_eq!(snapshot.connect_requests, 1);
            assert_eq!(snapshot.policy_denied, 1);
            assert_eq!(snapshot.policy_allowed, 0);
        });
    }

    #[derive(Clone)]
    struct FailingContextSink;

    #[async_trait]
    impl OverlaySink for FailingContextSink {
        type RequestContext = ();

        fn request_context(
            &self,
            _request: &Request,
        ) -> anyhow::Result<OverlayRequestContext<Self::RequestContext>> {
            anyhow::bail!("synthetic context failure")
        }

        async fn handle(
            &self,
            _request: Request,
            _peer: SocketAddr,
            _context: &OverlayRequestContext<Self::RequestContext>,
        ) -> anyhow::Result<Response> {
            unreachable!()
        }
    }

    #[test]
    fn handler_turns_internal_failures_into_502_and_releases_active_count() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let active = Arc::new(AtomicUsize::new(0));
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                FailingContextSink,
                Arc::clone(&active),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .uri("/events")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = proxy_handler(
                ConnectInfo("127.0.0.1:40000".parse().unwrap()),
                State(state),
                request,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(active.load(Ordering::Relaxed), 0);
            assert_eq!(metrics.snapshot().requests_seen, 1);
            assert_eq!(metrics.snapshot().failures, 1);
        });
    }

    #[test]
    fn malformed_connect_is_400_and_never_reaches_policy() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metrics = InterceptionMetrics::default();
            let state = OverlayServerState::new(
                Arc::new(PolicyControlController),
                PolicySink,
                Arc::new(AtomicUsize::new(0)),
            )
            .with_interception_metrics(metrics.clone());
            let request = Request::builder()
                .method(Method::CONNECT)
                .uri("example.com:65536")
                .body(axum::body::Body::empty())
                .unwrap();
            let response = dispatch(&state, request, "127.0.0.1:40000".parse().unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.connect_requests, 1);
            assert_eq!(snapshot.failures, 1);
            assert_eq!(snapshot.policy_allowed, 0);
            assert_eq!(snapshot.policy_denied, 0);
        });
    }
}
