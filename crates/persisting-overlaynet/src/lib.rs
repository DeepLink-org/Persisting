//! Network interception and egress policy drivers for pVisor.
//!
//! Host/container execution uses the cooperative HTTP proxy. libkrun VM
//! execution uses a non-bypassable virtio-net/smoltcp IPv4 TCP and DNS data
//! plane; unsupported protocols fail closed.

mod bandwidth;
mod egress;
pub mod forward;
pub mod headers;
pub mod interception;
pub mod policy;
mod resolver;
pub mod server;
pub mod vm;

pub use bandwidth::BandwidthRegistry;
pub use egress::{EgressContext, EgressRuntime};

pub use interception::{
    InterceptionDriver, InterceptionMetrics, InterceptionProfile, InterceptionSnapshot,
    InterceptionStrength,
};
pub use policy::{
    NetworkAccessRule, NetworkBandwidthLimit, NetworkConfig, NetworkMode, NetworkPolicy,
};
pub use server::{OverlayRequestContext, OverlayServerState, OverlaySink};
