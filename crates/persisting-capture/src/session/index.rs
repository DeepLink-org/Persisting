//! Session index on disk for `capture list` without full Vortex replay.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::ProviderKind;
use crate::usage::TokenUsage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    pub storage: String,
    pub updated_at: DateTime<Utc>,
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub agent_id: String,
    pub session_id: String,
    pub provider: String,
    pub protocol: String,
    pub model: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub request_count: u64,
    pub usage: TokenUsage,
    pub estimated_cost_usd: f64,
    pub active: bool,
}

pub struct SessionIndexStore {
    path: PathBuf,
    inner: Arc<RwLock<SessionIndex>>,
    dirty: Arc<AtomicBool>,
}

impl SessionIndexStore {
    pub fn open(storage: &Path) -> Result<Self> {
        fs::create_dir_all(storage)?;
        let path = storage.join(".capture").join("sessions.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let inner = if path.exists() {
            let s = fs::read_to_string(&path)?;
            serde_json::from_str(&s).unwrap_or_else(|_| SessionIndex {
                storage: storage.display().to_string(),
                updated_at: Utc::now(),
                sessions: Vec::new(),
            })
        } else {
            SessionIndex {
                storage: storage.display().to_string(),
                updated_at: Utc::now(),
                sessions: Vec::new(),
            }
        };
        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(inner)),
            dirty: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn clone_handle(&self) -> SessionIndexHandle {
        SessionIndexHandle {
            inner: Arc::clone(&self.inner),
            path: self.path.clone(),
            dirty: Arc::clone(&self.dirty),
        }
    }

