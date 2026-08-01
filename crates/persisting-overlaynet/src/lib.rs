//! Lightweight proxy-based network overlay for pVisor.
//!
//! The current backend is deliberately limited to explicit HTTP/HTTPS proxy
//! traffic. It does not claim transparent socket, DNS, UDP, TUN, or network
//! namespace isolation.

pub mod forward;
pub mod headers;
pub mod policy;
pub mod server;

pub use policy::{NetworkConfig, NetworkMode, NetworkPolicy};
pub use server::{OverlayRequestContext, OverlayServerState, OverlaySink};
