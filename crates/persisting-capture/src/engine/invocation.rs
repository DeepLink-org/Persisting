use crate::capture_call::CaptureCall;
use crate::config::CaptureLevel;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::session_storage::CaptureRoute;

#[derive(Clone)]
pub struct CaptureInvocation {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub call: CaptureCall,
    pub request_headers: Vec<(String, String)>,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: ProviderKind,
    pub protocol: ProtocolKind,
    pub debug_on: bool,
}
