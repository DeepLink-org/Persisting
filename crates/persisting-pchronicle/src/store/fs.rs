//! Filesystem JSONL store for the three ATIF tables.
//!
//! Layout:
//! ```text
//! {root}/
//!   sessions.jsonl
//!   steps.jsonl
//!   tool_calls.jsonl
//! ```

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::schema::{tables, SessionRow, StepRow, ToolCallRow};
use crate::store::NormalizedStore;
use crate::Result;

#[derive(Debug, Clone)]
pub struct FsChronicleStore {
    root: PathBuf,
    sessions: BTreeMap<String, SessionRow>,
    steps: BTreeMap<String, Vec<StepRow>>,
    tool_calls: BTreeMap<String, Vec<ToolCallRow>>,
}

const SNAPSHOT_FILE: &str = ".chronicle.snapshot.json";

#[derive(Debug, Serialize, Deserialize)]
struct StoreSnapshot {
    version: u32,
    sessions: Vec<SessionRow>,
    steps: Vec<StepRow>,
    tool_calls: Vec<ToolCallRow>,
}

impl FsChronicleStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let mut store = Self {
            root,
            sessions: BTreeMap::new(),
            steps: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
        };
        store.reload()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.jsonl"))
    }

    fn reload(&mut self) -> Result<()> {
        self.sessions.clear();
        self.steps.clear();
        self.tool_calls.clear();

        let snapshot_path = self.root.join(SNAPSHOT_FILE);
        let (sessions, steps, tool_calls) = if snapshot_path.exists() {
            let snapshot: StoreSnapshot =
                serde_json::from_reader(BufReader::new(File::open(&snapshot_path)?))?;
            (snapshot.sessions, snapshot.steps, snapshot.tool_calls)
        } else {
            (
                read_jsonl::<SessionRow>(&self.path(tables::SESSIONS))?,
                read_jsonl::<StepRow>(&self.path(tables::STEPS))?,
                read_jsonl::<ToolCallRow>(&self.path(tables::TOOL_CALLS))?,
            )
        };
        for row in sessions {
            self.sessions.insert(row.session_id.clone(), row);
        }
        for row in steps {
            self.steps
                .entry(row.session_id.clone())
                .or_default()
                .push(row);
        }
        for rows in self.steps.values_mut() {
            rows.sort_by_key(|r| r.step_id);
        }
        for row in tool_calls {
            self.tool_calls
                .entry(row.session_id.clone())
                .or_default()
                .push(row);
        }
        for rows in self.tool_calls.values_mut() {
            rows.sort_by(|a, b| {
                a.step_id
                    .cmp(&b.step_id)
                    .then(a.tool_call_id.cmp(&b.tool_call_id))
            });
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let sessions = self.sessions.values().cloned().collect::<Vec<_>>();
        let mut all_steps = Vec::new();
        for rows in self.steps.values() {
            all_steps.extend(rows.iter().cloned());
        }
        all_steps.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
        });
        let mut all_calls = Vec::new();
        for rows in self.tool_calls.values() {
            all_calls.extend(rows.iter().cloned());
        }
        all_calls.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
                .then(a.tool_call_id.cmp(&b.tool_call_id))
        });
        let snapshot = StoreSnapshot {
            version: 1,
            sessions: sessions.clone(),
            steps: all_steps.clone(),
            tool_calls: all_calls.clone(),
        };
        write_json_atomic(&self.root.join(SNAPSHOT_FILE), &snapshot)?;

        // JSONL tables are compatibility projections; the atomic snapshot above is
        // authoritative for reopen/recovery.
        let _ = write_jsonl_atomic(&self.path(tables::SESSIONS), &sessions);
        let _ = write_jsonl_atomic(&self.path(tables::STEPS), &all_steps);
        let _ = write_jsonl_atomic(&self.path(tables::TOOL_CALLS), &all_calls);
        Ok(())
    }
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row = serde_json::from_str::<T>(&line).map_err(|e| {
            crate::Error::Other(format!("parse {} line {}: {e}", path.display(), idx + 1))
        })?;
        out.push(row);
    }
    Ok(out)
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_extension(format!("tmp-{}-{nonce}", std::process::id()))
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temporary_path(path);
    let mut file = File::create(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    sync_parent(path)?;
    Ok(())
}

