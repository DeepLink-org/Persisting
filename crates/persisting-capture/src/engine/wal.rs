//! Event write-ahead log — append-before-apply durability for in-flight events.
//!
//! `spawn_apply` synchronously appends a JSONL line containing the
//! `(CallContext, Event)` pair to `<storage>/.capture/events.wal.jsonl`
//! BEFORE handing the event to [`super::apply_queue::ApplyDispatcher`].
//! After apply succeeds, an `ack` line for the same `seq` is appended.
//! On a clean shutdown the file is truncated.
//!
//! On startup [`replay_pending`] scans the file: events whose `seq` was
//! never acked are returned for replay. This complements
//! [`crate::dead_letter`] (which only catches application-layer failures)
//! by giving us a recovery path for OOM/panic/SIGKILL between
//! `spawn_apply` and `apply` completion.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{CallContext, Event};
use crate::dead_letter::{DeadLetterContext, SerializableEvent};

const WAL_FILENAME: &str = "events.wal.jsonl";

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

/// Append-only WAL file with a process-local monotonic sequence.
pub(crate) struct EventWal {
    path: PathBuf,
    next_seq: AtomicU64,
    writer: Mutex<Option<File>>,
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
                return Self {
                    path,
                    next_seq: AtomicU64::new(0),
                    writer: Mutex::new(Some(file)),
                    enabled: true,
                };
            }
            Err(e) => {
                tracing::warn!(target: "persisting_capture", "wal disabled: {e:#}");
                false
            }
        };
        Self {
            path,
            next_seq: AtomicU64::new(0),
            writer: Mutex::new(None),
            enabled,
        }
    }

    /// Append an event before dispatch. Returns `Some(seq)` on success
    /// (must later be passed to [`Self::ack`]), or `None` if WAL is disabled.
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
        if let Err(e) = self.write_line(&line) {
            tracing::warn!(target: "persisting_capture", "wal append: {e:#}");
            return None;
        }
        Some(seq)
    }

    /// Mark a previously enqueued event as applied.
    pub fn ack(&self, seq: u64) {
        if !self.enabled {
            return;
        }
        if let Err(e) = self.write_line(&WalLine::Ack { seq }) {
            tracing::warn!(target: "persisting_capture", "wal ack: {e:#}");
        }
    }

    /// Truncate the WAL — call this on clean shutdown after every event has been applied.
    pub fn truncate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut guard = self.writer.lock().expect("wal writer mutex");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .with_context(|| format!("truncate wal {}", self.path.display()))?;
        *guard = Some(file);
        Ok(())
    }

    fn write_line(&self, line: &WalLine) -> Result<()> {
        let payload = serde_json::to_string(line).context("serialize wal line")?;
        let mut guard = self.writer.lock().expect("wal writer mutex");
        let file = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("wal not open"))?;
        writeln!(file, "{payload}").context("append wal line")?;
        file.sync_data().context("fsync wal")?;
        Ok(())
    }
}

fn prepare_writer(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open wal {}", path.display()))
}

/// One unacked entry recovered from the WAL.
#[derive(Debug, Clone)]
pub(crate) struct PendingEntry {
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
            tracing::warn!(target: "persisting_capture", "wal open: {e:#}");
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
                tracing::warn!(target: "persisting_capture", "wal read line {i}: {e:#}");
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
                events.insert(seq, PendingEntry { context, event });
            }
            Ok(WalLine::Ack { seq }) => {
                acked.insert(seq);
            }
            Err(e) => {
                tracing::warn!(target: "persisting_capture", "wal parse line {i}: {e:#}");
            }
        }
    }

    events
        .into_iter()
        .filter_map(|(seq, entry)| (!acked.contains(&seq)).then_some(entry))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::engine::RequestEvent;
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::session_storage::CaptureRoute;
    use crate::Call;

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
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            model_rewritten: false,
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
}
