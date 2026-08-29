use std::path::Path;

use persisting_pchronicle::storage::{session_filename_stem, session_markdown_filename};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_session_filename_helpers_are_bounded_and_path_safe(session_id in any::<String>()) {
        let stem = session_filename_stem(&session_id);
        let filename = session_markdown_filename(&session_id);
        prop_assert!(!stem.is_empty());
        prop_assert!(stem.len() <= 128);
        prop_assert!(stem.is_ascii());
        prop_assert_eq!(&filename, &format!("{stem}.md"));
        prop_assert_eq!(
            Path::new(&filename).file_name().and_then(|name| name.to_str()),
            Some(filename.as_str()),
        );
    }

    #[test]
    fn public_session_filename_stem_is_deterministic_for_arbitrary_unicode(
        session_id in proptest::collection::vec(any::<char>(), 0..256)
            .prop_map(|chars| chars.into_iter().collect::<String>()),
    ) {
        let first = session_filename_stem(&session_id);
        let second = session_filename_stem(&session_id);
        prop_assert_eq!(first, second);
    }
}
