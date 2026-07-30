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

use crate::schema::{tables, SessionRow, StepRow, ToolCallRow};
use crate::store::ChronicleStore;
use crate::Result;

#[derive(Debug, Clone)]
pub struct FsChronicleStore {
    root: PathBuf,
    sessions: BTreeMap<String, SessionRow>,
    steps: BTreeMap<String, Vec<StepRow>>,
    tool_calls: BTreeMap<String, Vec<ToolCallRow>>,
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

        for row in read_jsonl::<SessionRow>(&self.path(tables::SESSIONS))? {
            self.sessions.insert(row.session_id.clone(), row);
        }
        for row in read_jsonl::<StepRow>(&self.path(tables::STEPS))? {
            self.steps
                .entry(row.session_id.clone())
                .or_default()
                .push(row);
        }
        for rows in self.steps.values_mut() {
            rows.sort_by_key(|r| r.step_id);
        }
        for row in read_jsonl::<ToolCallRow>(&self.path(tables::TOOL_CALLS))? {
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
        write_jsonl(
            &self.path(tables::SESSIONS),
            self.sessions
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .as_slice(),
        )?;
        let mut all_steps = Vec::new();
        for rows in self.steps.values() {
            all_steps.extend(rows.iter().cloned());
        }
        all_steps.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
        });
        write_jsonl(&self.path(tables::STEPS), &all_steps)?;

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
        write_jsonl(&self.path(tables::TOOL_CALLS), &all_calls)?;
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

fn write_jsonl<T: serde::Serialize>(path: &Path, rows: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    for row in rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

impl ChronicleStore for FsChronicleStore {
    fn upsert_session(&mut self, row: SessionRow) -> Result<()> {
        self.sessions.insert(row.session_id.clone(), row);
        self.persist()
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
        self.steps.insert(session_id.to_string(), rows);
        self.persist()
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
        self.tool_calls.insert(session_id.to_string(), rows);
        self.persist()
    }

    fn list_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCallRow>> {
        Ok(self.tool_calls.get(session_id).cloned().unwrap_or_default())
    }
}
