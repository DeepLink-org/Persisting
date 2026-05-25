//! Subagent registry actor — serializes spawn-link state for one proxy run.

use std::collections::HashMap;

use pulsing_actor::prelude::*;

use crate::record::CaptureRecord;
use crate::subagent_link::{enrich_record, main_route_for_backfill, SubagentRegistry};

use super::super::wire::{headers_to_header_map, RegistryCommand, RegistryReply};

/// Subagent registry actor — serializes spawn-link state for one proxy run.
pub(crate) struct SubagentRegistryActor {
    inner: SubagentRegistry,
}

impl SubagentRegistryActor {
    pub fn new() -> Self {
        Self {
            inner: SubagentRegistry::default(),
        }
    }
}

impl Default for SubagentRegistryActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for SubagentRegistryActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([("runs".into(), self.inner.tracked_run_count().to_string())])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: RegistryCommand = msg.unpack()?;
        let reply = match cmd {
            RegistryCommand::Enrich {
                record_json,
                route,
                headers,
                body_json,
                assistant_text,
            } => {
                let mut record: CaptureRecord =
                    serde_json::from_str(&record_json).map_err(|e| {
                        pulsing_actor::error::PulsingError::from(
                            pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                        )
                    })?;
                let header_map = headers_to_header_map(&headers).map_err(|e| {
                    pulsing_actor::error::PulsingError::from(
                        pulsing_actor::error::RuntimeError::Other(e.to_string()),
                    )
                })?;
                let body_val = body_json
                    .as_ref()
                    .map(|s| serde_json::from_str(s))
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
                    &mut self.inner,
                );
                RegistryReply::Enrich {
                    record_json: serde_json::to_string(&record).map_err(|e| {
                        pulsing_actor::error::PulsingError::from(
                            pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                        )
                    })?,
                    backfills: outcome.spawn_link_backfills,
                }
            }
            RegistryCommand::MainRoute { route } => {
                RegistryReply::MainRoute(main_route_for_backfill(&route, &self.inner))
            }
        };
        Message::pack(&reply)
    }
}
