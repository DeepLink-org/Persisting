//! Shared wall-clock helpers for runtime event timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

/// Return the current Unix wall-clock time in milliseconds.
///
/// Event timestamps are observational metadata rather than ordering truth.
/// A clock before the Unix epoch is represented as zero, and conversion
/// saturates at `u64::MAX` instead of truncating.
pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}
