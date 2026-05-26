//! Registry actor command/reply pairs + runtime-side client helpers.

use anyhow::Result;
use pulsing_actor::ActorRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::record::CaptureRecord;
use crate::session_storage::CaptureRoute;
use crate::subagent_link::SpawnLinkBackfill;

use super::super::CaptureInvocation;

/// Global registry actor path (one per capture runtime / ActorSystem).
pub(crate) const REGISTRY_ACTOR_NAME: &str = "capture/subagent-registry";

/// Commands handled by [`super::super::actors::registry::SubagentRegistryActor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RegistryCommand {
    Enrich {
        record_json: String,
        route: CaptureRoute,
        headers: Vec<(String, String)>,
        body_json: Option<String>,
        assistant_text: Option<String>,
    },
    MainRoute {
        route: CaptureRoute,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RegistryReply {
    Enrich {
        record_json: String,
        backfills: Vec<SpawnLinkBackfill>,
    },
    MainRoute(CaptureRoute),
}

/// Enrich a record via registry actor; returns spawn-link backfills to apply later.
pub(crate) async fn registry_enrich(
    registry: &ActorRef,
    rec: &mut CaptureRecord,
    inv: &CaptureInvocation,
    body_json: Option<&Value>,
    assistant_text: Option<&str>,
) -> Result<Vec<SpawnLinkBackfill>> {
    let reply: RegistryReply = registry
        .ask(RegistryCommand::Enrich {
            record_json: serde_json::to_string(rec)?,
            route: inv.route.clone(),
            headers: inv.request_headers.clone(),
            body_json: body_json.map(serde_json::to_string).transpose()?,
            assistant_text: assistant_text.map(str::to_string),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match reply {
        RegistryReply::Enrich {
            record_json,
            backfills,
        } => {
            *rec = serde_json::from_str(&record_json)?;
            Ok(backfills)
        }
        _ => Err(anyhow::anyhow!("unexpected registry reply")),
    }
}

pub(crate) async fn registry_main_route(
    registry: &ActorRef,
    route: &CaptureRoute,
) -> Result<CaptureRoute> {
    let reply: RegistryReply = registry
        .ask(RegistryCommand::MainRoute {
            route: route.clone(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match reply {
        RegistryReply::MainRoute(r) => Ok(r),
        _ => Err(anyhow::anyhow!("unexpected registry reply")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_command_bincode_roundtrip() {
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "s".into(),
            storage_session_id: "s".into(),
            subagent_id: None,
        };
        let cmd = RegistryCommand::MainRoute {
            route: route.clone(),
        };
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: RegistryCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, RegistryCommand::MainRoute { .. }));

        let reply = RegistryReply::MainRoute(route);
        let packed = pulsing_actor::Message::pack(&reply).expect("pack reply");
        let back: RegistryReply = packed.unpack().expect("unpack reply");
        assert!(matches!(back, RegistryReply::MainRoute(_)));
    }
}
