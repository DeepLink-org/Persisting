//! [`CaptureRuntime`] — orchestrates prepare → registry → session actors.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use pulsing_actor::prelude::*;

use super::actors::{CaptureSessionActor, SessionActorDeps, SubagentRegistryActor};
use super::prepare::CapturePreparer;
use super::types::{CaptureEvent, CaptureInvocation};
use super::wire::{
    registry_main_route, CaptureAck, SessionCommand, SessionScope, REGISTRY_ACTOR_NAME,
};
use crate::dead_letter;
use crate::session_index::SessionIndexHandle;
use crate::sink::CaptureSink;
use crate::subagent_link::{spawn_link_backfill_record, SpawnLinkBackfill};

const SESSION_MAILBOX: usize = 256;

/// Actor topology (one ActorSystem per proxy):
///
/// ```text
/// CaptureRuntime
///   ├── capture/subagent-registry   RegistryCommand → RegistryReply
///   └── capture/session/{seq_key}   SessionCommand  → CaptureAck
/// ```
#[derive(Clone)]
pub struct CaptureRuntime {
    system: Arc<ActorSystem>,
    preparer: Arc<CapturePreparer>,
    registry: ActorRef,
    session_deps: SessionActorDeps,
    sessions: Arc<DashMap<String, ActorRef>>,
}

impl CaptureRuntime {
    pub async fn new(
        sink: Arc<dyn CaptureSink>,
        index: SessionIndexHandle,
        storage: Arc<PathBuf>,
        stream_markdown: bool,
    ) -> Result<Self> {
        let system = ActorSystem::builder()
            .mailbox_capacity(SESSION_MAILBOX)
            .build()
            .await
            .context("capture actor system")?;

        let registry = system
            .spawn_named(REGISTRY_ACTOR_NAME, SubagentRegistryActor::new())
            .await
            .map_err(pulsing_err)?;

        Ok(Self {
            system,
            preparer: Arc::new(CapturePreparer {
                sink: Arc::clone(&sink),
                index,
                stream_markdown,
            }),
            registry,
            session_deps: SessionActorDeps::new(sink, storage, stream_markdown),
            sessions: Arc::new(DashMap::new()),
        })
    }

    pub async fn apply(&self, inv: &CaptureInvocation, event: CaptureEvent) -> Result<()> {
        let storage = self.session_deps.storage.as_path();
        match self.apply_inner(inv, &event).await {
            Ok(()) => Ok(()),
            Err(e) => {
                record_dead_letter(storage, inv, &event, &e, None);
                Err(e)
            }
        }
    }

    async fn apply_inner(&self, inv: &CaptureInvocation, event: &CaptureEvent) -> Result<()> {
        let mut prepared = self
            .preparer
            .prepare(&self.registry, inv, event.clone())
            .await?;

        if !prepared.backfills.is_empty() {
            let main = registry_main_route(&self.registry, &prepared.inv.route).await?;
            self.dispatch_backfills(&prepared.inv, &main, &prepared.backfills)
                .await?;
        }

        if let Some(cmd) = prepared.take_session_command() {
            let record_json = persist_record_json(&cmd);
            if let Err(e) = self.dispatch_session_tell(&inv.route.seq_key(), cmd).await {
                record_dead_letter(
                    self.session_deps.storage.as_path(),
                    inv,
                    event,
                    &e,
                    record_json,
                );
                return Err(e);
            }
        }

        Ok(())
    }

    pub async fn shutdown(self) -> Result<()> {
        self.flush_sessions().await?;
        self.system.shutdown().await.map_err(pulsing_err)
    }

    /// Wait until all session mailboxes have drained queued commands (for tests and graceful shutdown).
    pub async fn flush(&self) -> Result<()> {
        self.flush_sessions().await
    }

    async fn flush_sessions(&self) -> Result<()> {
        for entry in self.sessions.iter() {
            let actor = entry.value().clone();
            let ack: CaptureAck = actor
                .ask(SessionCommand::Flush)
                .await
                .map_err(pulsing_err)?;
            ack.into_result()?;
        }
        Ok(())
    }

    async fn dispatch_session_tell(&self, seq_key: &str, cmd: SessionCommand) -> Result<()> {
        let actor = self.session_actor(seq_key).await?;
        if cfg!(test) {
            let ack: CaptureAck = actor.ask(cmd).await.map_err(pulsing_err)?;
            ack.into_result()
        } else {
            actor.tell(cmd).await.map_err(pulsing_err)
        }
    }

    async fn dispatch_backfills(
        &self,
        inv: &CaptureInvocation,
        main_route: &crate::session_storage::CaptureRoute,
        backfills: &[SpawnLinkBackfill],
    ) -> Result<()> {
        let scope = SessionScope {
            route: main_route.clone(),
            agent_id: inv.agent_id.clone(),
        };
        let actor = self.session_actor(&main_route.seq_key()).await?;
        for bf in backfills {
            let rec = spawn_link_backfill_record(&bf.parent_call_id, &bf.links, &inv.call);
            let record_json = serde_json::to_string(&rec)?;
            let cmd = SessionCommand::persist_record(scope.clone(), record_json.clone());
            if let Err(e) = actor.tell(cmd).await.map_err(pulsing_err) {
                tracing::warn!("spawn link backfill tell: {e:#}");
            }
        }
        Ok(())
    }

    async fn session_actor(&self, seq_key: &str) -> Result<ActorRef> {
        if let Some(existing) = self.sessions.get(seq_key) {
            return Ok(existing.clone());
        }

        let name = session_actor_name(seq_key);
        if let Ok(actor) = self.system.resolve(name.as_str()).await {
            self.sessions
                .entry(seq_key.to_string())
                .or_insert(actor.clone());
            return Ok(actor);
        }

        let actor = self
            .system
            .spawning()
            .name(&name)
            .supervision(SupervisionSpec::on_failure().with_max_restarts(3))
            .mailbox_capacity(SESSION_MAILBOX)
            .spawn(CaptureSessionActor::new(seq_key, self.session_deps.clone()))
            .await
            .map_err(pulsing_err)?;

        match self.sessions.entry(seq_key.to_string()) {
            Entry::Occupied(entry) => Ok(entry.get().clone()),
            Entry::Vacant(entry) => {
                entry.insert(actor.clone());
                Ok(actor)
            }
        }
    }
}

/// Public entry type (actor-backed capture runtime).
pub type CaptureEngine = CaptureRuntime;

fn persist_record_json(cmd: &SessionCommand) -> Option<String> {
    match cmd {
        SessionCommand::PersistRecord { record_json, .. } => Some(record_json.clone()),
        _ => None,
    }
}

fn record_dead_letter(
    storage: &std::path::Path,
    inv: &CaptureInvocation,
    event: &CaptureEvent,
    error: &anyhow::Error,
    prepared_record_json: Option<String>,
) {
    if let Err(dl) = dead_letter::append_dead_letter(
        storage,
        inv,
        event,
        &format!("{error:#}"),
        prepared_record_json,
    ) {
        tracing::error!("dead letter write failed: {dl:#}");
    }
}

fn session_actor_name(seq_key: &str) -> String {
    let sanitized = seq_key.replace('|', "/");
    format!("capture/session/{sanitized}")
}

fn pulsing_err(e: pulsing_actor::error::PulsingError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}
