//! Run actor command/reply pairs + runtime-side client helpers.

use anyhow::Result;
use pulsing_actor::ActorRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::story::{RunId, StoryId};
use crate::record::CaptureRecord;
use crate::session_storage::CaptureRoute;
use crate::subagent_link::SpawnLinkBackfill;

use super::super::CallContext;

/// Global run actor path (one per capture runtime / ActorSystem).
pub(crate) const RUN_ACTOR_NAME: &str = "capture/run";

/// Commands handled by [`super::super::actors::run::RunActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RunCommand {
    Enrich {
        record_json: String,
        route: CaptureRoute,
        headers: Vec<(String, String)>,
        body_json: Option<String>,
        assistant_text: Option<String>,
        story_id: Option<StoryId>,
        run_id: Option<RunId>,
    },
    MainRoute {
        route: CaptureRoute,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RunReply {
    Enrich {
        record_json: String,
        backfills: Vec<SpawnLinkBackfill>,
    },
    MainRoute(CaptureRoute),
}

pub(crate) async fn run_enrich(
    run: &ActorRef,
    rec: &mut CaptureRecord,
    ctx: &CallContext,
    body_json: Option<&Value>,
    assistant_text: Option<&str>,
) -> Result<Vec<SpawnLinkBackfill>> {
    let story = ctx.story.clone();
    let reply: RunReply = run
        .ask(RunCommand::Enrich {
            record_json: serde_json::to_string(rec)?,
            route: ctx.route().clone(),
            headers: ctx.request_headers.clone(),
            body_json: body_json.map(serde_json::to_string).transpose()?,
            assistant_text: assistant_text.map(str::to_string),
            story_id: Some(story.story_id.clone()),
            run_id: story.run_id.clone(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match reply {
        RunReply::Enrich {
            record_json,
            backfills,
        } => {
            *rec = serde_json::from_str(&record_json)?;
            Ok(backfills)
        }
        _ => Err(anyhow::anyhow!("unexpected run reply")),
    }
}

pub(crate) async fn run_main_route(run: &ActorRef, route: &CaptureRoute) -> Result<CaptureRoute> {
    let reply: RunReply = run
        .ask(RunCommand::MainRoute {
            route: route.clone(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match reply {
        RunReply::MainRoute(r) => Ok(r),
        _ => Err(anyhow::anyhow!("unexpected run reply")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_bincode_roundtrip() {
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "s".into(),
            storage_session_id: "s".into(),
            subagent_id: None,
        };
        let cmd = RunCommand::MainRoute {
            route: route.clone(),
        };
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: RunCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, RunCommand::MainRoute { .. }));
    }
}
