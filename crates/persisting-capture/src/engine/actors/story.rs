//! Per-story I/O actor — one mailbox per `story_id`, owns turn state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pulsing_actor::prelude::*;

use super::super::story::{StoryId, TurnMachine};
use super::super::wire::{CaptureAck, DraftPayload, StoryCommand, StoryReply, StoryScope};
use crate::markdown_trajectory::BlockHeader;
use crate::sink::CaptureSink;
use crate::storage::markdown_pipeline::{LiveMarkdownWriter, MarkdownTarget};

/// Injected sink + markdown flag for each story actor instance.
#[derive(Clone)]
pub(crate) struct StoryActorDeps {
    pub sink: Arc<dyn CaptureSink>,
    pub stream_markdown: bool,
    pub storage: Arc<PathBuf>,
}

impl StoryActorDeps {
    pub fn new(sink: Arc<dyn CaptureSink>, storage: Arc<PathBuf>, stream_markdown: bool) -> Self {
        Self {
            sink,
            stream_markdown,
            storage,
        }
    }
}

/// Per-story actor — serializes I/O and maintains [`TurnMachine`] for the narrative index.
pub(crate) struct StoryActor {
    story_id: StoryId,
    deps: StoryActorDeps,
    turns: TurnMachine,
    md: Option<LiveMarkdownWriter>,
}

impl StoryActor {
    pub fn new(story_id: StoryId, deps: StoryActorDeps) -> Self {
        let turns = TurnMachine::new(story_id.clone());
        Self {
            story_id,
            deps,
            turns,
            md: None,
        }
    }

    fn sync_scope(&mut self, scope: &StoryScope) {
        self.turns
            .set_story_meta(scope.agent_id(), scope.context.run_id.clone());
    }

    fn md_writer(&mut self, scope: &StoryScope) -> &mut LiveMarkdownWriter {
        if self.md.is_none() {
            self.md = Some(LiveMarkdownWriter::new(
                MarkdownTarget::new(
                    scope.route().clone(),
                    scope.agent_id().to_string(),
                    self.deps.storage.as_path().to_path_buf(),
                ),
                self.deps.stream_markdown,
            ));
        }
        self.md.as_mut().expect("md writer initialized")
    }

    fn handle(&mut self, cmd: StoryCommand) -> Result<StoryReply> {
        match cmd {
            StoryCommand::Flush => return Ok(StoryReply::Ack(CaptureAck::ok())),
            StoryCommand::Snapshot { scope } => {
                self.sync_scope(&scope);
                return Ok(StoryReply::Snapshot {
                    story: self.turns.snapshot(),
                });
            }
            _ => {}
        }
        let scope = cmd.scope().clone();
        self.sync_scope(&scope);
        match cmd {
            StoryCommand::PersistRecord { record_json, .. } => {
                let mut rec: crate::record::CaptureRecord = serde_json::from_str(&record_json)?;
                self.turns.observe_record(&mut rec);
                self.deps
                    .sink
                    .append(scope.route(), scope.agent_id(), &mut rec)
                    .context("capture append")?;
                self.md_writer(&scope).write_record(&rec)?;
            }
            StoryCommand::UpsertDraft { draft_json, .. } => {
                let draft: DraftPayload = serde_json::from_str(&draft_json)?;
                let header = BlockHeader {
                    type_name: draft.type_name,
                    length: draft.length,
                    fields: draft.fields,
                };
                self.md_writer(&scope)
                    .write_draft(&draft.call_id, (header, draft.body))?;
            }
            StoryCommand::Flush | StoryCommand::Snapshot { .. } => unreachable!(),
        }
        Ok(StoryReply::Ack(CaptureAck::ok()))
    }
}

#[async_trait]
impl Actor for StoryActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("story_id".into(), self.story_id.as_str().to_string()),
            ("turns".into(), self.turns.turns().len().to_string()),
        ])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: StoryCommand = msg.unpack()?;
        let reply = match self.handle(cmd) {
            Ok(r) => r,
            Err(e) => StoryReply::Ack(CaptureAck::err(format!("{e:#}"))),
        };
        Message::pack(&reply)
    }
}
