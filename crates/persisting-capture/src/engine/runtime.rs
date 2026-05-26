//! [`CaptureRuntime`] — prepare → run actor → story actors.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use pulsing_actor::prelude::*;

use super::actors::{RunActor, StoryActor, StoryActorDeps};
use super::apply_queue::ApplyDispatcher;
use super::prepare::CapturePreparer;
use super::story::Story;
use super::story::{StoryContext, StoryId};
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
        let inner = Arc::new(CaptureRuntimeInner {
            system,
            preparer: Arc::new(CapturePreparer {
                sink: Arc::clone(&sink),
                index,
                storage: Arc::clone(&storage),
                stream_markdown,
            }),
            run,
            story_deps: StoryActorDeps::new(sink, Arc::clone(&storage), stream_markdown),
            stories: Arc::new(DashMap::new()),
        });
        let apply_dispatcher = ApplyDispatcher::new(Arc::clone(&inner));
        Ok(Self {
            inner,
            apply_dispatcher,
        })
    }

    /// Enqueue a capture event on the per-story ordered apply queue (non-blocking for callers).
    pub fn spawn_apply(&self, ctx: CallContext, event: Event) {
        self.apply_dispatcher.enqueue(ctx, event);
    }

    pub async fn apply(&self, ctx: &CallContext, event: Event) -> Result<()> {
        self.inner.apply(ctx, event).await
    }

    pub async fn shutdown(self) -> Result<()> {
        self.flush().await?;
        self.inner.system.shutdown().await.map_err(pulsing_err)
    }

    /// Wait until all story mailboxes have drained queued commands.
    pub async fn flush(&self) -> Result<()> {
        self.inner.flush_stories().await
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
            let record_json = serde_json::to_string(&rec)?;
            let cmd = StoryCommand::persist_record(scope.clone(), record_json);
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
        StoryReply::Snapshot { .. } => Err(anyhow::anyhow!("unexpected snapshot reply")),
    }
}

fn persist_record_json(cmd: &StoryCommand) -> Option<String> {
    match cmd {
        StoryCommand::PersistRecord { record_json, .. } => Some(record_json.clone()),
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
