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

    #[test]
    fn public_story_bundle_reports_every_copied_file(
        file_names in proptest::collection::vec(
            proptest::string::string_regex("[A-Za-z0-9_-]{1,16}\\.md").unwrap(),
            1..8,
        ),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let session = storage.join("agent").join("session");
        std::fs::create_dir_all(&session).unwrap();
        for (index, file_name) in file_names.iter().enumerate() {
            std::fs::write(session.join(format!("{index}-{file_name}")), "content").unwrap();
        }
        let coords = StoryCoords::new(storage.to_string_lossy(), "agent", "session", None);
        let output = temp.path().join("out");
        let report = persisting_pchronicle::storage::export_story_bundle(&coords, &output, false)
            .unwrap();
        prop_assert_eq!(report.files_copied, file_names.len());
        prop_assert_eq!(report.source_paths.len(), 1);
        prop_assert!(report.note.contains(&file_names.len().to_string()));
    }
}
