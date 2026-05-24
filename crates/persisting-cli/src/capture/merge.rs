//! Sort pending rows and assign monotonic `seq`.

use std::collections::HashSet;

use anyhow::{bail, Result};

use crate::capture::record::{CaptureRecord, PendingRecord};

pub fn merge_and_assign_seq(mut pending: Vec<PendingRecord>) -> Vec<CaptureRecord> {
    pending.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    pending
        .into_iter()
        .enumerate()
        .map(|(seq, p)| p.into_capture(seq as u64))
        .collect()
}

/// If `session_id` is unset, require exactly one distinct session in the batch.
pub fn resolve_session_id(explicit: Option<&str>, pending: &[PendingRecord]) -> Result<String> {
    if let Some(s) = explicit {
        return Ok(s.to_string());
    }
    let mut ids: HashSet<String> = HashSet::new();
    for p in pending {
        if let Some(ref sid) = p.session_id {
            ids.insert(sid.clone());
        }
    }
    match ids.len() {
        0 => bail!(
            "no session_id on captured events; pass --session-id or capture from a session that sets sessionId"
        ),
        1 => Ok(ids.into_iter().next().unwrap()),
        n => {
            let sample: Vec<_> = ids.into_iter().take(5).collect();
            bail!(
                "found {n} distinct session ids ({sample:?}); pass --session-id to import one session"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::record::SortKey;
    use serde_json::json;

    #[test]
    fn sort_by_timestamp() {
        let pending = vec![
            PendingRecord {
                sort_key: SortKey {
                    ts_nanos: 200,
                    file_order: 0,
                    line_no: 0,
                },
                source: "t".into(),
                kind: "a".into(),
                timestamp: None,
                session_id: Some("s1".into()),
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                payload: json!({}),
            },
            PendingRecord {
                sort_key: SortKey {
                    ts_nanos: 100,
                    file_order: 1,
                    line_no: 0,
                },
                source: "t".into(),
                kind: "b".into(),
                timestamp: None,
                session_id: Some("s1".into()),
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                payload: json!({}),
            },
        ];
        let merged = merge_and_assign_seq(pending);
        assert_eq!(merged[0].kind, "b");
        assert_eq!(merged[1].kind, "a");
        assert_eq!(merged[0].seq, 0);
        assert_eq!(merged[1].seq, 1);
    }
}
