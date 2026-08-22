use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde_json::{Map, Value};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

pub struct Journal {
    lock: File,
    file: File,
    pub path: PathBuf,
}

impl Journal {
    pub fn open(state_dir: &Path) -> Result<Self, ReplayError> {
        fs::create_dir_all(state_dir).replay_context(
            ReplayErrorKind::Executor,
            format!("create replay state {}", state_dir.display()),
        )?;
        let lock_path = state_dir.join("run.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .replay_context(
                ReplayErrorKind::Executor,
                format!("open {}", lock_path.display()),
            )?;
        lock.try_lock_exclusive().map_err(|error| {
            ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                format!(
                    "another replay process owns {}: {error}",
                    lock_path.display()
                ),
            )
        })?;
        let path = state_dir.join("replay-events.jsonl");
        if let Some(call_id) = Self::find_ambiguous(&path)? {
            return Err(ReplayError::new(
                ReplayErrorKind::AmbiguousExecution,
                format!(
                    "state contains an uncertain started tool call {call_id:?}; use a new sandbox and run-id to replay from T1"
                ),
            ));
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .replay_context(
                ReplayErrorKind::Executor,
                format!("open {}", path.display()),
            )?;
        Ok(Self { lock, file, path })
    }

    pub fn append(
        &mut self,
        event: &str,
        fields: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), ReplayError> {
        let mut object = Map::new();
        object.insert("event".into(), Value::String(event.into()));
        object.insert(
            "timestamp_ns".into(),
            Value::Number(
                chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .unwrap_or_default()
                    .into(),
            ),
        );
        object.extend(fields);
        serde_json::to_writer(&mut self.file, &Value::Object(object))
            .replay_context(ReplayErrorKind::Executor, "append replay journal")?;
        self.file
            .write_all(b"\n")
            .and_then(|_| self.file.sync_data())
            .replay_context(ReplayErrorKind::Executor, "flush replay journal")
    }

    pub fn find_ambiguous(path: &Path) -> Result<Option<String>, ReplayError> {
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(path).replay_context(
            ReplayErrorKind::AmbiguousExecution,
            format!("read {}", path.display()),
        )?;
        let mut started_since_terminal = BTreeSet::new();
        for line in BufReader::new(file).lines() {
            let line =
                line.replay_context(ReplayErrorKind::AmbiguousExecution, "read replay journal")?;
            let event: Value = serde_json::from_str(&line)
                .replay_context(ReplayErrorKind::AmbiguousExecution, "parse replay journal")?;
            match event.get("event").and_then(Value::as_str) {
                Some("tool_started") => {
                    if let Some(call_id) = event.get("call_id").and_then(Value::as_str) {
                        started_since_terminal.insert(call_id.to_owned());
                    }
                }
                Some("run_finished" | "run_failed") => started_since_terminal.clear(),
                _ => {}
            }
        }
        Ok(started_since_terminal.into_iter().next())
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.file.sync_data();
        let _ = FileExt::unlock(&self.lock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_events(path: &Path, events: &[Value]) {
        let contents = events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{contents}\n")).unwrap();
    }

    #[test]
    fn finished_tool_without_terminal_run_is_ambiguous() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
                serde_json::json!({"event": "tool_finished", "call_id": "same-call"}),
            ],
        );

        assert_eq!(
            Journal::find_ambiguous(&path).unwrap().as_deref(),
            Some("same-call")
        );
    }

    #[test]
    fn repeated_call_id_after_a_terminal_run_remains_ambiguous() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
                serde_json::json!({"event": "tool_finished", "call_id": "same-call"}),
                serde_json::json!({"event": "run_finished"}),
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "same-call"}),
            ],
        );

        assert_eq!(
            Journal::find_ambiguous(&path).unwrap().as_deref(),
            Some("same-call")
        );
    }

    #[test]
    fn interruption_before_any_tool_starts_is_retryable() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "plan_validated"}),
            ],
        );

        assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
    }

    #[test]
    fn failed_run_is_terminal() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "call-1"}),
                serde_json::json!({"event": "run_failed"}),
            ],
        );

        assert_eq!(Journal::find_ambiguous(&path).unwrap(), None);
    }

    #[test]
    fn opening_a_journal_rejects_ambiguous_state_while_holding_the_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("replay-events.jsonl");
        write_events(
            &path,
            &[
                serde_json::json!({"event": "run_started"}),
                serde_json::json!({"event": "tool_started", "call_id": "call-1"}),
            ],
        );

        let error = Journal::open(temporary.path()).err().unwrap();

        assert_eq!(error.kind, ReplayErrorKind::AmbiguousExecution);
        assert!(error.message.contains("call-1"));
    }
}
