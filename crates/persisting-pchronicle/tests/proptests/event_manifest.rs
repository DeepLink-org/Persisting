use persisting_pchronicle::storage::EventWriterFence;
use proptest::prelude::*;

fn writer_id_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._-]{1,24}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_writer_fences_require_a_positive_epoch_and_nonempty_owner(
        epoch in 1u64..1_000_000,
        owner in writer_id_strategy(),
    ) {
        let fence = EventWriterFence::new(epoch, owner.clone()).expect("generated fence is valid");
        prop_assert_eq!(fence.epoch, epoch);
        prop_assert_eq!(fence.writer_id, owner);
        prop_assert!(EventWriterFence::new(0, "writer").is_err());
        prop_assert!(EventWriterFence::new(epoch, "   ").is_err());
    }

    #[test]
    fn public_writer_fence_serialization_roundtrips(
        epoch in 1u64..1_000_000,
        owner in writer_id_strategy(),
    ) {
        let fence = EventWriterFence::new(epoch, owner).unwrap();
        let encoded = serde_json::to_string(&fence).unwrap();
        prop_assert_eq!(serde_json::from_str::<EventWriterFence>(&encoded).unwrap(), fence);
    }

    #[test]
    fn public_writer_fence_preserves_nonempty_owner_whitespace(
        epoch in 1u64..1_000_000,
        owner in proptest::string::string_regex("[ A-Za-z0-9._-]{0,32}").unwrap(),
    ) {
        let result = EventWriterFence::new(epoch, owner.clone());
        if owner.trim().is_empty() {
            prop_assert!(result.is_err());
        } else {
            prop_assert_eq!(result.unwrap().writer_id, owner);
        }
    }
}
