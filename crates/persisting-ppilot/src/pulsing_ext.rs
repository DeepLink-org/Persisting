//! Thin helpers over Pulsing so pPilot uses one resolve/spawn style.

use anyhow::{Context, Result};
use pulsing_actor::prelude::*;
use std::sync::Arc;
use std::time::Duration;

/// Default infra `ask` deadline (select! / timeout wrapper).
pub const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// Worker slot supervision: restart on failure, capped.
pub fn worker_supervision() -> SupervisionSpec {
    SupervisionSpec::on_failure().with_max_restarts(3)
}

/// Unified named resolve (prefer this over mixing `resolve` / `resolve_named`).
pub async fn resolve_actor(system: &ActorSystem, name: &str) -> Result<ActorRef> {
    system
        .resolve_named(name, None)
        .await
        .with_context(|| format!("resolve Pulsing actor {name}"))
}

/// `ask` with an explicit deadline (Pulsing has no ask_timeout yet).
pub async fn ask_timeout<M, R>(actor: &ActorRef, msg: M, timeout: Duration) -> Result<R>
where
    M: serde::Serialize + 'static,
    R: serde::de::DeserializeOwned,
{
    match tokio::time::timeout(timeout, actor.ask::<M, R>(msg)).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(anyhow::anyhow!("pulsing ask: {e}")),
        Err(_) => Err(anyhow::anyhow!("pulsing ask timed out after {timeout:?}")),
    }
}

/// Spawn a named actor with supervision via factory (enables restart).
pub async fn spawn_supervised<F, A>(
    system: &Arc<ActorSystem>,
    name: &str,
    factory: F,
) -> Result<ActorRef>
where
    F: FnMut() -> pulsing_actor::error::Result<A> + Send + 'static,
    A: Actor,
{
    system
        .spawning()
        .name(name)
        .supervision(worker_supervision())
        .mailbox_capacity(256)
        .spawn_factory(factory)
        .await
        .with_context(|| format!("spawn supervised {name}"))
}
