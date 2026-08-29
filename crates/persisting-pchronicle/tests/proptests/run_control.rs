use persisting_agentctl::RunId;
use persisting_pchronicle::storage::{LeaseAcquireOutcome, RunControlStore};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_same_owner_lease_acquisition_is_idempotent(
        run in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        owner in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        ttl_ms in 1u64..10_000,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let store = RunControlStore::open(temp.path().to_string_lossy()).await.unwrap();
            let run_id = RunId::new(run);
            let first = store
                .acquire_lease(&run_id, Some("task"), &owner, ttl_ms)
                .await
                .unwrap();
            let first_lease = match first {
                LeaseAcquireOutcome::Acquired(lease) => lease,
                other => panic!("new run should acquire a lease, got {other:?}"),
            };

            let second = store
                .acquire_lease(&run_id, Some("task"), &owner, ttl_ms)
                .await
                .unwrap();
            let second_lease = match second {
                LeaseAcquireOutcome::Acquired(lease) => lease,
                other => panic!("same owner should renew its lease, got {other:?}"),
            };
            assert_eq!(second_lease.run_id, run_id);
            assert_eq!(second_lease.owner, owner);
            assert_eq!(second_lease.epoch, first_lease.epoch);
            assert!(second_lease.expires_at_unix_ms >= first_lease.expires_at_unix_ms);

            let record = store.get(&run_id).await.unwrap().unwrap();
            let current = record.lease.expect("lease is persisted");
            assert_eq!(current.epoch, second_lease.epoch);
            assert_eq!(current.owner, second_lease.owner);
        });
    }
}
