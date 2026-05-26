//! Agent-side injection: env vars for child processes and peer process detection.
//!
//! | Module | Role |
//! |--------|------|
//! | [`env`] | `HTTP_PROXY`, SDK base URLs, session id for `capture run` children |
//! | [`peer`] | lsof/ps lookup of the TCP peer behind proxied connections |

pub mod env;
pub mod peer;

pub use env::{
    capture_openai_v1_base, client_gateway_config_args, proxy_environment, CAPTURE_PROXY_ENV_KEYS,
};
