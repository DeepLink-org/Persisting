//! Gateway daemon lifecycle, run directories, and debug sidecars.

pub mod debug;
pub mod discover;
pub mod in_process;
mod private_fs;
pub mod run_config;
pub mod run_env;
pub mod service;
