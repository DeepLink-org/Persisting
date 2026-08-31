use persisting_pchronicle::storage::{AttemptRecord, AttemptRecordState};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_attempt_liveness_requires_active_unexpired_records(
        active in any::<bool>(),
        now in any::<u64>(),
        expires in any::<u64>(),
    ) {
        let record = AttemptRecord {
            revision: 1,
            run_id: "run".into(),
            attempt_id: "attempt".into(),
            lease_epoch: 1,
            state: if active { AttemptRecordState::Active } else { AttemptRecordState::Terminal },
            heartbeat_at_unix_ms: now,
            expires_at_unix_ms: expires,
            terminal_result: None,
        };
        prop_assert_eq!(record.is_live_at(now), active && expires > now);
    }

    #[test]
    fn public_attempt_records_roundtrip_optional_terminal_results(
        revision in any::<u64>(),
        run_id in "[A-Za-z0-9_-]{1,24}",
        attempt_id in "[A-Za-z0-9_-]{1,24}",
        lease_epoch in any::<u64>(),
        active in any::<bool>(),
        heartbeat in any::<u64>(),
        expires in any::<u64>(),
        result in prop::option::of(proptest::string::string_regex("[A-Za-z0-9 _:/-]{0,64}").unwrap()),
    ) {
        let record = AttemptRecord {
            revision,
            run_id,
            attempt_id,
            lease_epoch,
            state: if active { AttemptRecordState::Active } else { AttemptRecordState::Terminal },
            heartbeat_at_unix_ms: heartbeat,
            expires_at_unix_ms: expires,
            terminal_result: result.map(serde_json::Value::String),
        };
        let encoded = serde_json::to_string(&record).unwrap();
        prop_assert_eq!(serde_json::from_str::<AttemptRecord>(&encoded).unwrap(), record);
    }

    #[test]
    fn public_attempt_liveness_treats_expiry_as_an_exclusive_boundary(
        now in any::<u64>(),
    ) {
        let active = AttemptRecord {
            revision: 1,
            run_id: "run".into(),
            attempt_id: "attempt".into(),
            lease_epoch: 1,
            state: AttemptRecordState::Active,
            heartbeat_at_unix_ms: now,
            expires_at_unix_ms: now,
            terminal_result: None,
        };
        prop_assert!(!active.is_live_at(now));
        let expires_after = AttemptRecord { expires_at_unix_ms: now.saturating_add(1), ..active };
        if now < u64::MAX {
            prop_assert!(expires_after.is_live_at(now));
        }
    }
}
