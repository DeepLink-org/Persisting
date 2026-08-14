//! Job-scoped pPilot supervisor embedded into normal orchestration commands.

use anyhow::{bail, Context};
use persisting_agentctl::{NetworkBandwidthLimit, RunId, SupervisorBootstrap};
use persisting_pvisor::{
    SupervisorClientMessage, SupervisorDirective, SupervisorDirectiveEnvelope,
    SupervisorNetworkQuotaGrant, SupervisorServerMessage, SUPERVISOR_PROTOCOL_VERSION,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct EmbeddedSupervisorConfig {
    /// Aggregate job rate divided into conservative fixed shares.
    pub network_limit_bytes_per_second: Option<u64>,
    /// Maximum number of concurrently consuming pVisors. A quota grant receives
    /// `network_limit_bytes_per_second / quota_slots`.
    pub quota_slots: usize,
}

impl Default for EmbeddedSupervisorConfig {
    fn default() -> Self {
        Self {
            network_limit_bytes_per_second: None,
            quota_slots: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorRegistrationSnapshot {
    pub run_id: RunId,
    pub attempt_id: persisting_agentctl::AttemptId,
    pub lease_epoch: u64,
    pub connected: bool,
    pub last_heartbeat_unix_ms: u64,
    pub last_applied_directive_seq: u64,
}

struct LiveRegistration {
    snapshot: SupervisorRegistrationSnapshot,
    directives: mpsc::Sender<SupervisorDirectiveEnvelope>,
}

struct SupervisorState {
    token: String,
    controller_epoch: u64,
    config: EmbeddedSupervisorConfig,
    next_directive_seq: AtomicU64,
    registrations: Mutex<BTreeMap<RunId, LiveRegistration>>,
}

/// Handle owned by one pPilot execution session.
pub struct EmbeddedSupervisor {
    bootstrap: SupervisorBootstrap,
    state: Arc<SupervisorState>,
    stop: CancellationToken,
    join: Option<JoinHandle<anyhow::Result<()>>>,
}

impl EmbeddedSupervisor {
    pub async fn start(config: EmbeddedSupervisorConfig) -> anyhow::Result<Self> {
        if config.network_limit_bytes_per_second == Some(0) {
            bail!("Supervisor network limit must be greater than zero");
        }
        if let Some(limit) = config.network_limit_bytes_per_second {
            anyhow::ensure!(
                config.quota_slots > 0,
                "Supervisor quota slots must be non-zero"
            );
            anyhow::ensure!(
                limit >= config.quota_slots as u64,
                "Supervisor network limit is too small for {} quota slots",
                config.quota_slots
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind embedded pPilot Supervisor")?;
        let address = listener.local_addr()?;
        let controller_epoch = unix_now_ms().max(1);
        let token = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(SupervisorState {
            token: token.clone(),
            controller_epoch,
            config,
            next_directive_seq: AtomicU64::new(1),
            registrations: Mutex::new(BTreeMap::new()),
        });
        let stop = CancellationToken::new();
        let task_state = Arc::clone(&state);
        let task_stop = stop.clone();
        let join = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = task_stop.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let (stream, _) = accepted.context("accept pVisor Supervisor connection")?;
                let connection_state = Arc::clone(&task_state);
                let connection_stop = task_stop.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        handle_connection(stream, connection_state, connection_stop).await
                    {
                        tracing::debug!(%error, "pVisor Supervisor session ended");
                    }
                });
            }
            Ok(())
        });
        Ok(Self {
            bootstrap: SupervisorBootstrap {
                endpoint: format!("tcp://{address}"),
                token,
                controller_epoch,
                connect_timeout_ms: 500,
                attempt_registry_uri: None,
                attempt_ttl_ms: 15_000,
            },
            state,
            stop,
            join: Some(join),
        })
    }

    pub fn bootstrap(&self) -> SupervisorBootstrap {
        self.bootstrap.clone()
    }

    pub async fn registrations(&self) -> Vec<SupervisorRegistrationSnapshot> {
        self.state
            .registrations
            .lock()
            .await
            .values()
            .map(|registration| registration.snapshot.clone())
            .collect()
    }

    pub async fn cancel(&self, run_id: &RunId) -> anyhow::Result<u64> {
        let registrations = self.state.registrations.lock().await;
        let registration = registrations
            .get(run_id)
            .ok_or_else(|| anyhow::anyhow!("Run {run_id} is not registered with Supervisor"))?;
        let seq = self
            .state
            .next_directive_seq
            .fetch_add(1, Ordering::Relaxed);
        registration
            .directives
            .try_send(SupervisorDirectiveEnvelope {
                controller_epoch: self.state.controller_epoch,
                lease_epoch: registration.snapshot.lease_epoch,
                directive_seq: seq,
                directive: SupervisorDirective::Cancel,
            })
            .map_err(|_| anyhow::anyhow!("Run {run_id} Supervisor connection is closed"))?;
        Ok(seq)
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.stop.cancel();
        if let Some(join) = self.join.take() {
            join.await.context("join embedded pPilot Supervisor")??;
        }
        Ok(())
    }
}

impl Drop for EmbeddedSupervisor {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn handle_connection(
    stream: TcpStream,
    state: Arc<SupervisorState>,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let first = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .context("pVisor did not register within five seconds")??
        .ok_or_else(|| anyhow::anyhow!("pVisor closed before registration"))?;
    if first.len() > MAX_FRAME_BYTES {
        bail!("Supervisor registration exceeds {MAX_FRAME_BYTES} bytes");
    }
    let message: SupervisorClientMessage =
        serde_json::from_str(&first).context("decode pVisor Supervisor registration")?;
    let SupervisorClientMessage::Register(registration) = message else {
        send_message(
            &mut write,
            &SupervisorServerMessage::Error {
                message: "first Supervisor message must be register".into(),
            },
        )
        .await?;
        bail!("first Supervisor message was not register");
    };
    if registration.protocol_version != SUPERVISOR_PROTOCOL_VERSION {
        bail!(
            "unsupported Supervisor protocol {}; expected {}",
            registration.protocol_version,
            SUPERVISOR_PROTOCOL_VERSION
        );
    }
    if registration.token != state.token {
        send_message(
            &mut write,
            &SupervisorServerMessage::Error {
                message: "Supervisor authentication failed".into(),
            },
        )
        .await?;
        bail!("Supervisor authentication failed");
    }

    let (directive_tx, mut directive_rx) = mpsc::channel(32);
    let mut initial = Vec::new();
    if let Some(bytes_per_second) = state.config.network_limit_bytes_per_second {
        let bytes_per_second = bytes_per_second / state.config.quota_slots.max(1) as u64;
        let seq = state.next_directive_seq.fetch_add(1, Ordering::Relaxed);
        initial.push(SupervisorDirectiveEnvelope {
            controller_epoch: state.controller_epoch,
            lease_epoch: registration.lease_epoch,
            directive_seq: seq,
            directive: SupervisorDirective::GrantNetworkQuota(SupervisorNetworkQuotaGrant {
                grant_id: format!("{}-{seq}", state.controller_epoch),
                quota_epoch: state.controller_epoch,
                valid_until_unix_ms: u64::MAX,
                limit: NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second,
                },
            }),
        });
    }
    let run_id = registration.run_id.clone();
    state.registrations.lock().await.insert(
        run_id.clone(),
        LiveRegistration {
            snapshot: SupervisorRegistrationSnapshot {
                run_id: registration.run_id.clone(),
                attempt_id: registration.attempt_id.clone(),
                lease_epoch: registration.lease_epoch,
                connected: true,
                last_heartbeat_unix_ms: unix_now_ms(),
                last_applied_directive_seq: 0,
            },
            directives: directive_tx,
        },
    );
    send_message(
        &mut write,
        &SupervisorServerMessage::Registered {
            controller_epoch: state.controller_epoch,
            directives: initial,
        },
    )
    .await?;

    let outcome = loop {
        tokio::select! {
            _ = stop.cancelled() => break Ok(()),
            directive = directive_rx.recv() => {
                let Some(directive) = directive else { break Ok(()) };
                send_message(&mut write, &SupervisorServerMessage::Directive(directive)).await?;
            }
            line = lines.next_line() => {
                let Some(line) = line? else { break Ok(()) };
                if line.len() > MAX_FRAME_BYTES {
                    break Err(anyhow::anyhow!("Supervisor client frame exceeds {MAX_FRAME_BYTES} bytes"));
                }
                let message: SupervisorClientMessage = serde_json::from_str(&line)?;
                let mut registrations = state.registrations.lock().await;
                let Some(live) = registrations.get_mut(&run_id) else { continue };
                match message {
                    SupervisorClientMessage::Heartbeat(heartbeat) => {
                        live.snapshot.last_heartbeat_unix_ms = unix_now_ms();
                        live.snapshot.last_applied_directive_seq = heartbeat.last_applied_directive_seq;
                    }
                    SupervisorClientMessage::Ack(ack) => {
                        if ack.applied {
                            live.snapshot.last_applied_directive_seq = live
                                .snapshot
                                .last_applied_directive_seq
                                .max(ack.directive_seq);
                        }
                    }
                    SupervisorClientMessage::Register(_) => {}
                }
            }
        }
    };
    if let Some(live) = state.registrations.lock().await.get_mut(&run_id) {
        live.snapshot.connected = false;
    }
    outcome
}

async fn send_message(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    message: &SupervisorServerMessage,
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

/// Parse an explicit network rate. Lowercase `bps` units are bits per second;
/// `B/s` units are bytes per second.
pub fn parse_bandwidth(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let units = [
        ("gbps", 1_000_000_000_u64, true),
        ("mbps", 1_000_000, true),
        ("kbps", 1_000, true),
        ("bps", 1, true),
        ("gb/s", 1_000_000_000, false),
        ("mb/s", 1_000_000, false),
        ("kb/s", 1_000, false),
        ("b/s", 1, false),
    ];
    for (suffix, multiplier, bits) in units {
        if let Some(amount) = lower.strip_suffix(suffix) {
            let amount = amount
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("invalid bandwidth `{value}`"))?;
            let scaled = amount
                .checked_mul(multiplier)
                .ok_or_else(|| format!("bandwidth `{value}` is too large"))?;
            let bytes = if bits { scaled.div_ceil(8) } else { scaled };
            return (bytes > 0)
                .then_some(bytes)
                .ok_or_else(|| "bandwidth must be greater than zero".into());
        }
    }
    Err(format!(
        "invalid bandwidth `{value}`; use e.g. `10mbps` or `2mb/s`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_agentctl::AttemptId;
    use persisting_pvisor::SupervisorRegistration;

    #[test]
    fn bandwidth_parser_distinguishes_bits_and_bytes() {
        assert_eq!(parse_bandwidth("8bps").unwrap(), 1);
        assert_eq!(parse_bandwidth("10mbps").unwrap(), 1_250_000);
        assert_eq!(parse_bandwidth("2mb/s").unwrap(), 2_000_000);
        assert!(parse_bandwidth("0mbps").is_err());
    }

    #[tokio::test]
    async fn embedded_supervisor_authenticates_and_grants_quota() {
        let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig {
            network_limit_bytes_per_second: Some(32_768),
            quota_slots: 2,
        })
        .await
        .unwrap();
        let bootstrap = supervisor.bootstrap();
        let address = bootstrap.endpoint.strip_prefix("tcp://").unwrap();
        let stream = TcpStream::connect(address).await.unwrap();
        let (read, mut write) = stream.into_split();
        let registration = SupervisorClientMessage::Register(SupervisorRegistration {
            protocol_version: SUPERVISOR_PROTOCOL_VERSION,
            token: bootstrap.token,
            run_id: RunId::new("run-1"),
            attempt_id: AttemptId::new("attempt-1"),
            lease_epoch: 7,
        });
        write
            .write_all(format!("{}\n", serde_json::to_string(&registration).unwrap()).as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(read).lines();
        let reply: SupervisorServerMessage =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let SupervisorServerMessage::Registered { directives, .. } = reply else {
            panic!("expected registration response")
        };
        let SupervisorDirective::GrantNetworkQuota(grant) = &directives[0].directive else {
            panic!("expected quota grant")
        };
        assert_eq!(grant.limit.bytes_per_second, 16_384);
        supervisor.shutdown().await.unwrap();
    }
}
