//! Event write-ahead log — append-before-apply durability for in-flight events.
//!
//! `spawn_apply` submits a JSONL `(CallContext, Event)` record to the bounded
//! writer queue before handing the event to [`super::apply_queue::ApplyDispatcher`].
//! Submission never waits for filesystem I/O; accepted records are committed
//! in the background and may be lost if the process crashes inside that window.
//! After apply succeeds, an `ack` line for the same `seq` is appended.
//! On a clean shutdown the file is truncated only when every event is acked;
//! failed durable writes remain pending for restart replay.
//!
//! On startup [`replay_pending`] scans the file: events whose `seq` was
//! never acked are returned for replay. This complements
//! [`crate::dead_letter`] (which only catches application-layer failures)
//! by giving us a recovery path for OOM/panic/SIGKILL between
//! `spawn_apply` and `apply` completion.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{CallContext, Event};
use crate::dead_letter::{DeadLetterContext, SerializableEvent};

const WAL_FILENAME: &str = "events.wal.jsonl";
/// Maximum time the writer deliberately waits for peers to join a commit.
/// Disk scheduling can add latency beyond this coalescing window.
const GROUP_COMMIT_MAX_DELAY: Duration = Duration::from_millis(2);
const GROUP_COMMIT_MAX_LINES: usize = 256;
const WAL_QUEUE_CAPACITY: usize = 256;

pub fn wal_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join(WAL_FILENAME)
}

/// Payload of a WAL `Event` line — split out so the larger variant doesn't bloat
/// the `Ack`-only path through `WalLine`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalEventBody {
    seq: u64,
    timestamp: String,
    context: DeadLetterContext,
    event: SerializableEvent,
}

/// One line in the WAL — either an enqueued event or an ack for a previously enqueued one.
/// `Event` carries its body via a `Box` so the enum's stack footprint is dominated by `Ack`,
/// which is by far the more common variant on the hot path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WalLine {
    Event {
        #[serde(flatten)]
        body: Box<WalEventBody>,
    },
    Ack {
        seq: u64,
    },
}

type ControlResult = std::result::Result<(), String>;
type ControlCompletion = mpsc::SyncSender<ControlResult>;

enum WalCommand {
    Line(WalLine),
    Flush { completion: ControlCompletion },
    Truncate { completion: ControlCompletion },
    Shutdown,
}

/// Append-only WAL with a process-local monotonic sequence and a dedicated
/// bounded-delay group-commit writer.
///
/// Event and ACK submissions are both best-effort and asynchronous. Once a line
/// reaches the writer it is protected by group commit, while queue saturation or
/// a crash before the next commit may lose the WAL copy. Canonical capture keeps
/// its independent apply/Lance durability path.
pub(crate) struct EventWal {
    next_seq: AtomicU64,
    sender: Option<mpsc::SyncSender<WalCommand>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    sync_count: Arc<AtomicU64>,
    enabled: bool,
}

impl EventWal {
    /// Open (or create) the WAL at `<storage>/.capture/events.wal.jsonl`.
    /// On any I/O error the WAL falls back to disabled mode and the proxy keeps running
    /// without durability — capture is best-effort, not critical-path.
    pub fn open(storage: &Path) -> Self {
        let path = wal_path(storage);
        let enabled = match prepare_writer(&path) {
            Ok(file) => {
                let next_seq = next_sequence(&path);
                let (sender, receiver) = mpsc::sync_channel(WAL_QUEUE_CAPACITY);
                let writer_path = path.clone();
                let sync_count = Arc::new(AtomicU64::new(0));
                let worker_sync_count = Arc::clone(&sync_count);
                match std::thread::Builder::new()
                    .name("persisting-wal-commit".to_string())
                    .spawn(move || {
                        group_commit_loop(file, &writer_path, receiver, worker_sync_count)
                    }) {
                    Ok(worker) => {
                        return Self {
                            next_seq: AtomicU64::new(next_seq),
                            sender: Some(sender),
                            worker: Mutex::new(Some(worker)),
                            #[cfg(test)]
                            sync_count,
                            enabled: true,
                        };
                    }
                    Err(e) => {
                        tracing::warn!(target: "persisting_gateway", "wal writer disabled: {e}");
                        false
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "persisting_gateway", "wal disabled: {e:#}");
                false
            }
        };
        Self {
            next_seq: AtomicU64::new(0),
            sender: None,
            worker: Mutex::new(None),
            #[cfg(test)]
            sync_count: Arc::new(AtomicU64::new(0)),
            enabled,
        }
    }

    /// Submit an event before dispatch without waiting for queue capacity or I/O.
    /// Returns `Some(seq)` when accepted (must later be passed to [`Self::ack`]),
    /// or `None` when WAL is disabled, full, or closed.
    pub fn append_event(&self, ctx: &CallContext, event: &Event) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let line = WalLine::Event {
            body: Box::new(WalEventBody {
                seq,
                timestamp: chrono::Utc::now().to_rfc3339(),
                context: DeadLetterContext::from_context(ctx),
                event: SerializableEvent::from_event(event),
            }),
        };
        let Some(sender) = &self.sender else {
            return None;
        };
        match sender.try_send(WalCommand::Line(line)) {
            Ok(()) => Some(seq),
            Err(e) => {
                tracing::warn!(target: "persisting_gateway", "wal append: {e}");
                None
            }
        }
    }