    pub fn load(storage: &Path) -> Result<SessionIndex> {
        let path = storage.join(".capture").join("sessions.json");
        if !path.exists() {
            return Ok(SessionIndex {
                storage: storage.display().to_string(),
                updated_at: Utc::now(),
                sessions: Vec::new(),
            });
        }
        let s = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save(&self) -> Result<()> {
        let mut guard = self.inner.write().unwrap();
        guard.updated_at = Utc::now();
        let json = serde_json::to_string_pretty(&*guard)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, json)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SessionIndexHandle {
    inner: Arc<RwLock<SessionIndex>>,
    path: PathBuf,
    dirty: Arc<AtomicBool>,
}

impl SessionIndexHandle {
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub fn record_request(
        &self,
        agent_id: &str,
        session_id: &str,
        provider: ProviderKind,
        protocol: &str,
        model: &str,
    ) {
        let now = Utc::now();
        let mut guard = self.inner.write().unwrap();
        if let Some(s) = guard
            .sessions
            .iter_mut()
            .find(|s| s.agent_id == agent_id && s.session_id == session_id)
        {
            s.last_seen = now;
            s.request_count += 1;
            s.active = true;
            if !model.is_empty() && model != "_unknown" {
                s.model = model.to_string();
            }
            s.provider = provider.as_str().to_string();
            s.protocol = protocol.to_string();
        } else {
            guard.sessions.push(SessionSummary {
                agent_id: agent_id.to_string(),
                session_id: session_id.to_string(),
                provider: provider.as_str().to_string(),
                protocol: protocol.to_string(),
                model: model.to_string(),
                first_seen: now,
                last_seen: now,
                request_count: 1,
                usage: TokenUsage::default(),
                estimated_cost_usd: 0.0,
                active: true,
            });
        }
        self.mark_dirty();
    }

    pub fn record_response(
        &self,
        agent_id: &str,
        session_id: &str,
        provider: ProviderKind,
        model: &str,
        usage: &TokenUsage,
        cost_usd: f64,
    ) {
        let now = Utc::now();
        let mut guard = self.inner.write().unwrap();
        if let Some(s) = guard
            .sessions
            .iter_mut()
            .find(|s| s.agent_id == agent_id && s.session_id == session_id)
        {
            s.last_seen = now;
            s.active = false;
            s.usage.merge(usage);
            s.estimated_cost_usd += cost_usd;
            if !model.is_empty() {
                s.model = model.to_string();
            }
            s.provider = provider.as_str().to_string();
        }
        self.mark_dirty();
    }

    pub fn set_active(&self, agent_id: &str, session_id: &str, active: bool) {
        let mut guard = self.inner.write().unwrap();
        if let Some(s) = guard
            .sessions
            .iter_mut()
            .find(|s| s.agent_id == agent_id && s.session_id == session_id)
        {
            s.active = active;
        }
        self.mark_dirty();
    }

    pub fn flush_if_dirty(&self) -> Result<()> {
        if !self.dirty.load(Ordering::Acquire) {
            return Ok(());
        }
        self.flush()
    }

    pub fn flush(&self) -> Result<()> {
        let guard = self.inner.read().unwrap();
        let json = serde_json::to_string_pretty(&*guard)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, json).context("write sessions.json")?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub fn snapshot(&self) -> SessionIndex {
        self.inner.read().unwrap().clone()
    }
}

/// Scan session layout dirs and merge with index file.
pub fn discover_sessions(storage: &Path) -> Result<Vec<SessionSummary>> {
    let mut by_key: HashMap<(String, String), SessionSummary> = HashMap::new();
    if let Ok(index) = SessionIndexStore::load(storage) {
        for s in index.sessions {
            by_key.insert((s.agent_id.clone(), s.session_id.clone()), s);
        }
    }
    if storage.is_dir() {
        for agent_entry in fs::read_dir(storage)? {
            let agent_entry = agent_entry?;
            if !agent_entry.file_type()?.is_dir() {
                continue;
            }
            let name = agent_entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            for sess_entry in fs::read_dir(agent_entry.path())? {
                let sess_entry = sess_entry?;
                if !sess_entry.file_type()?.is_dir() {
                    continue;
                }
                let sid = sess_entry.file_name().to_string_lossy().to_string();
                by_key
                    .entry((name.clone(), sid.clone()))
                    .or_insert_with(|| SessionSummary {
                        agent_id: name.clone(),
                        session_id: sid,
                        provider: "unknown".into(),
                        protocol: "unknown".into(),
                        model: "unknown".into(),
                        first_seen: Utc::now(),
                        last_seen: Utc::now(),
                        request_count: 0,
                        usage: TokenUsage::default(),
                        estimated_cost_usd: 0.0,
                        active: false,
                    });
            }
        }
    }
    let mut out: Vec<_> = by_key.into_values().collect();
    out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderKind;
    use crate::usage::TokenUsage;

    #[test]
    fn flush_if_dirty_skips_when_not_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionIndexStore::open(dir.path()).unwrap();
        let handle = store.clone_handle();
        handle.flush_if_dirty().unwrap();
        assert!(!dir.path().join(".capture").join("sessions.json").exists());
    }

    #[test]
    fn flush_if_dirty_persists_after_record_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionIndexStore::open(dir.path()).unwrap();
        let handle = store.clone_handle();
        handle.record_request("agent", "sess", ProviderKind::OpenAi, "chat", "gpt-4");
        handle.flush_if_dirty().unwrap();

        let loaded = SessionIndexStore::load(dir.path()).unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].request_count, 1);
        assert!(loaded.sessions[0].active);
    }

    #[test]
    fn cloned_handles_share_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionIndexStore::open(dir.path()).unwrap();
        let writer = store.clone_handle();
        let reader = writer.clone();

        writer.record_request("agent", "sess", ProviderKind::OpenAi, "chat", "m");
        reader.flush_if_dirty().unwrap();

        writer.record_response(
            "agent",
            "sess",
            ProviderKind::OpenAi,
            "m",
            &TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            0.01,
        );
        writer.flush_if_dirty().unwrap();

        let loaded = SessionIndexStore::load(dir.path()).unwrap();
        assert_eq!(loaded.sessions[0].request_count, 1);
        assert_eq!(loaded.sessions[0].usage.total_tokens, 15);
        assert!(!loaded.sessions[0].active);
    }

    #[test]
    fn second_flush_if_dirty_without_changes_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionIndexStore::open(dir.path()).unwrap();
        let handle = store.clone_handle();
        handle.record_request(
            "agent",
            "sess",
            ProviderKind::Anthropic,
            "messages",
            "claude",
        );
        handle.flush_if_dirty().unwrap();
        handle.flush_if_dirty().unwrap();

        let loaded = SessionIndexStore::load(dir.path()).unwrap();
        assert_eq!(loaded.sessions.len(), 1);
    }
}
