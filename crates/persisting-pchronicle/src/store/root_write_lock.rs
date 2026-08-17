use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

type WriteLock = tokio::sync::Mutex<()>;

/// Return a process-local lock scoped to one physical storage root.
///
/// The registry keeps only weak references so opening many short-lived stores
/// does not retain one mutex per root forever. Cross-process exclusion and
/// object-store fencing remain the responsibility of each backend.
pub(super) fn for_root(root: &str) -> Arc<WriteLock> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<WriteLock>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(WriteLock::new(()));
    locks.insert(root.to_string(), Arc::downgrade(&lock));
    lock
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_are_shared_per_root_and_distinct_between_roots() {
        let first = for_root("root-a");
        let same = for_root("root-a");
        let other = for_root("root-b");
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other));
    }
}
