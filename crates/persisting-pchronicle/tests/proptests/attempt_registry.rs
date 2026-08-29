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
}
