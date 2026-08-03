//! Lightweight proxy-based network overlay for pVisor.
//!
//! The current backend is deliberately limited to explicit HTTP/HTTPS proxy
//! traffic. It does not claim transparent socket, DNS, UDP, TUN, or network
//! namespace isolation.

mod bandwidth;
pub mod forward;
pub mod headers;
pub mod interception;
pub mod policy;
mod resolver;
pub mod server;

pub use interception::{
    InterceptionDriver, InterceptionMetrics, InterceptionProfile, InterceptionSnapshot,
    InterceptionStrength,
};
pub use policy::{
    NetworkAccessRule, NetworkBandwidthLimit, NetworkConfig, NetworkMode, NetworkPolicy,
};
pub use server::{OverlayRequestContext, OverlayServerState, OverlaySink};
