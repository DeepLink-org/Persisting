//! Per-session I/O actor — one mailbox per `seq_key`, no registry calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pulsing_actor::prelude::*;

use super::super::wire::{CaptureAck, DraftPayload, SessionCommand, SessionScope};
use crate::markdown_trajectory::BlockHeader;
use crate::sink::CaptureSink;
use crate::storage::markdown_pipeline::{LiveMarkdownWriter, MarkdownTarget};

/// Injected sink + markdown flag for each session actor instance.
#[derive(Clone)]
pub(crate) struct SessionActorDeps {
    pub sink: Arc<dyn CaptureSink>,
    pub stream_markdown: bool,
    pub storage: Arc<PathBuf>,
}

impl SessionActorDeps {
    pub fn new(sink: Arc<dyn CaptureSink>, storage: Arc<PathBuf>, stream_markdown: bool) -> Self {
        Self {
            sink,
            stream_markdown,
            storage,
        }
    }
}

/// Per-session I/O actor — one mailbox per `seq_key`, no registry calls.
pub(crate) struct CaptureSessionActor {
    seq_key: String,
    deps: SessionActorDeps,
    md: Option<LiveMarkdownWriter>,
}

impl CaptureSessionActor {
    pub fn new(seq_key: impl Into<String>, deps: SessionActorDeps) -> Self {
        Self {
            seq_key: seq_key.into(),
            deps,
            md: None,
        }
    }

    fn md_writer(&mut self, scope: &SessionScope) -> &mut LiveMarkdownWriter {
        if self.md.is_none() {
            self.md = Some(LiveMarkdownWriter::new(
                MarkdownTarget::new(
                    scope.route.clone(),
                    scope.agent_id.clone(),
                    self.deps.storage.as_path().to_path_buf(),
                ),
                self.deps.stream_markdown,
            ));
        }
        self.md.as_mut().expect("md writer initialized")
    }

    fn handle(&mut self, cmd: SessionCommand) -> Result<()> {
        match cmd {
            SessionCommand::Flush => return Ok(()),
            _ => {}
        }
        let scope = cmd.scope().clone();
        match cmd {
            SessionCommand::PersistRecord { record_json, .. } => {
                let mut rec: crate::record::CaptureRecord = serde_json::from_str(&record_json)?;
                self.deps
                    .sink
                    .append(&scope.route, &scope.agent_id, &mut rec)
                    .context("capture append")?;
                self.md_writer(&scope).write_record(&rec)?;
            }
            SessionCommand::UpsertDraft { draft_json, .. } => {
                let draft: DraftPayload = serde_json::from_str(&draft_json)?;
                let header = BlockHeader {
                    type_name: draft.type_name,
                    length: draft.length,
                    fields: draft.fields,
                };
                self.md_writer(&scope)
                    .write_draft(&draft.call_id, (header, draft.body))?;
            }
            SessionCommand::Flush => unreachable!("handled above"),
        }
        Ok(())
    }
}

#[async_trait]
impl Actor for CaptureSessionActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([("seq_key".into(), self.seq_key.clone())])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: SessionCommand = msg.unpack()?;
        let ack = match self.handle(cmd) {
            Ok(()) => CaptureAck::ok(),
            Err(e) => CaptureAck::err(format!("{e:#}")),
        };
        Message::pack(&ack)
    }
}
