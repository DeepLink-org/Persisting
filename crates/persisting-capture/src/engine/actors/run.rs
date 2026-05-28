//! Run-scoped actor — subagent registry and cross-story links (one per capture runtime).

use std::collections::HashMap;

use pulsing_actor::prelude::*;

use crate::engine::story::{RunId, StoryId};
use crate::record::CaptureRecord;
use crate::subagent_link::{enrich_record, main_route_for_backfill, SubagentRegistry};

use super::super::wire::{headers_to_header_map, RunCommand, RunReply};

/// Run-level actor: subagent spawn links + active story index for one proxy instance.
pub(crate) struct RunActor {
    registry: SubagentRegistry,
    stories_by_run: HashMap<String, Vec<String>>,
}

impl RunActor {
    pub fn new() -> Self {
        Self {
            registry: SubagentRegistry::default(),
            stories_by_run: HashMap::new(),
        }
    }

    fn track_story(&mut self, run_id: Option<&RunId>, story_id: &StoryId) {
        let Some(run_id) = run_id else {
            return;
        };
        let entry = self
            .stories_by_run
            .entry(run_id.as_str().to_string())
            .or_default();
        let sid = story_id.as_str().to_string();
        if !entry.contains(&sid) {
            entry.push(sid);
        }
    }
}

impl Default for RunActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for RunActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("runs".into(), self.stories_by_run.len().to_string()),
            (
                "tracked_runs".into(),
                self.registry.tracked_run_count().to_string(),
            ),
        ])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: RunCommand = msg.unpack()?;
        let reply = match cmd {
            RunCommand::Enrich {
                record_bytes,
                route,
                headers,
                body_bytes,
                assistant_text,
                story_id,
                run_id,
            } => {
                if let Some(ref sid) = story_id {
                    self.track_story(run_id.as_ref(), sid);
                }
                let mut record: CaptureRecord =
                    serde_json::from_slice(&record_bytes).map_err(|e| {
                        pulsing_actor::error::PulsingError::from(
                            pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                        )
                    })?;
                let header_map = headers_to_header_map(&headers).map_err(|e| {
                    pulsing_actor::error::PulsingError::from(
                        pulsing_actor::error::RuntimeError::Other(e.to_string()),
                    )
                })?;
                let body_val = body_bytes
                    .as_deref()
                    .map(serde_json::from_slice)
                    .transpose()
                    .map_err(|e| {
                        pulsing_actor::error::PulsingError::from(
                            pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                        )
                    })?;
                let outcome = enrich_record(
                    &mut record,
                    &route,
                    &header_map,
                    body_val.as_ref(),
                    assistant_text.as_deref(),
                    &mut self.registry,
                );
                RunReply::Enrich {
                    record_bytes: serde_json::to_vec(&record).map_err(|e| {
                        pulsing_actor::error::PulsingError::from(
                            pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                        )
                    })?,
                    backfills: outcome.spawn_link_backfills,
                }
            }
            RunCommand::MainRoute { route } => {
                RunReply::MainRoute(main_route_for_backfill(&route, &self.registry))
            }
        };
        Message::pack(&reply)
    }
}
