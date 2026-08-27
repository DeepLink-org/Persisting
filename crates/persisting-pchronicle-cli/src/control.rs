//! pChronicle's write-capable control plane.
//!
//! The Warehouse HTTP server remains read-only. This long-lived JSONL/stdin
//! protocol is intended for trusted local orchestrators such as pPilot.

use anyhow::{Context, Result};
use persisting_events::{
    AttemptRecord as ProtocolAttemptRecord, AttemptRecordState as ProtocolAttemptRecordState,
    CHRONICLE_CONTROL_MAX_FRAME_BYTES, CHRONICLE_CONTROL_VERSION, ChronicleControlEnvelope,
    ChronicleControlRequest, ChronicleControlResponse, ChronicleControlResponseEnvelope,
    ChronicleServeControlReady, CommitRunOutcome, LeaseAcquireOutcome, TrajectoryAppendRequest,
    TrajectoryAppendResponse, TrajectoryFormat,
};
use persisting_pchronicle::storage::{
    AttemptRecord, AttemptRecordState, AttemptRegistry, DatasetLocation, RawEventLanceStore,
    RunControlStore, StoryCoords,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub(super) struct PreparedControl {
    listener: tokio::net::TcpListener,
    endpoint: SocketAddr,
    auth_token: String,
    control: Arc<RunControlStore>,
    attempts: Arc<AttemptRegistry>,
}

impl PreparedControl {
    pub(super) async fn bind(storage: &str, listen: SocketAddr) -> Result<Self> {
        anyhow::ensure!(
            listen.ip().is_loopback(),
            "pChronicle control may only bind to a loopback address"
        );
        let control = Arc::new(
            RunControlStore::open(storage)
                .await
                .context("open pChronicle Run control store")?,
        );
        let attempts = Arc::new(
            AttemptRegistry::open(storage)
                .await
                .context("open pChronicle Attempt registry")?,
        );
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .context("bind pChronicle control listener")?;
        let endpoint = listener
            .local_addr()
            .context("read pChronicle control listener address")?;
        Ok(Self {
            listener,
            endpoint,
            auth_token: uuid::Uuid::new_v4().simple().to_string(),
            control,
            attempts,
        })
    }

    pub(super) fn ready(&self) -> ChronicleServeControlReady {
        ChronicleServeControlReady {
            endpoint: self.endpoint.to_string(),
            auth_token: self.auth_token.clone(),
        }
    }

    pub(super) async fn serve(self, shutdown: impl std::future::Future<Output = ()>) -> Result<()> {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.context("accept pChronicle control client")?;
                    stream
                        .set_nodelay(true)
                        .context("configure pChronicle control socket")?;
                    let control = Arc::clone(&self.control);
                    let attempts = Arc::clone(&self.attempts);
                    let auth_token = self.auth_token.clone();
                    tokio::spawn(async move {
                        if let Err(error) =
                            serve_connection(stream, control, attempts, auth_token).await
                        {
                            eprintln!("pChronicle control request failed: {error:#}");
                        }
                    });
                }
            }
        }
    }
}

async fn serve_connection(
    stream: TcpStream,
    control: Arc<RunControlStore>,
    attempts: Arc<AttemptRegistry>,
    auth_token: String,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut input = BufReader::new(read.take((CHRONICLE_CONTROL_MAX_FRAME_BYTES + 1) as u64));
    let mut line = String::new();
    let bytes = input
        .read_line(&mut line)
        .await
        .context("read control request")?;
    anyhow::ensure!(bytes > 0, "empty pChronicle control request");
    let request_id = serde_json::from_str::<ChronicleControlEnvelope>(&line)
        .map(|request| request.request_id)
        .unwrap_or(0);
    let response = match decode_request(&line, &auth_token) {
        Ok(request) => handle_request(&control, &attempts, request).await,
        Err(error) => Err(error),
    };
    let response = ChronicleControlResponseEnvelope {
        version: CHRONICLE_CONTROL_VERSION,
        request_id,
        response: match response {
            Ok(response) => response,
            Err(error) => ChronicleControlResponse::Error {
                message: format!("{error:#}"),
            },
        },
    };
    let mut encoded = serde_json::to_vec(&response).context("encode control response")?;
    anyhow::ensure!(
        encoded.len() <= CHRONICLE_CONTROL_MAX_FRAME_BYTES,
        "pChronicle control response exceeds frame limit"
    );
    encoded.push(b'\n');
    write
        .write_all(&encoded)
        .await
        .context("write control response")?;
    write.flush().await.context("flush control response")?;
    Ok(())
}

