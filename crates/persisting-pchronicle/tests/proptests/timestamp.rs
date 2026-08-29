use persisting_pchronicle::model::StorylineTimestamp;
use proptest::prelude::*;
use serde_json::json;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_numeric_timestamps_preserve_nanoseconds(
        seconds in -9_000_000_000i64..=9_000_000_000,
    ) {
        let source = json!(seconds);
        let timestamp = StorylineTimestamp::from_json(source.clone()).unwrap();
        prop_assert_eq!(timestamp.timestamp_nanos(), seconds * 1_000_000_000);
        prop_assert_eq!(timestamp.source_value(), &source);
        prop_assert_eq!(serde_json::to_value(&timestamp).unwrap(), source);
    }
}