    /// Mark a previously enqueued event as applied.
    pub fn ack(&self, seq: u64) {
        if !self.enabled {
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
        if let Err(e) = sender.try_send(WalCommand::Line(WalLine::Ack { seq })) {
            tracing::warn!(target: "persisting_gateway", "wal ack: {e}");
        }
    }

    /// Wait until every line submitted before this call has completed its group
    /// commit. Required before inspecting or truncating the WAL during shutdown.
    pub fn flush(&self) -> Result<()> {
        self.control(|completion| WalCommand::Flush { completion })
    }

    /// Truncate the WAL after verifying that no unacknowledged event remains.
    pub fn truncate(&self) -> Result<()> {
        self.control(|completion| WalCommand::Truncate { completion })
    }

    fn control(&self, command: impl FnOnce(ControlCompletion) -> WalCommand) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let (completion, completed) = mpsc::sync_channel(1);
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wal not open"))?;
        sender
            .send(command(completion))
            .map_err(|_| anyhow::anyhow!("wal writer stopped"))?;
        completed
            .recv()
            .map_err(|_| anyhow::anyhow!("wal writer dropped control result"))?
            .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    fn sync_count(&self) -> u64 {
        self.sync_count.load(Ordering::Relaxed)
    }
}

impl Drop for EventWal {
    fn drop(&mut self) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(WalCommand::Shutdown);
        }
        if let Some(worker) = self.worker.lock().expect("wal worker mutex").take()
            && worker.join().is_err()
        {
            tracing::warn!(target: "persisting_gateway", "wal writer thread panicked");
        }
    }
}

fn group_commit_loop(
    mut file: File,
    path: &Path,
    receiver: mpsc::Receiver<WalCommand>,
    sync_count: Arc<AtomicU64>,
) {
    let mut deferred = None;
    loop {
        let command = match deferred.take().or_else(|| receiver.recv().ok()) {
            Some(command) => command,
            None => return,
        };
        match command {
            WalCommand::Line(line) => {
                let deadline = Instant::now() + GROUP_COMMIT_MAX_DELAY;
                let mut batch = vec![line];
                let mut disconnected = false;
                while batch.len() < GROUP_COMMIT_MAX_LINES {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match receiver.recv_timeout(remaining) {
                        Ok(WalCommand::Line(line)) => batch.push(line),
                        Ok(command) => {
                            deferred = Some(command);
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                commit_lines(&mut file, batch, &sync_count);
                if disconnected {
                    return;
                }
            }
            WalCommand::Flush { completion } => {
                // FIFO command ordering means all preceding line batches have
                // already completed `sync_data` when this barrier is observed.
                let _ = completion.send(Ok(()));
            }
            WalCommand::Truncate { completion } => {
                let result = (|| -> Result<File> {
                    let truncated = crate::runtime::open_private_truncate_file(path)
                        .with_context(|| format!("truncate wal {}", path.display()))?;
                    truncated.sync_data().context("fsync truncated wal")?;
                    Ok(truncated)
                })();
                match result {
                    Ok(truncated) => {
                        file = truncated;
                        sync_count.fetch_add(1, Ordering::Relaxed);
                        let _ = completion.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = completion.send(Err(format!("{error:#}")));
                    }
                }
            }
            WalCommand::Shutdown => return,
        }
    }
}

fn commit_lines(file: &mut File, batch: Vec<WalLine>, sync_count: &AtomicU64) {
    let result = (|| -> Result<()> {
        for line in &batch {
            serde_json::to_writer(&mut *file, line).context("serialize wal line")?;
            file.write_all(b"\n").context("append wal newline")?;
        }
        file.sync_data().context("fsync wal")?;
        Ok(())
    })();
    let commit = sync_count.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::trace!(
        target: "persisting_gateway",
        commit,
        lines = batch.len(),
        "wal group commit"
    );
    if let Err(error) = &result {
        tracing::warn!(
            target: "persisting_gateway",
            lines = batch.len(),
            "wal group commit failed: {error}"
        );
    }
}

fn prepare_writer(path: &Path) -> Result<File> {
    crate::runtime::open_private_append_file(path)
        .with_context(|| format!("open wal {}", path.display()))
}

/// One unacked entry recovered from the WAL.
#[derive(Debug, Clone)]
pub(crate) struct PendingEntry {
    pub seq: u64,
    pub context: DeadLetterContext,
    pub event: SerializableEvent,
}

/// Read the WAL and return events that were appended but never acked.
/// Missing or corrupt files are treated as "no pending events" with a warning.
pub(crate) fn replay_pending(storage: &Path) -> Vec<PendingEntry> {
    let path = wal_path(storage);
    if !path.exists() {
        return Vec::new();
    }
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(target: "persisting_gateway", "wal open: {e:#}");
            return Vec::new();
        }
    };

    let mut events: std::collections::BTreeMap<u64, PendingEntry> =
        std::collections::BTreeMap::new();
    let mut acked: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(target: "persisting_gateway", "wal read line {i}: {e:#}");
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<WalLine>(trimmed) {
            Ok(WalLine::Event { body }) => {
                let WalEventBody {
                    seq,
                    context,
                    event,
                    ..
                } = *body;
                events.insert(
                    seq,
                    PendingEntry {
                        seq,
                        context,
                        event,
                    },
                );
            }
            Ok(WalLine::Ack { seq }) => {
                acked.insert(seq);
            }
            Err(e) => {
                tracing::warn!(target: "persisting_gateway", "wal parse line {i}: {e:#}");
            }
        }
    }

    events
        .into_iter()
        .filter_map(|(seq, entry)| (!acked.contains(&seq)).then_some(entry))
        .collect()
}