fn decode_request(line: &str, auth_token: &str) -> Result<ChronicleControlEnvelope> {
    anyhow::ensure!(
        line.len() <= CHRONICLE_CONTROL_MAX_FRAME_BYTES,
        "pChronicle control request exceeds frame limit"
    );
    let request: ChronicleControlEnvelope =
        serde_json::from_str(line).context("decode pChronicle control request")?;
    anyhow::ensure!(
        request.version == CHRONICLE_CONTROL_VERSION,
        "unsupported pChronicle control version {}",
        request.version
    );
    anyhow::ensure!(
        request.auth_token == auth_token,
        "invalid pChronicle control token"
    );
    Ok(request)
}

async fn handle_request(
    control: &RunControlStore,
    attempts: &AttemptRegistry,
    envelope: ChronicleControlEnvelope,
) -> Result<ChronicleControlResponse> {
    use ChronicleControlRequest as Request;
    use ChronicleControlResponse as Response;

    Ok(match envelope.request {
        Request::Ping => Response::Pong,
        Request::AcquireLease {
            run_id,
            task_id,
            owner,
            ttl_ms,
        } => Response::LeaseAcquire(map_lease_outcome(
            control
                .acquire_lease(&run_id, task_id.as_deref(), &owner, ttl_ms)
                .await?,
        )),
        Request::TakeoverLease {
            run_id,
            task_id,
            owner,
            ttl_ms,
        } => Response::LeaseAcquire(map_lease_outcome(
            control
                .takeover_lease(&run_id, task_id.as_deref(), &owner, ttl_ms)
                .await?,
        )),
        Request::BindAttempt {
            run_id,
            epoch,
            attempt_id,
        } => Response::Boolean(control.bind_attempt(&run_id, epoch, attempt_id).await?),
        Request::RenewLease {
            run_id,
            epoch,
            owner,
            ttl_ms,
        } => Response::Boolean(control.renew_lease(&run_id, epoch, &owner, ttl_ms).await?),
        Request::CommitRun(request) => {
            Response::CommitRun(map_commit_outcome(control.commit_run(request).await?))
        }
        Request::GetRun { run_id } => Response::Run(control.get(&run_id).await?),
        Request::ListRuns => Response::Runs(control.list().await?),
        Request::GetAttempt { run_id } => {
            Response::Attempt(attempts.get(&run_id).await?.map(map_attempt_record))
        }
        Request::PublishAttemptActive {
            run_id,
            attempt_id,
            lease_epoch,
            ttl_ms,
        } => Response::Boolean(
            attempts
                .publish_active(&run_id, &attempt_id, lease_epoch, ttl_ms)
                .await?,
        ),
        Request::HeartbeatAttempt {
            run_id,
            attempt_id,
            lease_epoch,
            ttl_ms,
        } => Response::Boolean(
            attempts
                .heartbeat(&run_id, &attempt_id, lease_epoch, ttl_ms)
                .await?,
        ),
        Request::PublishAttemptTerminal {
            run_id,
            attempt_id,
            lease_epoch,
            result,
        } => Response::Boolean(
            attempts
                .publish_terminal(&run_id, &attempt_id, lease_epoch, result)
                .await?,
        ),
        Request::AppendTrajectory(request) => Response::TrajectoryAppend(
            append_trajectory(request)
                .await
                .context("append trajectory")?,
        ),
    })
}

fn map_lease_outcome(
    value: persisting_pchronicle::storage::LeaseAcquireOutcome,
) -> LeaseAcquireOutcome {
    match value {
        persisting_pchronicle::storage::LeaseAcquireOutcome::Acquired(value) => {
            LeaseAcquireOutcome::Acquired(value)
        }
        persisting_pchronicle::storage::LeaseAcquireOutcome::Held(value) => {
            LeaseAcquireOutcome::Held(value)
        }
        persisting_pchronicle::storage::LeaseAcquireOutcome::AlreadyCommitted(value) => {
            LeaseAcquireOutcome::AlreadyCommitted(value)
        }
    }
}

