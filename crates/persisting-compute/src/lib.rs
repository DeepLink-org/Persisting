//! Persisting compute control plane (library; CLI entry is `persisting compute`).
//!
//! # Semantic primitives
//!
//! Contracts are listed in [`blocks`]. Unit / interface tests live in the same
//! source file as each primitive; `tests/` holds multi-module integration only.
//!
//! # Design sketch
//!
//! - **One CLI** = `persisting compute` (thin orchestration, not Ray)
//! - **Driver** owns plan emit + dispatch (least-loaded asks workers directly)
//! - **Launch with torchrun** for multi-process; local `-w N` for single-process
//! - **Plan script** (user Python): `plan()` + `execute(item)`; argv after `--`
//!
//! ```text
//! torchrun --nproc_per_node=4 -- persisting compute plan.py -- --n 2
//!   rank0: Driver (plan + dispatch) + worker slots
//!   rankN: worker slots
//! ```
//!
//! Most modules are `pub(crate)`; only surfaces needed by the CLI binary and
//! integration tests are re-exported or left as `pub mod`.

pub(crate) mod blocks;
pub(crate) mod check;
pub(crate) mod checkpoint;
pub mod cli;
pub mod dist;
pub mod driver;
pub(crate) mod executor;
pub(crate) mod future;
pub mod job_control;
pub mod observe;
pub(crate) mod plan;
pub mod pulsing_ext;
pub(crate) mod python_env;
pub(crate) mod result_cache;
pub mod runtime;
pub(crate) mod scheduler;
pub(crate) mod sink;
#[cfg(feature = "traj-sink")]
pub(crate) mod sink_traj;
pub(crate) mod sink_writer;
pub(crate) mod skip;
pub mod task;
pub(crate) mod worker;

// ── Public surface (CLI + integration tests) ─────────────────────────

pub use check::{run_check, run_self_test, CheckOptions, CheckReport};
pub use checkpoint::CheckpointLedger;
pub use cli::{init_tracing, init_tracing_with_verbose, run_compute, ComputeArgs, ResultsFormat};
pub use dist::DistEnv;
pub use driver::{Driver, RunOptions};
pub use observe::{Observer, ObserverOptions};
pub use runtime::{run_fleet, run_local_fleet};
pub use skip::SkipSet;
pub use task::{TaskExpr, TaskResult};
pub use worker::{WorkerActor, WorkerCommand, WorkerReply};

// Sink types used by CLI orchestration (re-exported so `cli` stays thin).
pub use sink::{JsonlFileSink, ResultSink, TeeSink};
#[cfg(feature = "traj-sink")]
pub use sink_traj::VortexResultSink;
pub use sink_writer::{spawn_sink_writer, SinkSubmitter, SinkWriterHandle};
