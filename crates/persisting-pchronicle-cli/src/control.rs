//! pChronicle's write-capable control plane.
//!
//! The Warehouse HTTP server remains read-only. This long-lived JSONL/stdin
//! protocol is intended for trusted local orchestrators such as pPilot.

use anyhow::{Context, Result};
use persisting_pchronicle::{AttemptRegistry, RunControlStore};
use persisting_pchronicle_client::{
    ChronicleControlEnvelope, ChronicleControlReady, ChronicleControlRequest,
    ChronicleControlResponse, ChronicleControlResponseEnvelope, CommitRunOutcome,
    LeaseAcquireOutcome, CHRONICLE_CONTROL_MAX_FRAME_BYTES, CHRONICLE_CONTROL_VERSION,
};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub(super) async fn run_control(
    storage: &str,
    listen: SocketAddr,
    stdout: &mut dyn Write,
) -> Result<()> {
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
    let endpoint = listener.local_addr()?;
    let auth_token = uuid::Uuid::new_v4().simple().to_string();
    serde_json::to_writer(
        &mut *stdout,
        &ChronicleControlReady {
            version: CHRONICLE_CONTROL_VERSION,
            endpoint: endpoint.to_string(),
            auth_token: auth_token.clone(),
        },
    )?;
    writeln!(stdout)?;
    stdout.flush()?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accept pChronicle control client")?;
        stream
            .set_nodelay(true)
            .context("configure pChronicle control socket")?;
        let control = Arc::clone(&control);
        let attempts = Arc::clone(&attempts);
        let auth_token = auth_token.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, control, attempts, auth_token).await {
                eprintln!("pChronicle control request failed: {error:#}");
            }
        });
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
            Response::Attempt(transcode(attempts.get(&run_id).await?)?)
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
        Request::AppendTrajectory(request) => {
            let request = transcode(request)?;
            let response = persisting_pchronicle::operations::trajectory::append_async(request)
                .await
                .context("append trajectory")?;
            Response::TrajectoryAppend(transcode(response)?)
        }
    })
}

fn map_lease_outcome(value: persisting_pchronicle::LeaseAcquireOutcome) -> LeaseAcquireOutcome {
    match value {
        persisting_pchronicle::LeaseAcquireOutcome::Acquired(value) => {
            LeaseAcquireOutcome::Acquired(value)
        }
        persisting_pchronicle::LeaseAcquireOutcome::Held(value) => LeaseAcquireOutcome::Held(value),
        persisting_pchronicle::LeaseAcquireOutcome::AlreadyCommitted(value) => {
            LeaseAcquireOutcome::AlreadyCommitted(value)
        }
    }
}

fn map_commit_outcome(value: persisting_pchronicle::CommitRunOutcome) -> CommitRunOutcome {
    match value {
        persisting_pchronicle::CommitRunOutcome::Committed(value) => {
            CommitRunOutcome::Committed(value)
        }
        persisting_pchronicle::CommitRunOutcome::AlreadyCommitted(value) => {
            CommitRunOutcome::AlreadyCommitted(value)
        }
        persisting_pchronicle::CommitRunOutcome::StaleLease {
            supplied_epoch,
            current_epoch,
        } => CommitRunOutcome::StaleLease {
            supplied_epoch,
            current_epoch,
        },
        persisting_pchronicle::CommitRunOutcome::Conflict(value) => {
            CommitRunOutcome::Conflict(value)
        }
    }
}

fn transcode<T: serde::Serialize, U: serde::de::DeserializeOwned>(value: T) -> Result<U> {
    serde_json::from_value(serde_json::to_value(value)?).context("translate control protocol value")
}