fn next_sequence(path: &Path) -> u64 {
    let Ok(file) = File::open(path) else {
        return 0;
    };
    BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|line| serde_json::from_str::<WalLine>(line.trim()).ok())
        .map(|line| match line {
            WalLine::Event { body } => body.seq,
            WalLine::Ack { seq } => seq,
        })
        .max()
        .map_or(0, |seq| seq.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::engine::RequestEvent;
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::session::storage::CaptureRoute;

    fn sample_ctx() -> CallContext {
        CallContext::new(
            CaptureRoute {
                root_session: Some("run-1".into()),
                session_id: "sess".into(),
                storage_session_id: "run-1".into(),
                subagent_id: None,
            },
            "agent",
            Call {
                call_id: "c1".into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            Vec::new(),
            CaptureLevel::Dialogue,
            "m",
            "m",
            ProviderKind::OpenAi,
            ProtocolKind::ChatCompletions,
            false,
        )
    }

    fn sample_event() -> Event {
        Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: None,
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            semantic: None,
            model_rewritten: false,
            headers: vec![],
        })
    }

    #[test]
    fn wal_replays_unacked_only() {
        let dir = tempfile::tempdir().unwrap();
        let wal = EventWal::open(dir.path());

        let ctx = sample_ctx();
        let event = sample_event();
        let s1 = wal.append_event(&ctx, &event).expect("seq");
        let s2 = wal.append_event(&ctx, &event).expect("seq");
        wal.ack(s1);
        drop(wal);

        let pending = replay_pending(dir.path());
        assert_eq!(pending.len(), 1, "expected only s2 to be pending");
        assert_eq!(s2, 1);
    }

    #[test]
    fn writer_commits_queued_lines_with_one_fsync() {
        let dir = tempfile::tempdir().unwrap();
        let path = wal_path(dir.path());
        let file = prepare_writer(&path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(WAL_QUEUE_CAPACITY);
        let sync_count = Arc::new(AtomicU64::new(0));
        for seq in 0..16 {
            sender.send(WalCommand::Line(WalLine::Ack { seq })).unwrap();
        }
        let (completion, completed) = mpsc::sync_channel(1);
        sender.send(WalCommand::Flush { completion }).unwrap();
        sender.send(WalCommand::Shutdown).unwrap();

        let writer_path = path.clone();
        let writer_sync_count = Arc::clone(&sync_count);
        let worker = std::thread::spawn(move || {
            group_commit_loop(file, &writer_path, receiver, writer_sync_count)
        });
        completed.recv().unwrap().unwrap();
        worker.join().unwrap();

        assert_eq!(sync_count.load(Ordering::Relaxed), 1);
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 16);
    }

    #[test]
    fn flush_makes_async_ack_visible_before_pending_check() {
        let dir = tempfile::tempdir().unwrap();
        let wal = EventWal::open(dir.path());
        let seq = wal.append_event(&sample_ctx(), &sample_event()).unwrap();
        wal.ack(seq);
        wal.flush().unwrap();

        assert!(replay_pending(dir.path()).is_empty());
        assert!(wal.sync_count() >= 1);
    }

    #[test]
    fn truncate_clears_unacked() {
        let dir = tempfile::tempdir().unwrap();
        let wal = EventWal::open(dir.path());
        wal.append_event(&sample_ctx(), &sample_event()).unwrap();
        wal.truncate().unwrap();
        drop(wal);

        let pending = replay_pending(dir.path());
        assert!(pending.is_empty());
    }

    #[test]
    fn missing_file_replays_empty() {
        let dir = tempfile::tempdir().unwrap();
        let pending = replay_pending(dir.path());
        assert!(pending.is_empty());
    }

    #[test]
    fn reopen_continues_sequence_after_existing_entries() {
        let dir = tempfile::tempdir().unwrap();
        let first = EventWal::open(dir.path());
        assert_eq!(first.append_event(&sample_ctx(), &sample_event()), Some(0));
        drop(first);

        let reopened = EventWal::open(dir.path());
        assert_eq!(
            reopened.append_event(&sample_ctx(), &sample_event()),
            Some(1)
        );
    }

    #[test]
    fn wal_retains_original_client_request_before_protocol_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let original_bytes = bytes::Bytes::from_static(
            br#"{"model":"claude-client","max_tokens":16,"messages":[{"role":"user","content":"original"}]}"#,
        );
        let original_json: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
        let converted =
            crate::conversion::messages_request_to_completions(&original_bytes, "upstream-model")
                .unwrap();
        let converted_json: serde_json::Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(converted_json["model"], "upstream-model");

        let wal = EventWal::open(dir.path());
        let semantic =
            crate::understanding::understand_request(ProtocolKind::Messages, &original_bytes)
                .unwrap()
                .semantic;
        wal.append_event(
            &sample_ctx(),
            &Event::Request(RequestEvent {
                path: "/v1/messages".into(),
                method: "POST".into(),
                url: Some("//gateway/v1/messages".into()),
                body_bytes: original_bytes.len(),
                user_content: Some("original".into()),
                body_json: Some(original_json.clone()),
                semantic: Some(semantic),
                model_rewritten: true,
                headers: vec![("x-request-id".into(), "req-1".into())],
            }),
        )
        .unwrap();
        drop(wal);

        let serialized = std::fs::read_to_string(wal_path(dir.path())).unwrap();
        assert!(serialized.contains("body_json"));
        assert!(
            !serialized.contains("\"semantic\""),
            "WAL must not duplicate the typed payload: {serialized}"
        );

        let pending = replay_pending(dir.path());
        assert_eq!(pending.len(), 1);
        let Event::Request(replayed) = pending[0].event.to_event() else {
            panic!("expected request event")
        };
        assert_eq!(replayed.path, "/v1/messages");
        assert_eq!(replayed.url.as_deref(), Some("//gateway/v1/messages"));
        assert_eq!(replayed.body_json, Some(original_json));
        assert_eq!(replayed.headers[0], ("x-request-id".into(), "req-1".into()));
    }

    #[test]
    fn wal_redacts_credentials_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = sample_ctx();
        ctx.request_headers = vec![
            ("authorization".into(), "Bearer context-secret".into()),
            ("x-request-id".into(), "req-safe".into()),
        ];
        ctx.upstream_url = Some("https://upstream.example/v1?key=url-secret".into());
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
            method: "POST".into(),
            url: Some("//gateway.example/v1?api_key=request-url-secret".into()),
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: Some(serde_json::json!({
                "apiKey": "body-secret",
                "safe": "kept"
            })),
            semantic: None,
            model_rewritten: false,
            headers: vec![("x-api-key".into(), "event-secret".into())],
        });

        let wal = EventWal::open(dir.path());
        wal.append_event(&ctx, &event).unwrap();
        drop(wal);

        let serialized = std::fs::read_to_string(wal_path(dir.path())).unwrap();
        for secret in [
            "context-secret",
            "event-secret",
            "body-secret",
            "url-secret",
            "request-url-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "WAL persisted credential {secret}: {serialized}"
            );
        }
        assert!(serialized.contains("req-safe"));
        assert!(serialized.contains("kept"));
        assert!(serialized.contains("<redacted>"));

        let pending = replay_pending(dir.path());
        assert_eq!(pending[0].context.request_headers[0].1, "<redacted>");
        let Event::Request(replayed) = pending[0].event.to_event() else {
            panic!("expected request event")
        };
        assert_eq!(replayed.headers[0].1, "<redacted>");
        assert_eq!(replayed.body_json.unwrap()["apiKey"], "<redacted>");
    }

    #[cfg(unix)]
    #[test]
    fn wal_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let wal = EventWal::open(dir.path());
        wal.append_event(&sample_ctx(), &sample_event()).unwrap();
        drop(wal);

        let capture_dir = dir.path().join(".capture");
        assert_eq!(
            std::fs::metadata(&capture_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(wal_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
