//! Live skip set: task_ids that must not be dispatched.
//!
//! Seeded from `--resume` (ready ∪ failures) and grown as tasks are claimed /
//! completed in the current job — so mid-run sink persistence and duplicate
//! `plan()` yields do not re-dispatch the same id onto another worker.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Shared, mutable set of task ids to skip (or already claimed).
#[derive(Clone, Default)]
pub struct SkipSet {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl SkipSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.inner.lock().map(|g| g.contains(id)).unwrap_or(false)
    }

    /// Insert `id`. Returns `true` if it was newly claimed (caller may dispatch).
    pub fn insert(&self, id: impl Into<String>) -> bool {
        self.inner
            .lock()
            .map(|mut g| g.insert(id.into()))
            .unwrap_or(false)
    }

    /// Drop a claim (e.g. sink persist failed — id was never durable).
    pub fn remove(&self, id: &str) -> bool {
        self.inner.lock().map(|mut g| g.remove(id)).unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FromIterator<String> for SkipSet {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(iter.into_iter().collect())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_once() {
        let s = SkipSet::new();
        assert!(s.insert("t-0"));
        assert!(!s.insert("t-0"));
        assert!(s.contains("t-0"));
    }

    #[test]
    fn from_iter_seeds() {
        let s: SkipSet = ["a".into(), "b".into()].into_iter().collect();
        assert!(s.contains("a") && s.contains("b"));
        assert!(!s.insert("a"));
    }

    #[test]
    fn remove_unclaims() {
        let s = SkipSet::new();
        assert!(s.insert("t-0"));
        assert!(s.remove("t-0"));
        assert!(!s.contains("t-0"));
        assert!(s.insert("t-0"));
    }
}
