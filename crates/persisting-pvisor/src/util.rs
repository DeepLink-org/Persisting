//! Shared helpers for pVisor.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
