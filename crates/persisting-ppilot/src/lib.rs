//! pPilot — Durable Run Orchestrator.
//!
//! # Semantic primitives
//!
//! Contracts are listed in [`blocks`]. Unit / interface tests live in the same
//! source file as each primitive; `tests/` holds multi-module integration only.
//!
//! # Design sketch
//!
//! The driver owns plan emission and dispatch. A Python workload supplies
//! `plan()` and `execute(item)`; an embedding host chooses local workers or a
//! torchrun-created multi-process environment. The standalone `ppilot` command
//! exposes only scalable Run production.
//!
//! Most modules are `pub(crate)`; only embedding and integration-test surfaces
//! are re-exported or left as `pub mod`.

pub mod agent_abi;
pub mod batch;
pub mod blocks;
pub(crate) mod check;
pub(crate) mod checkpoint;
pub mod cli;
pub mod coordination;
pub(crate) mod digest;
pub mod dist;
pub mod driver;
pub(crate) mod executor;
pub mod future;
pub mod job_control;
pub mod observe;
pub(crate) mod plan;
pub mod pulsing_ext;
pub(crate) mod python_env;
pub(crate) mod result_cache;
pub mod runtime;
pub mod runtime_bridge;
pub(crate) mod scheduler;
pub(crate) mod sink;
#[cfg(feature = "traj-sink")]
pub(crate) mod sink_traj;
pub(crate) mod sink_writer;
pub(crate) mod skip;
pub mod supervisor;
pub mod task;
pub(crate) mod worker;

// ── Public surface (embedding + integration tests) ──────────────────

pub use agent_abi::{checkpoint_directive, AgentAbiClient, AgentAbiClientConfig};
pub use batch::{
    produce_from_planner, produce_trajectories, BatchProductionManifest, BatchProductionOptions,
    BatchProductionReport, TrajectoryProductionRun,
};
pub use check::{run_check, CheckOptions, CheckReport};
pub use checkpoint::CheckpointLedger;
pub use cli::{init_tracing, init_tracing_with_verbose, run_ppilot, PPilotArgs, ResultsFormat};
pub use coordination::{
    AttemptObservation, AttemptObserver, DurableAttemptObserver, ProcessLocalAttemptObserver,
    ReconcileReport, RunCoordinator,
};
pub use dist::DistEnv;
pub use driver::{Driver, RunOptions};
pub use observe::{Observer, ObserverOptions};
pub use runtime::{run_fleet, run_local_fleet};
pub use runtime_bridge::PilotRuntimeBridge;
pub use skip::SkipSet;
pub use supervisor::{
    parse_bandwidth, EmbeddedSupervisor, EmbeddedSupervisorConfig, SupervisorRegistrationSnapshot,
};
pub use task::{TaskExpr, TaskResult};
pub use worker::{WorkerActor, WorkerCommand, WorkerReply};

// Sink types used by orchestration hosts.
pub use sink::{JsonlFileSink, ResultSink, TeeSink};
#[cfg(feature = "traj-sink")]
pub use sink_traj::LanceResultSink;
pub use sink_writer::{
    spawn_coordinated_sink_writer, spawn_sink_writer, SinkSubmitter, SinkWriterHandle,
};
