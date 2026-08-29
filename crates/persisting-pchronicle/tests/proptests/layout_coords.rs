use persisting_pchronicle::storage::{StoryCoords, story_lance_event_path, story_run_dir};
use proptest::prelude::*;

fn segment_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap()
}

fn padded_segment_strategy() -> impl Strategy<Value = (String, String)> {
    (segment_strategy(), 0usize..=3, 0usize..=3).prop_map(|(value, left, right)| {
        (
            format!("{}{}{}", " ".repeat(left), value, " ".repeat(right)),
            value,
        )
    })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_valid_segments_are_trimmed_but_remain_single_path_components(
        (agent, agent_trimmed) in padded_segment_strategy(),
        (session, session_trimmed) in padded_segment_strategy(),
    ) {
        let path = story_run_dir("/store", &agent, &session, None).unwrap();
        let suffix = format!("{agent_trimmed}/{session_trimmed}");
        prop_assert!(path.ends_with(suffix));
        prop_assert!(!agent_trimmed.contains('/'));
        prop_assert!(!session_trimmed.contains('/'));
    }

    #[test]
    fn public_nested_sessions_share_the_root_run_partition(
        agent in segment_strategy(),
        root in segment_strategy(),
        child in segment_strategy(),
    ) {
        let root_path = story_lance_event_path("/store", &agent, &root, Some(&root)).unwrap();
        let child_path = story_lance_event_path("/store", &agent, &child, Some(&root)).unwrap();
        prop_assert_eq!(root_path, child_path);
    }

    #[test]
    fn public_invalid_storage_and_segments_fail_closed(
        agent in segment_strategy(),
        session in segment_strategy(),
        invalid in prop_oneof![
            Just(String::new()),
            Just("   ".to_string()),
            Just(".".to_string()),
            Just("..".to_string()),
            Just("a/b".to_string()),
            Just("a\\b".to_string()),
        ],
    ) {
        prop_assert!(story_run_dir("", &agent, &session, None).is_err());
        prop_assert!(story_run_dir("/store", &invalid, &session, None).is_err());
        prop_assert!(story_run_dir("/store", &agent, &invalid, None).is_err());
    }

    #[test]
    fn public_coords_methods_delegate_to_the_free_functions(
        storage in prop_oneof![Just("/store".to_string()), Just("s3://bucket/prefix".to_string())],
        agent in segment_strategy(),
        session in segment_strategy(),
        root in prop::option::of(segment_strategy()),
    ) {
        let coords = StoryCoords::new(storage.clone(), agent.clone(), session.clone(), root.clone());
        prop_assert_eq!(
            coords.run_dir().unwrap(),
            story_run_dir(&storage, &agent, &session, root.as_deref()).unwrap(),
        );
        prop_assert_eq!(
            coords.lance_event_path().unwrap(),
            story_lance_event_path(&storage, &agent, &session, root.as_deref()).unwrap(),
        );
    }

    #[test]
    fn public_root_partition_is_independent_of_child_session(
        agent in segment_strategy(),
        root in segment_strategy(),
        child in segment_strategy(),
    ) {
        prop_assert_eq!(
            story_lance_event_path("/store", &agent, &root, Some(&root)).unwrap(),
            story_lance_event_path("/store", &agent, &child, Some(&root)).unwrap(),
        );
    }
}
