use persisting_pchronicle::storage::{StoryCoords, export_source_dirs};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_root_sessions_always_export_the_run_directory(
        agent in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
        include_subagents in any::<bool>(),
    ) {
        let storage = format!("/tmp/pchronicle-egress-{agent}");
        let coords = StoryCoords::new(&storage, &agent, &session, Some(session.clone()));
        let sources = export_source_dirs(&coords, include_subagents).unwrap();
        prop_assert_eq!(sources.len(), 1);
        prop_assert_eq!(&sources[0], &coords.run_dir().unwrap());
    }

    #[test]
    fn public_child_sessions_use_a_dedicated_existing_directory(
        agent in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
        root in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
        child in proptest::string::string_regex("[A-Za-z0-9_-]{1,16}").unwrap(),
    ) {
        prop_assume!(root != child);
        let temp = tempfile::tempdir().unwrap();
        let coords = StoryCoords::new(
            temp.path().to_string_lossy(),
            &agent,
            &child,
            Some(root),
        );
        let sub = coords.run_dir().unwrap().join("subagents").join(&child);
        std::fs::create_dir_all(&sub).unwrap();
        let sources = export_source_dirs(&coords, false).unwrap();
        prop_assert_eq!(sources, vec![sub]);
    }
}
