use persisting_pchronicle::storage::{StoryCoords, drop_lifecycle_run_partitions};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_lifecycle_root_is_removed_only_when_a_sibling_story_exists(
        root in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
        child in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
    ) {
        prop_assume!(root != child);
        let locations = vec![
            StoryCoords::new("store", "agent", &root, Some(root.clone())),
            StoryCoords::new("store", "agent", &child, Some(root.clone())),
        ];
        let kept = drop_lifecycle_run_partitions(locations);
        prop_assert_eq!(kept.len(), 1);
        prop_assert_eq!(&kept[0].session_id, &child);
    }

    #[test]
    fn public_standalone_locations_are_never_dropped(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
    ) {
        let location = StoryCoords::new("store", "agent", &session, None);
        let kept = drop_lifecycle_run_partitions(vec![location.clone()]);
        prop_assert_eq!(kept, vec![location]);
    }
}
