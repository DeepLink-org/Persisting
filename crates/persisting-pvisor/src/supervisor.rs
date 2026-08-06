//! Optional pPilot Supervisor client. The Run data plane never depends on it.

use persisting_control::{AttemptId, NetworkBandwidthLimit, RunId, SupervisorBootstrap};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

pub const SUPERVISOR_PROTOCOL_VERSION: u32 = 1;

fn supervisor_protocol_version() -> u32 {
    SUPERVISOR_PROTOCOL_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRegistration {
    #[serde(default = "supervisor_protocol_version")]
    pub protocol_version: u32,
    pub token: String,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorHeartbeat {
    pub last_applied_directive_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorDirectiveAck {
    pub directive_seq: u64,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorClientMessage {
    Register(SupervisorRegistration),
    Heartbeat(SupervisorHeartbeat),
    Ack(SupervisorDirectiveAck),
}

/// A time-bounded rate grant. pVisor enforces it locally on intercepted proxy
/// traffic, so consuming bytes never performs a synchronous control-plane RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorNetworkQuotaGrant {
    pub grant_id: String,
    pub quota_epoch: u64,
    pub valid_until_unix_ms: u64,
    pub limit: NetworkBandwidthLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisorDirective {
    GrantNetworkQuota(SupervisorNetworkQuotaGrant),
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorDirectiveEnvelope {
    pub controller_epoch: u64,
    pub lease_epoch: u64,
    pub directive_seq: u64,
    pub directive: SupervisorDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorServerMessage {
    Registered {
        controller_epoch: u64,
        directives: Vec<SupervisorDirectiveEnvelope>,
    },
    Directive(SupervisorDirectiveEnvelope),
    Error {
        message: String,
    },
}

pub(crate) struct SupervisorConnectOutcome {
    pub(crate) connected: Option<bool>,
    pub(crate) controller_epoch: Option<u64>,
    pub(crate) initial_limits: Vec<NetworkBandwidthLimit>,
    pub(crate) warning: Option<String>,
    pub(crate) session: Option<SupervisorSession>,
}

pub(crate) struct SupervisorSession {
    stop: CancellationToken,
    _join: JoinHandle<()>,
}

impl Drop for SupervisorSession {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) async fn connect_optional(
    bootstrap: Option<&SupervisorBootstrap>,
    run_id: &RunId,
    attempt_id: &AttemptId,
    lease_epoch: u64,
    run_cancellation: CancellationToken,
) -> SupervisorConnectOutcome {
    let Some(bootstrap) = bootstrap else {
        return SupervisorConnectOutcome {
            connected: None,
            controller_epoch: None,
            initial_limits: Vec::new(),
            warning: None,
            session: None,
        };
    };
    match connect(bootstrap, run_id, attempt_id, lease_epoch, run_cancellation).await {
        Ok(outcome) => outcome,
        Err(error) => SupervisorConnectOutcome {
            connected: Some(false),
            controller_epoch: None,
            initial_limits: Vec::new(),
            warning: Some(format!(
                "optional pPilot Supervisor unavailable; continuing standalone: {error:#}"
            )),
            session: None,
        },
    }
}

async fn connect(
    bootstrap: &SupervisorBootstrap,
    run_id: &RunId,
    attempt_id: &AttemptId,
    lease_epoch: u64,
    run_cancellation: CancellationToken,
) -> anyhow::Result<SupervisorConnectOutcome> {
    anyhow::ensure!(
        bootstrap.controller_epoch > 0,
        "Supervisor controller epoch must be non-zero"
    );
    let address = bootstrap
        .endpoint
        .strip_prefix("tcp://")
        .ok_or_else(|| anyhow::anyhow!("unsupported Supervisor endpoint {}", bootstrap.endpoint))?;
    let timeout = Duration::from_millis(bootstrap.connect_timeout_ms.max(1));
    let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
        .await
        .map_err(|_| anyhow::anyhow!("Supervisor connect timed out after {timeout:?}"))??;
    let (read, mut write) = stream.into_split();
    let registration = SupervisorClientMessage::Register(SupervisorRegistration {
        protocol_version: SUPERVISOR_PROTOCOL_VERSION,
        token: bootstrap.token.clone(),
        run_id: run_id.clone(),
        attempt_id: attempt_id.clone(),
        lease_epoch,
    });
    write_client_message(&mut write, &registration).await?;
    let mut lines = BufReader::new(read).lines();
    let line = tokio::time::timeout(timeout, lines.next_line())
        .await
        .map_err(|_| anyhow::anyhow!("Supervisor registration timed out after {timeout:?}"))??
        .ok_or_else(|| anyhow::anyhow!("Supervisor closed during registration"))?;
    anyhow::ensure!(
        line.len() <= MAX_FRAME_BYTES,
        "Supervisor registration response exceeds {MAX_FRAME_BYTES} bytes"
    );
    let response: SupervisorServerMessage = serde_json::from_str(&line)?;
    let (controller_epoch, directives) = match response {
        SupervisorServerMessage::Registered {
            controller_epoch,
            directives,
        } => (controller_epoch, directives),
        SupervisorServerMessage::Error { message } => anyhow::bail!(message),
        SupervisorServerMessage::Directive(_) => {
            anyhow::bail!("Supervisor sent a directive before registration completed")
        }
    };
    anyhow::ensure!(
        controller_epoch >= bootstrap.controller_epoch,
        "stale Supervisor controller epoch {controller_epoch}; expected at least {}",
        bootstrap.controller_epoch
    );

    let mut initial_limits = Vec::new();
    let mut last_applied = 0;
    for directive in &directives {
        if validate_directive(directive, controller_epoch, lease_epoch, last_applied).is_err() {
            continue;
        }
        if let SupervisorDirective::GrantNetworkQuota(grant) = &directive.directive {
            if grant.valid_until_unix_ms >= unix_now_ms() && grant.limit.bytes_per_second > 0 {
                initial_limits.push(grant.limit.clone());
                last_applied = directive.directive_seq;
                write_client_message(
                    &mut write,
                    &SupervisorClientMessage::Ack(SupervisorDirectiveAck {
                        directive_seq: directive.directive_seq,
                        applied: true,
                    }),
                )
                .await?;
            }
        }
    }

    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let last_applied = Arc::new(Mutex::new(last_applied));
    let task_last_applied = Arc::clone(&last_applied);
    let join = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let result: anyhow::Result<()> = tokio::select! {
                _ = task_stop.cancelled() => break,
                _ = heartbeat.tick() => {
                    let seq = *task_last_applied.lock().await;
                    write_client_message(
                        &mut write,
                        &SupervisorClientMessage::Heartbeat(SupervisorHeartbeat {
                            last_applied_directive_seq: seq,
                        }),
                    ).await
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) if line.len() <= MAX_FRAME_BYTES => {
                            match serde_json::from_str::<SupervisorServerMessage>(&line) {
                                Ok(SupervisorServerMessage::Directive(directive)) => {
                                    let current = *task_last_applied.lock().await;
                                    let result = apply_live_directive(
                                        &directive,
                                        controller_epoch,
                                        lease_epoch,
                                        current,
                                        &run_cancellation,
                                    );
                                    if result.0 {
                                        *task_last_applied.lock().await = directive.directive_seq;
                                    }
                                    write_client_message(
                                        &mut write,
                                        &SupervisorClientMessage::Ack(SupervisorDirectiveAck {
                                            directive_seq: directive.directive_seq,
                                            applied: result.0,
                                        }),
                                    ).await
                                }
                                Ok(_) => Ok(()),
                                Err(error) => Err(error.into()),
                            }
                        }
                        Ok(Some(_)) => Err(anyhow::anyhow!("Supervisor frame exceeds {MAX_FRAME_BYTES} bytes")),
                        Ok(None) => break,
                        Err(error) => Err(error.into()),
                    }
                }
            };
            if let Err(error) = result {
                tracing::debug!(%error, "optional pPilot Supervisor disconnected; Run continues standalone");
                break;
            }
        }
    });

    Ok(SupervisorConnectOutcome {
        connected: Some(true),
        controller_epoch: Some(controller_epoch),
        initial_limits,
        warning: None,
        session: Some(SupervisorSession { stop, _join: join }),
    })
}

fn validate_directive(
    directive: &SupervisorDirectiveEnvelope,
    controller_epoch: u64,
    lease_epoch: u64,
    last_applied: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        directive.controller_epoch == controller_epoch,
        "Supervisor directive controller epoch mismatch"
    );
    anyhow::ensure!(
        directive.lease_epoch == lease_epoch,
        "Supervisor directive lease epoch mismatch"
    );
    anyhow::ensure!(
        directive.directive_seq > last_applied,
        "Supervisor directive is stale or duplicated"
    );
    Ok(())
}

