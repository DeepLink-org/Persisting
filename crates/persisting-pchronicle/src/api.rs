//! Async-first application API for canonical trajectory operations.

use crate::operations::trajectory;
use crate::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryMaterializeRequest,
    TrajectoryMaterializeResponse, TrajectoryReplayRequest, TrajectoryReplayResponse,
    TrajectoryStatsRequest, TrajectoryStatsResponse,
};

/// Stateless async facade. Callers own their executor and can cheaply clone
/// this value into CLI, server, Gateway, or pPilot tasks.
#[derive(Debug, Clone, Copy, Default)]
pub struct Chronicle;

impl Chronicle {
    pub async fn append(
        &self,
        request: TrajectoryAppendRequest,
    ) -> anyhow::Result<TrajectoryAppendResponse> {
        trajectory::append_async(request).await
    }

    pub async fn replay(
        &self,
        request: TrajectoryReplayRequest,
    ) -> anyhow::Result<TrajectoryReplayResponse> {
        trajectory::replay_async(request).await
    }

    pub async fn stats(
        &self,
        request: TrajectoryStatsRequest,
    ) -> anyhow::Result<TrajectoryStatsResponse> {
        trajectory::stats_async(request).await
    }

    pub async fn materialize(
        &self,
        request: TrajectoryMaterializeRequest,
    ) -> anyhow::Result<TrajectoryMaterializeResponse> {
        trajectory::materialize_async(request).await
    }
}