fn map_commit_outcome(value: persisting_pchronicle::storage::CommitRunOutcome) -> CommitRunOutcome {
    match value {
        persisting_pchronicle::storage::CommitRunOutcome::Committed(value) => {
            CommitRunOutcome::Committed(value)
        }
        persisting_pchronicle::storage::CommitRunOutcome::AlreadyCommitted(value) => {
            CommitRunOutcome::AlreadyCommitted(value)
        }
        persisting_pchronicle::storage::CommitRunOutcome::StaleLease {
            supplied_epoch,
            current_epoch,
        } => CommitRunOutcome::StaleLease {
            supplied_epoch,
            current_epoch,
        },
        persisting_pchronicle::storage::CommitRunOutcome::Conflict(value) => {
            CommitRunOutcome::Conflict(value)
        }
    }
}

fn map_attempt_record(value: AttemptRecord) -> ProtocolAttemptRecord {
    ProtocolAttemptRecord {
        revision: value.revision,
        run_id: value.run_id,
        attempt_id: value.attempt_id,
        lease_epoch: value.lease_epoch,
        state: match value.state {
            AttemptRecordState::Active => ProtocolAttemptRecordState::Active,
            AttemptRecordState::Terminal => ProtocolAttemptRecordState::Terminal,
        },
        heartbeat_at_unix_ms: value.heartbeat_at_unix_ms,
        expires_at_unix_ms: value.expires_at_unix_ms,
        terminal_result: value.terminal_result,
    }
}

pub(crate) async fn append_trajectory(
    request: TrajectoryAppendRequest,
) -> Result<TrajectoryAppendResponse> {
    if request.format == TrajectoryFormat::Json {
        return append_json_trajectory(request).await;
    }
    let session = StoryCoords::new(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        request.root_session_id.clone(),
    );
    let store = RawEventLanceStore;
    let accepted_records = request.records.len();
    let note = if request.records.is_empty() {
        "No non-empty records; storage unchanged.".to_string()
    } else {
        let outcome = store.append_events(&session, &request.records).await?;
        format!("canonical Lance event log. {}", outcome.note)
    };
    Ok(TrajectoryAppendResponse {
        dataset: store.display_path(&session)?,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        accepted_records,
        status: "ok".into(),
        note,
    })
}

async fn append_json_trajectory(
    request: TrajectoryAppendRequest,
) -> Result<TrajectoryAppendResponse> {
    let base = format!(
        "{}/json/{}/{}",
        request.storage.trim_end_matches('/'),
        safe_segment(&request.agent_id),
        safe_segment(&request.session_id)
    );
    let location = DatasetLocation::parse(&base)?;
    if let Some(path) = location.local_path() {
        std::fs::create_dir_all(path)?;
        let file_path = path.join("events.jsonl");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;
        for record in &request.records {
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")?;
        }
        file.flush()?;
    } else {
        // Object stores do not provide an atomic append primitive. Keep each
        // event immutable and independently recoverable under the warehouse
        // prefix; the control worker serializes these writes.
        for record in &request.records {
            let uri = format!("{base}/events/{:020}.json", record.seq);
            let location = DatasetLocation::parse(&uri)?;
            location
                .put_bytes(&serde_json::to_vec(record)?, false)
                .await?;
        }
    }
    Ok(TrajectoryAppendResponse {
        dataset: base,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        accepted_records: request.records.len(),
        status: "ok".into(),
        note: "JSON EventRecord warehouse log".into(),
    })
}

fn safe_segment(value: &str) -> String {
    let mut result = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if result.is_empty() || result == "." || result == ".." {
        result = "_unknown".into();
    }
    result.truncate(128);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_events::ChronicleServeControlReady;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn prepared_control_serves_ping_and_stops_on_shutdown() -> Result<()> {
        let storage = tempfile::tempdir()?;
        let prepared = PreparedControl::bind(
            storage.path().to_str().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await?;
        let ChronicleServeControlReady {
            endpoint,
            auth_token,
        } = prepared.ready();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            prepared
                .serve(async {
                    let _ = stop_rx.await;
                })
                .await
        });

        let mut stream = TcpStream::connect(endpoint).await?;
        let request = ChronicleControlEnvelope {
            version: CHRONICLE_CONTROL_VERSION,
            request_id: 7,
            auth_token,
            request: ChronicleControlRequest::Ping,
        };
        let mut encoded = serde_json::to_vec(&request)?;
        encoded.push(b'\n');
        stream.write_all(&encoded).await?;
        stream.flush().await?;

        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).await?;
        let response: ChronicleControlResponseEnvelope = serde_json::from_str(&response)?;
        assert_eq!(response.request_id, 7);
        assert!(matches!(response.response, ChronicleControlResponse::Pong));

        stop_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server).await???;
        Ok(())
    }
}
