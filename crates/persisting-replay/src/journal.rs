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
        let mut started = BTreeSet::new();
        let mut finished = BTreeSet::new();
        for line in BufReader::new(file).lines() {
            let line =
                line.replay_context(ReplayErrorKind::AmbiguousExecution, "read replay journal")?;
            let event: Value = serde_json::from_str(&line)
                .replay_context(ReplayErrorKind::AmbiguousExecution, "parse replay journal")?;
            if let Some(call_id) = event.get("call_id").and_then(Value::as_str) {
                match event.get("event").and_then(Value::as_str) {
                    Some("tool_started") => {
                        started.insert(call_id.to_owned());
                    }
                    Some("tool_finished") => {
                        finished.insert(call_id.to_owned());
                    }
                    _ => {}
                }
            }
        }
        Ok(started.difference(&finished).next().cloned())
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.file.sync_data();
        let _ = FileExt::unlock(&self.lock);
    }
}