fn apply_live_directive(
    directive: &SupervisorDirectiveEnvelope,
    controller_epoch: u64,
    lease_epoch: u64,
    last_applied: u64,
    run_cancellation: &CancellationToken,
) -> (bool, Option<String>) {
    if let Err(error) = validate_directive(directive, controller_epoch, lease_epoch, last_applied) {
        return (false, Some(error.to_string()));
    }
    match &directive.directive {
        SupervisorDirective::Cancel => {
            run_cancellation.cancel();
            (true, None)
        }
        SupervisorDirective::GrantNetworkQuota(_) => (
            false,
            Some("live quota replacement is not supported by this pVisor version".into()),
        ),
    }
}

async fn write_client_message(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    message: &SupervisorClientMessage,
) -> anyhow::Result<()> {
    let encoded = serde_json::to_vec(message)?;
    write.write_all(&encoded).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    Ok(())
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_messages_roundtrip_as_json() {
        let message = SupervisorServerMessage::Directive(SupervisorDirectiveEnvelope {
            controller_epoch: 4,
            lease_epoch: 9,
            directive_seq: 2,
            directive: SupervisorDirective::GrantNetworkQuota(SupervisorNetworkQuotaGrant {
                grant_id: "grant-1".into(),
                quota_epoch: 3,
                valid_until_unix_ms: 100,
                limit: NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 32_768,
                },
            }),
        });
        let encoded = serde_json::to_vec(&message).unwrap();
        let decoded: SupervisorServerMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }
}
