use std::path::Path;

use persisting_pchronicle::storage::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, session_filename_stem,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
};
use proptest::prelude::*;

fn storage_safe_suffix_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_-]{1,64}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_markdown_filename_is_a_single_md_path_component(session_id in any::<String>()) {
        let filename = session_markdown_filename(&session_id);
        prop_assert!(filename.ends_with(".md"));
        prop_assert!(!filename.contains('/'));
        prop_assert!(!filename.contains('\\'));
        prop_assert_eq!(
            Path::new(&filename).file_name().and_then(|value| value.to_str()),
            Some(filename.as_str()),
        );
    }

    #[test]
    fn public_subagent_keys_are_detected_as_trajectory_paths(suffix in storage_safe_suffix_strategy()) {
        let key = format!("agent-{suffix}");
        let filename = session_markdown_filename(&key);
        prop_assert!(is_subagent_session_storage_key(&key));
        prop_assert!(is_trajectory_markdown_path(&filename));
    }

    #[test]
    fn public_run_and_agent_markdown_names_are_trajectory_paths(
        prefix in prop_oneof![Just("run-"), Just("agent-")],
        suffix in storage_safe_suffix_strategy(),
    ) {
        let filename = format!("{prefix}{suffix}.md");
        prop_assert!(is_trajectory_markdown_path(filename));
    }

    #[test]
    fn public_write_path_stays_beneath_run_directory_when_no_file_exists(session_id in any::<String>()) {
        let dir = tempfile::tempdir().unwrap();
        let path = session_markdown_write_path_for_key(dir.path(), &session_id);
        prop_assert_eq!(path.parent(), Some(dir.path()));
        prop_assert_eq!(path, session_markdown_path_for_key(dir.path(), &session_id));
    }

    #[test]
    fn public_filename_encoding_is_stable_after_trimming(session_id in any::<String>()) {
        prop_assert_eq!(session_filename_stem(&session_id), session_filename_stem(session_id.trim()));
    }

    #[test]
    fn public_short_distinct_session_ids_have_distinct_filename_stems(
        prefix in storage_safe_suffix_strategy(),
        first in prop_oneof![Just('/'), Just('\\'), Just('?'), Just(':')],
        second in prop_oneof![Just('/'), Just('\\'), Just('?'), Just(':')],
    ) {
        prop_assume!(first != second);
        let left = format!("{prefix}{first}x");
        let right = format!("{prefix}{second}x");
        prop_assert_ne!(session_filename_stem(&left), session_filename_stem(&right));
    }
}