fn write_jsonl_atomic<T: serde::Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temporary_path(path);
    let mut file = File::create(&temp)?;
    for row in rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    fs::rename(&temp, path)?;
    sync_parent(path)?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

impl NormalizedStore for FsChronicleStore {
    fn upsert_session(&mut self, row: SessionRow) -> Result<()> {
        let key = row.session_id.clone();
        let old = self.sessions.insert(key.clone(), row);
        if let Err(error) = self.persist() {
            match old {
                Some(row) => {
                    self.sessions.insert(key, row);
                }
                None => {
                    self.sessions.remove(&key);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn get_session(&self, session_id: &str) -> Result<Option<SessionRow>> {
        Ok(self.sessions.get(session_id).cloned())
    }

    fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        Ok(self.sessions.values().cloned().collect())
    }

    fn replace_steps(&mut self, session_id: &str, mut rows: Vec<StepRow>) -> Result<()> {
        for row in &rows {
            if row.session_id != session_id {
                return Err(crate::Error::Other(format!(
                    "step session_id {} does not match {}",
                    row.session_id, session_id
                )));
            }
        }
        rows.sort_by_key(|r| r.step_id);
        let key = session_id.to_string();
        let old = self.steps.insert(key.clone(), rows);
        if let Err(error) = self.persist() {
            match old {
                Some(rows) => {
                    self.steps.insert(key, rows);
                }
                None => {
                    self.steps.remove(&key);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn list_steps(&self, session_id: &str) -> Result<Vec<StepRow>> {
        Ok(self.steps.get(session_id).cloned().unwrap_or_default())
    }

    fn replace_tool_calls(&mut self, session_id: &str, mut rows: Vec<ToolCallRow>) -> Result<()> {
        let steps = self.list_steps(session_id)?;
        let step_ids: std::collections::HashSet<i64> = steps.iter().map(|s| s.step_id).collect();
        for row in &rows {
            if row.session_id != session_id {
                return Err(crate::Error::Other(format!(
                    "tool_call session_id {} does not match {}",
                    row.session_id, session_id
                )));
            }
            if !step_ids.contains(&row.step_id) {
                return Err(crate::Error::OrphanToolCall {
                    session_id: session_id.to_string(),
                    step_id: row.step_id,
                    tool_call_id: row.tool_call_id.clone(),
                });
            }
        }
        rows.sort_by(|a, b| {
            a.step_id
                .cmp(&b.step_id)
                .then(a.tool_call_id.cmp(&b.tool_call_id))
        });
        let key = session_id.to_string();
        let old = self.tool_calls.insert(key.clone(), rows);
        if let Err(error) = self.persist() {
            match old {
                Some(rows) => {
                    self.tool_calls.insert(key, rows);
                }
                None => {
                    self.tool_calls.remove(&key);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn list_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCallRow>> {
        Ok(self.tool_calls.get(session_id).cloned().unwrap_or_default())
    }

    fn replace_trajectory(&mut self, split: crate::ingest::SplitTables) -> Result<()> {
        let backup = (
            self.sessions.clone(),
            self.steps.clone(),
            self.tool_calls.clone(),
        );
        let session_id = split.session.session_id.clone();
        let mut steps = split.steps;
        steps.sort_by_key(|row| row.step_id);
        let step_ids: std::collections::HashSet<_> = steps.iter().map(|row| row.step_id).collect();
        let mut calls = split.tool_calls;
        for row in &calls {
            if row.session_id != session_id || !step_ids.contains(&row.step_id) {
                return Err(crate::Error::OrphanToolCall {
                    session_id,
                    step_id: row.step_id,
                    tool_call_id: row.tool_call_id.clone(),
                });
            }
        }
        calls.sort_by(|a, b| {
            a.step_id
                .cmp(&b.step_id)
                .then(a.tool_call_id.cmp(&b.tool_call_id))
        });
        self.sessions.insert(session_id.clone(), split.session);
        self.steps.insert(session_id.clone(), steps);
        self.tool_calls.insert(session_id, calls);
        if let Err(error) = self.persist() {
            (self.sessions, self.steps, self.tool_calls) = backup;
            return Err(error);
        }
        Ok(())
    }
}
