//! Infra-retry idempotency cache: `task_id` → last terminal [`TaskResult`].
//!
//! Semantic block: **L2 idempotency (same worker)**.
//! Kept separate from [`crate::worker::WorkerActor`] so the policy is unit-testable
//! without Pulsing / Python.

use crate::task::TaskResult;
use std::collections::{HashMap, VecDeque};

/// Default cap for cached results per worker slot.
pub const DEFAULT_RESULT_CACHE_CAP: usize = 4096;

/// Bounded LRU-ish cache (evict oldest insert order).
#[derive(Debug, Default)]
pub struct ResultCache {
    map: HashMap<String, TaskResult>,
    order: VecDeque<String>,
    cap: usize,
}

impl ResultCache {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub fn get(&self, task_id: &str) -> Option<&TaskResult> {
        self.map.get(task_id)
    }

    /// Insert or replace. Skips caching cancelled results (caller should not insert them).
    pub fn put(&mut self, task_id: impl Into<String>, result: TaskResult) {
        let task_id = task_id.into();
        if result.cancelled {
            return;
        }
        if self.map.contains_key(&task_id) {
            self.map.insert(task_id, result);
            return;
        }
        while self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        self.order.push_back(task_id.clone());
        self.map.insert(task_id, result);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn put_get_and_skip_cancelled() {
        let mut c = ResultCache::new(8);
        c.put("t-0", TaskResult::success("t-0", json!(1), "w0", 0.0));
        assert!(c.get("t-0").unwrap().ok);
        c.put("t-1", TaskResult::cancelled("t-1"));
        assert!(c.get("t-1").is_none());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_over_cap() {
        let mut c = ResultCache::new(2);
        c.put("a", TaskResult::success("a", json!(1), "w0", 0.0));
        c.put("b", TaskResult::success("b", json!(2), "w0", 0.0));
        c.put("c", TaskResult::success("c", json!(3), "w0", 0.0));
        assert!(c.get("a").is_none());
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn replace_keeps_cap() {
        let mut c = ResultCache::new(2);
        c.put("a", TaskResult::success("a", json!(1), "w0", 0.0));
        c.put("a", TaskResult::success("a", json!(9), "w0", 0.0));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("a").unwrap().value, Some(json!(9)));
    }
}
