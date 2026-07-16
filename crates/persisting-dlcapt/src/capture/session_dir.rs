use chrono::{DateTime, Utc};

/// Resolved storage layout for a session (relative to `store_dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLayout {
    pub session_dir: String,
    pub run_bucket: String,
}

/// Compute `session_dir` and `run_bucket` per spec §3.7.1.
pub fn resolve_session_layout(
    storage_session_id: &str,
    default_session_id: &str,
    now: DateTime<Utc>,
) -> SessionLayout {
    let today = now.format("%Y-%m-%d").to_string();
    if storage_session_id == default_session_id {
        SessionLayout {
            session_dir: format!("default/{today}"),
            run_bucket: today,
        }
    } else {
        SessionLayout {
            session_dir: storage_session_id.to_string(),
            run_bucket: today,
        }
    }
}

/// For real sessions, `run_bucket` is the first-write UTC date (may differ from `now`).
pub fn resolve_session_layout_with_bucket(
    storage_session_id: &str,
    default_session_id: &str,
    now: DateTime<Utc>,
    existing_run_bucket: Option<&str>,
) -> SessionLayout {
    let mut layout = resolve_session_layout(storage_session_id, default_session_id, now);
    if storage_session_id != default_session_id
        && let Some(bucket) = existing_run_bucket.filter(|b| !b.is_empty())
    {
        layout.run_bucket = bucket.to_string();
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_session_uses_date_bucket_in_path() {
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let layout = resolve_session_layout("default", "default", now);
        assert_eq!(layout.session_dir, "default/2026-06-16");
        assert_eq!(layout.run_bucket, "2026-06-16");
    }

    #[test]
    fn real_session_uses_flat_dir() {
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let layout = resolve_session_layout("abc-123", "default", now);
        assert_eq!(layout.session_dir, "abc-123");
        assert_eq!(layout.run_bucket, "2026-06-16");
    }
}
