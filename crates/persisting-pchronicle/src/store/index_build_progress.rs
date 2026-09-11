//! Optional UI hook for long-running Lance index builds.
//!
//! Import / maintain callers can install a short-lived listener so progress stays
//! on the dense TTY surface instead of relying on Lance's INFO spam.

use std::sync::{Arc, Mutex, OnceLock};

type Listener = Arc<dyn Fn(&str) + Send + Sync>;

fn slot() -> &'static Mutex<Option<Listener>> {
    static SLOT: OnceLock<Mutex<Option<Listener>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Restores the previous listener when dropped.
pub struct Guard {
    previous: Option<Listener>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(mut slot) = slot().lock() {
            *slot = self.previous.take();
        }
    }
}

/// Install a process-wide index-progress listener for the current scope.
pub fn install(listener: Arc<dyn Fn(&str) + Send + Sync>) -> Guard {
    let previous = match slot().lock() {
        Ok(mut slot) => slot.replace(listener),
        Err(_) => None,
    };
    Guard { previous }
}

/// Report a short, single-line index activity message (best-effort).
pub fn note(message: impl AsRef<str>) {
    let Ok(slot) = slot().lock() else {
        return;
    };
    if let Some(listener) = slot.as_ref() {
        listener(message.as_ref());
    }
}

pub(crate) fn table_label(uri: &str) -> &str {
    let trimmed = uri.trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches(".lance")
}
