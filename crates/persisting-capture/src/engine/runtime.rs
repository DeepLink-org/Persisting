//! [`CaptureRuntime`] — prepare → run actor → story actors.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use pulsing_actor::prelude::*;

use super::actors::{RunActor, StoryActor, StoryActorDeps};
use super::apply_queue::ApplyDispatcher;
use super::egress::persist_story_snapshots;
use super::prepare::CapturePreparer;
use super::story::Story;
use super::story::{StoryContext, StoryId};
use super::wal::{replay_pending, EventWal};
use super::wire::{
    run_main_route, CaptureAck, StoryCommand, StoryReply, StoryScope, RUN_ACTOR_NAME,
};
use super::{CallContext, Event};
use crate::dead_letter;
use crate::session_index::SessionIndexHandle;
use crate::sink::CaptureSink;
use crate::subagent_link::{spawn_link_backfill_record, SpawnLinkBackfill};

const STORY_MAILBOX: usize = 256;

pub(crate) struct CaptureRuntimeInner {
    system: Arc<ActorSystem>,
    preparer: Arc<CapturePreparer>,
    run: ActorRef,
    pub(crate) story_deps: StoryActorDeps,
    stories: Arc<DashMap<String, ActorRef>>,
    pub(crate) wal: Arc<EventWal>,
}

/// Actor topology (one ActorSystem per proxy):
///
/// ```text
/// CaptureRuntime
///   ├── capture/run              RunActor (subagent registry + run story index)
///   └── capture/story/{story_id} StoryActor (TurnMachine + sink/md I/O)
/// ```
#[derive(Clone)]
pub struct CaptureRuntime {
    inner: Arc<CaptureRuntimeInner>,
    apply_dispatcher: ApplyDispatcher,
}

impl CaptureRuntime {
    pub async fn new(
        sink: Arc<dyn CaptureSink>,
        index: SessionIndexHandle,
        storage: Arc<PathBuf>,
        stream_markdown: bool,
    ) -> Result<Self> {
        let system = ActorSystem::builder()
            .mailbox_capacity(STORY_MAILBOX)
            .build()
            .await
            .context("capture actor system")?;

        let run = system
            .spawn_named(RUN_ACTOR_NAME, RunActor::new())
            .await
            .map_err(pulsing_err)?;

        let sink = Arc::clone(&sink);
        let wal = Arc::new(EventWal::open(storage.as_path()));
        let inner = Arc::new(CaptureRuntimeInner {
            system,
            preparer: Arc::new(CapturePreparer {
                index,
                storage: Arc::clone(&storage),
                stream_markdown,
            }),
            run,
            story_deps: StoryActorDeps::new(sink, Arc::clone(&storage), stream_markdown),
            stories: Arc::new(DashMap::new()),
            wal,
        });
        let apply_dispatcher = ApplyDispatcher::new(Arc::clone(&inner));
        let runtime = Self {
            inner,
            apply_dispatcher,
        };

        runtime.replay_wal(storage.as_path()).await;

        Ok(runtime)
    }

    /// Replay events that were appended to the WAL but never acked
    /// (process crash between [`Self::spawn_apply`] and apply completion).
    async fn replay_wal(&self, storage: &Path) {
        let pending = replay_pending(storage);
        if pending.is_empty() {
            return;
        }
        tracing::info!(
            target: "persisting_capture",
            replayed = pending.len(),
            "wal replay starting"
        );
        for entry in pending {
            let ctx = Arc::new(entry.context.to_call_context());
            let event = entry.event.to_event();
            // Replay through the same dispatcher the original call used so that
            // ordering, dead-letter, and ack semantics all stay consistent.
            self.apply_dispatcher.enqueue(ctx, event, None);
        }
    }

