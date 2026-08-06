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
//! torchrun-created multi-process environment. pPilot currently has no public
//! top-level Persisting command.
//!
//! Most modules are `pub(crate)`; only embedding and integration-test surfaces
//! are re-exported or left as `pub mod`.

pub mod agent_abi;
pub mod batch;
pub mod blocks;
pub(crate) mod check;
pub(crate) mod checkpoint;
#[cfg(feature = "query")]
pub mod chronicle_cli;
pub mod cli;
pub mod coordination;
pub mod dist;
pub mod driver;
pub(crate) mod executor;
#[cfg(feature = "query")]
pub mod federated;
pub mod future;
pub mod job_control;
pub mod observe;
pub(crate) mod plan;
pub mod pulsing_ext;
pub(crate) mod python_env;
#[cfg(feature = "query")]
pub mod query_cli;
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
#[cfg(feature = "query")]
pub use batch::{
    balanced_shards, process_trajectories, render_analysis_rows, AnalysisOutputFormat,
    BatchAnalysisOptions, BatchAnalysisReport,
};
pub use batch::{
    produce_from_planner, produce_trajectories, BatchProductionManifest, BatchProductionOptions,
    BatchProductionReport, TrajectoryProductionRun,
};
pub use check::{run_check, run_self_test, CheckOptions, CheckReport};
pub use checkpoint::CheckpointLedger;
#[cfg(feature = "query")]
pub use chronicle_cli::{
    run_chronicle, ChronicleArgs, ChronicleCommand, ChronicleImportArgs, ChronicleMaintainArgs,
};
pub use cli::{init_tracing, init_tracing_with_verbose, run_ppilot, PPilotArgs, ResultsFormat};
pub use coordination::{
    AttemptObservation, AttemptObserver, DurableAttemptObserver, ProcessLocalAttemptObserver,
    ReconcileReport, RunCoordinator,
};
pub use dist::DistEnv;
pub use driver::{Driver, RunOptions};
#[cfg(feature = "query")]
pub use federated::{
    federated_count_with_workers, process_federated_count, process_script,
    process_script_with_workers, CountTable, FederatedAnalysisCommand, FederatedAnalysisReply,
    FederatedAnalysisWorker, FederatedCountOptions, FederatedCountPartialReport,
    FederatedCountReport, ProcessMapperReport, ProcessScriptOptions, ProcessScriptReport,
};
pub use observe::{Observer, ObserverOptions};
#[cfg(feature = "query")]
pub use query_cli::{
    run_query, BatchQueryArgs, FollowQueryArgs, PointQueryArgs, QueryArgs, QueryCommand,
    QuerySource, SqlQueryArgs,
};
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
