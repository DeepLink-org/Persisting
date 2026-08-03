//! Index of **semantic primitives** — one contract per owning module.
//!
//! # Testing policy
//!
//! - **Unit / contract tests** live in the **same file** as the primitive
//!   (`#[cfg(test)]` at the bottom of `task.rs`, `scheduler.rs`, …).
//! - **`tests/`** holds **integration** only (multi-module paths: fleet, resume
//!   through Driver, argv end-to-end, …).
//!
//! | Primitive | Interface (types / traits / fns) | Module |
//! |-----------|----------------------------------|--------|
//! | Task wire | [`TaskExpr`], [`TaskResult`] | [`crate::task`] |
//! | Placement | [`Scheduler`], sticky-only / quarantine | [`crate::scheduler`] |
//! | Slot naming / dist | [`DistEnv`] | [`crate::dist`] |
//! | Run future | [`RunFuture`], [`wait_all`] | [`crate::future`] |
//! | Idempotency cache | [`ResultCache`] | [`crate::result_cache`] |
//! | Live skip / claim | [`SkipSet`] | [`crate::skip`] |
//! | Result sink | [`ResultSink`], [`persist_terminal`] | [`crate::sink`] |
//! | Async sink writer | [`SinkSubmitter`], [`spawn_sink_writer`] | [`crate::sink_writer`] |
//! | Checkpoint | [`CheckpointLedger`], [`CheckpointTracker`] | [`crate::checkpoint`] |
//! | Plan emit | [`stream_plan_tasks`] | [`crate::plan`] |
//! | Execute host | Executor router | `executor` |
//! | Worker seam | [`WorkerActor`], supervised spawn | [`crate::worker`] |
//! | Job cancel | [`JobControlActor`], DeathWatch | [`crate::job_control`] |
//! | Pulsing helpers | resolve / ask_timeout / spawn_supervised | [`crate::pulsing_ext`] |
//! | Driver | [`Driver`], [`RunOptions`] | [`crate::driver`] |
//! | Fleet boot | [`run_local_fleet`], [`run_fleet`] | [`crate::runtime`] |
//! | Observe | [`Observer`] | [`crate::observe`] |
//! | Python env | [`merge_pythonpath_parts`] | [`crate::python_env`] |
//! | Agent ABI | [`AgentAbiClient`], [`AgentAbiClientConfig`] | [`crate::agent_abi`] |
//! | Runtime bridge | [`PilotRuntimeBridge`] | [`crate::runtime_bridge`] |
//! | Batch trajectories | production + sharded analysis | [`crate::batch`] |
//! | Federated analysis | Pulsing partial aggregation + coordinator merge | [`crate::federated`] |
//!
//! [`TaskExpr`]: crate::task::TaskExpr
//! [`TaskResult`]: crate::task::TaskResult
//! [`Scheduler`]: crate::scheduler::Scheduler
//! [`DistEnv`]: crate::dist::DistEnv
//! [`RunFuture`]: crate::future::RunFuture
//! [`wait_all`]: crate::future::wait_all
//! [`ResultCache`]: crate::result_cache::ResultCache
//! [`SkipSet`]: crate::skip::SkipSet
//! [`ResultSink`]: crate::sink::ResultSink
//! [`persist_terminal`]: crate::sink::persist_terminal
//! [`SinkSubmitter`]: crate::sink_writer::SinkSubmitter
//! [`spawn_sink_writer`]: crate::sink_writer::spawn_sink_writer
//! [`CheckpointLedger`]: crate::checkpoint::CheckpointLedger
//! [`CheckpointTracker`]: crate::checkpoint::CheckpointTracker
//! [`stream_plan_tasks`]: crate::plan::stream_plan_tasks
//! [`WorkerActor`]: crate::worker::WorkerActor
//! [`WorkerCommand`]: crate::worker::WorkerCommand
//! [`JobControlActor`]: crate::job_control::JobControlActor
//! [`Driver`]: crate::driver::Driver
//! [`RunOptions`]: crate::driver::RunOptions
//! [`run_local_fleet`]: crate::runtime::run_local_fleet
//! [`run_fleet`]: crate::runtime::run_fleet
//! [`Observer`]: crate::observe::Observer
//! [`merge_pythonpath_parts`]: crate::python_env::merge_pythonpath_parts
//! [`AgentAbiClient`]: crate::agent_abi::AgentAbiClient
//! [`AgentAbiClientConfig`]: crate::agent_abi::AgentAbiClientConfig
//! [`PilotRuntimeBridge`]: crate::runtime_bridge::PilotRuntimeBridge

/// Stable ids for docs / observability (not a separate test suite).
pub mod ids {
    pub const TASK_WIRE: &str = "task_wire";
    pub const PLACEMENT: &str = "placement";
    pub const RUN_FUTURE: &str = "run_future";
    pub const IDEMPOTENCY: &str = "idempotency";
    pub const SKIP: &str = "skip";
    pub const SINK: &str = "sink";
    pub const SINK_WRITER: &str = "sink_writer";
    pub const CHECKPOINT: &str = "checkpoint";
    pub const PLAN: &str = "plan";
    pub const EXECUTE: &str = "execute";
    pub const WORKER: &str = "worker";
    pub const JOB_CONTROL: &str = "job_control";
    pub const PULSING_EXT: &str = "pulsing_ext";
    pub const DRIVER: &str = "driver";
    pub const FLEET: &str = "fleet";
    pub const OBSERVE: &str = "observe";
    pub const PYTHON_ENV: &str = "python_env";
    pub const AGENT_ABI: &str = "agent_abi";
    pub const RUNTIME_BRIDGE: &str = "runtime_bridge";
    pub const BATCH_TRAJECTORY: &str = "batch_trajectory";
    pub const FEDERATED_ANALYSIS: &str = "federated_analysis";

    pub const ALL: &[&str] = &[
        TASK_WIRE,
        PYTHON_ENV,
        AGENT_ABI,
        RUNTIME_BRIDGE,
        BATCH_TRAJECTORY,
        FEDERATED_ANALYSIS,
        PLACEMENT,
        RUN_FUTURE,
        IDEMPOTENCY,
        SKIP,
        SINK,
        SINK_WRITER,
        CHECKPOINT,
        OBSERVE,
        PLAN,
        EXECUTE,
        WORKER,
        JOB_CONTROL,
        PULSING_EXT,
        DRIVER,
        FLEET,
    ];
}

#[cfg(test)]
mod tests {
    use super::ids;

    #[test]
    fn primitive_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for id in ids::ALL {
            assert!(seen.insert(*id), "duplicate id {id}");
        }
    }
}
