//! Agent-side injection: env vars for child processes and peer process detection.
//!
//! | Module | Role |
//! |--------|------|
//! | [`mod@env`] | `HTTP_PROXY`, SDK base URLs, session id for `capture run` children |
//! | [`peer`] | lsof/ps lookup of the TCP peer behind proxied connections |
//! | [`host_identity`] | one-way `machine_fp` from hostname + identity IP |

pub mod env;
pub mod host_identity;
pub mod peer;

pub use env::{
    capture_openai_v1_base, client_gateway_config_args, proxy_environment,
    proxy_environment_with_local_auth, CAPTURE_PROXY_ENV_KEYS,
};