    /// Enqueue a capture event on the per-story ordered apply queue (non-blocking for callers).
    ///
    /// Accepts both `CallContext` (owned) and `Arc<CallContext>` so callers in the
    /// streaming hot-path can share an `Arc` and avoid per-event clones, while
    /// non-streaming callers can keep passing owned values.
    ///
    /// Synchronously appends to the WAL before queuing so that an OOM/panic between this call
    /// and `apply_inner` completion is recoverable on next startup.
    pub fn spawn_apply(&self, ctx: impl Into<Arc<CallContext>>, event: Event) {
        let ctx = ctx.into();
        let wal_seq = self.inner.wal.append_event(&ctx, &event);
        self.apply_dispatcher.enqueue(ctx, event, wal_seq);
    }

    pub async fn apply(&self, ctx: &CallContext, event: Event) -> Result<()> {
        self.inner.apply(ctx, event).await
    }

    pub async fn shutdown(self) -> Result<()> {
        self.flush().await?;
        let snapshots = self.inner.collect_local_snapshots().await?;
        persist_story_snapshots(self.inner.story_deps.storage.as_path(), &snapshots)?;
        // Once snapshots and per-story dispatcher queues are drained, every WAL entry
        // has either been applied or dead-lettered — safe to truncate.
        if let Err(e) = self.inner.wal.truncate() {
            tracing::warn!(target: "persisting_capture", "wal truncate on shutdown: {e:#}");
        }
        self.inner.system.shutdown().await.map_err(pulsing_err)
    }

    /// Wait until accepted async apply jobs and story mailboxes have drained.
    pub async fn flush(&self) -> Result<()> {
        self.apply_dispatcher.flush().await?;
        self.inner.flush_stories().await?;
        self.inner.preparer.index.flush_if_dirty()?;
        Ok(())
    }

    /// Read-model snapshot for one story (TurnMachine state inside StoryActor).
    pub async fn story_snapshot(&self, context: &StoryContext) -> Result<Story> {
        self.inner.story_snapshot(context).await
    }
}

impl CaptureRuntimeInner {
    pub(crate) async fn apply(&self, ctx: &CallContext, event: Event) -> Result<()> {
        self.apply_inner(ctx, &event).await
    }

    async fn apply_inner(&self, ctx: &CallContext, event: &Event) -> Result<()> {
        let mut prepared = match self.preparer.prepare(&self.run, ctx, event.clone()).await {
            Ok(p) => p,
            Err(e) => {
                record_dead_letter(self.story_deps.storage.as_path(), ctx, event, &e, None);
                return Err(e);
            }
        };

        if !prepared.backfills.is_empty() {
            let main = run_main_route(&self.run, prepared.ctx.route()).await?;
            self.dispatch_backfills(&prepared.ctx, &main, &prepared.backfills)
                .await?;
        }

        if let Some(cmd) = prepared.take_story_command() {
            let record_json = persist_record_json(&cmd);
            let story_id = ctx.story_id().as_str().to_string();
            if let Err(e) = self.dispatch_story(&story_id, cmd).await {
                record_dead_letter(
                    self.story_deps.storage.as_path(),
                    ctx,
                    event,
                    &e,
                    record_json,
                );
                return Err(e);
            }
        }

        Ok(())
    }

    async fn flush_stories(&self) -> Result<()> {
        for entry in self.stories.iter() {
            let actor = entry.value().clone();
            let reply: StoryReply = actor.ask(StoryCommand::Flush).await.map_err(pulsing_err)?;
            story_reply_ack(reply)?.into_result()?;
        }
        Ok(())
    }

    async fn collect_local_snapshots(&self) -> Result<std::collections::HashMap<String, Story>> {
        let mut out = std::collections::HashMap::new();
        for entry in self.stories.iter() {
            let actor = entry.value().clone();
            let reply: StoryReply = actor
                .ask(StoryCommand::LocalSnapshot)
                .await
                .map_err(pulsing_err)?;
            if let StoryReply::LocalSnapshot {
                storage_session_id,
                story,
            } = reply
            {
                out.insert(storage_session_id, story);
            }
        }
        Ok(out)
    }

