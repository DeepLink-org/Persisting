use persisting_pchronicle::storage::{StoryCoords, revision_dataset_path};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_revision_dataset_path_is_always_run_scoped(
        storage in prop_oneof![
            Just("/tmp/store".to_string()),
            Just("s3://bucket/prefix".to_string()),
        ],
        agent in proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
        session in proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
        root in prop::option::of(
            proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
        ),
    ) {
        let coords = StoryCoords::new(storage, agent, session, root);
        let path = revision_dataset_path(&coords).unwrap();
        prop_assert!(path.ends_with("revisions.lance"));
        let run_dir = coords.run_dir().unwrap();
        prop_assert_eq!(std::path::Path::new(&path).parent(), Some(run_dir.as_path()));
    }

    #[test]
    fn public_revision_paths_share_the_root_partition_for_child_sessions(
        agent in proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
        root in proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
        child in proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}").unwrap(),
    ) {
        let root_coords = StoryCoords::new("/tmp/store", &agent, &root, Some(root.clone()));
        let child_coords = StoryCoords::new("/tmp/store", &agent, &child, Some(root));
        prop_assert_eq!(
            revision_dataset_path(&root_coords).unwrap(),
            revision_dataset_path(&child_coords).unwrap(),
        );
    }

    #[test]
    fn public_revision_dataset_path_rejects_unsafe_coordinates(
        unsafe_segment in prop_oneof![
            Just("".to_string()),
            Just(".".to_string()),
            Just("..".to_string()),
            Just("a/b".to_string()),
            Just("a\\\\b".to_string()),
        ],
    ) {
        let coords = StoryCoords::new("/tmp/store", unsafe_segment, "session", None);
        prop_assert!(revision_dataset_path(&coords).is_err());
    }
}
