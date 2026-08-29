use persisting_pchronicle::storage::StorylineLanceStore;
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_storyline_store_normalizes_local_root_slashes(
        suffix in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        trailing_slashes in 1usize..5,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(format!("storyline-{suffix}"));
        let raw = format!("{}{}", path.to_string_lossy(), "/".repeat(trailing_slashes));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = runtime.block_on(StorylineLanceStore::open_uri(&raw)).unwrap();
        prop_assert_eq!(store.storage_scheme(), "file");
        prop_assert_eq!(store.root_uri(), path.to_string_lossy());
        prop_assert_eq!(store.root(), path.as_path());
    }

    #[test]
    fn public_storyline_store_keeps_a_stable_root_uri(
        suffix in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(format!("storyline-{suffix}"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let path_string = path.to_string_lossy().into_owned();
        let store = runtime.block_on(StorylineLanceStore::open_uri(&path_string)).unwrap();
        prop_assert_eq!(store.root_uri(), store.root().to_string_lossy());
        prop_assert_eq!(store.root_uri(), path_string);
    }
}