    async fn story_snapshot(&self, context: &StoryContext) -> Result<Story> {
        let story_id = context.story_id.as_str();
        let actor = self.story_actor(story_id).await?;
        let scope = StoryScope {
            context: context.clone(),
        };
        let reply: StoryReply = actor
            .ask(StoryCommand::Snapshot { scope })
            .await
            .map_err(pulsing_err)?;
        match reply {
            StoryReply::Snapshot { story } => Ok(story),
            StoryReply::LocalSnapshot { story, .. } => Ok(story),
            StoryReply::Ack(ack) => Err(anyhow::anyhow!(
                "unexpected ack for snapshot: {}",
                ack.error.unwrap_or_else(|| "unknown".into())
            )),
        }
    }

    async fn dispatch_story(&self, story_id: &str, cmd: StoryCommand) -> Result<()> {
        let actor = self.story_actor(story_id).await?;
        let reply: StoryReply = actor.ask(cmd).await.map_err(pulsing_err)?;
        story_reply_ack(reply)?.into_result()
    }

    async fn dispatch_backfills(
        &self,
        ctx: &CallContext,
        main_route: &crate::session_storage::CaptureRoute,
        backfills: &[SpawnLinkBackfill],
    ) -> Result<()> {
        let scope = StoryScope {
            context: StoryContext::from_route(main_route.clone(), ctx.agent_id().to_string()),
        };
        let actor = self
            .story_actor(
                StoryContext::from_route(main_route.clone(), ctx.agent_id())
                    .story_id()
                    .as_str(),
            )
            .await?;
        for bf in backfills {
            let rec = spawn_link_backfill_record(&bf.parent_call_id, &bf.links, &ctx.call);
            let record_bytes = serde_json::to_vec(&rec)?;
            let cmd = StoryCommand::persist_record(scope.clone(), record_bytes);
            let reply: StoryReply = actor.ask(cmd).await.map_err(pulsing_err)?;
            if let Err(e) = story_reply_ack(reply)?.into_result() {
                tracing::warn!("spawn link backfill failed: {e:#}");
            }
        }
        Ok(())
    }

    async fn story_actor(&self, story_id: &str) -> Result<ActorRef> {
        if let Some(existing) = self.stories.get(story_id) {
            return Ok(existing.clone());
        }

        let name = story_actor_name(story_id);
        if let Ok(actor) = self.system.resolve(name.as_str()).await {
            self.stories
                .entry(story_id.to_string())
                .or_insert(actor.clone());
            return Ok(actor);
        }

        let id = StoryId::new(story_id);
        let actor = self
            .system
            .spawning()
            .name(&name)
            .supervision(SupervisionSpec::on_failure().with_max_restarts(3))
            .mailbox_capacity(STORY_MAILBOX)
            .spawn(StoryActor::new(id, self.story_deps.clone()))
            .await
            .map_err(pulsing_err)?;

        match self.stories.entry(story_id.to_string()) {
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

fn story_reply_ack(reply: StoryReply) -> Result<CaptureAck> {
    match reply {
        StoryReply::Ack(ack) => Ok(ack),
        StoryReply::Snapshot { .. } | StoryReply::LocalSnapshot { .. } => {
            Err(anyhow::anyhow!("unexpected snapshot reply"))
        }
    }
}

/// Decode a `PersistRecord` command's JSON-encoded payload back to a `String` for
/// the dead-letter file (which carries `prepared_record_json` as a textual JSON).
/// Invalid UTF-8 should never happen — `serde_json::to_vec` always produces valid
/// UTF-8 — but if it ever does we drop the prepared JSON rather than panic.
fn persist_record_json(cmd: &StoryCommand) -> Option<String> {
    match cmd {
        StoryCommand::PersistRecord { record_bytes, .. } => {
            String::from_utf8(record_bytes.clone()).ok()
        }
        _ => None,
    }
}

fn record_dead_letter(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &anyhow::Error,
    prepared_record_json: Option<String>,
) {
    if let Err(dl) = dead_letter::append_dead_letter(
        storage,
        ctx,
        event,
        &format!("{error:#}"),
        prepared_record_json,
    ) {
        tracing::error!("dead letter write failed: {dl:#}");
    }
}

fn story_actor_name(story_id: &str) -> String {
    let sanitized = story_id.replace('|', "/");
    format!("capture/story/{sanitized}")
}

fn pulsing_err(e: pulsing_actor::error::PulsingError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}
