use persisting_pchronicle::document::{DocumentFormat, detect_format};
use proptest::prelude::*;
use std::path::Path;

fn safe_stem_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_-]{1,24}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_registered_path_suffixes_are_ascii_case_insensitive(
        stem in safe_stem_strategy(),
        uppercase in any::<bool>(),
    ) {
        let cases = [
            ("storyline.json", DocumentFormat::Storyline),
            ("actf.json", DocumentFormat::Actf),
            ("md", DocumentFormat::AgenticMd),
        ];
        for (suffix, expected) in cases {
            let path = format!("{stem}.{suffix}");
            let path = if uppercase { path.to_ascii_uppercase() } else { path };
            prop_assert_eq!(detect_format(Some(Path::new(&path)), None).unwrap(), Some(expected));
        }
    }

    #[test]
    fn public_event_lance_names_are_detected_independent_of_ascii_case(
        prefix in safe_stem_strategy(),
        suffix in safe_stem_strategy(),
        uppercase in any::<bool>(),
    ) {
        let path = format!("{prefix}-event-{suffix}.lance");
        let path = if uppercase { path.to_ascii_uppercase() } else { path };
        prop_assert_eq!(
            detect_format(Some(Path::new(&path)), None).unwrap(),
            Some(DocumentFormat::CanonicalEvent),
        );
    }

    #[test]
    fn public_agentic_markers_survive_leading_whitespace(
        spaces in 0usize..32,
        use_block_marker in any::<bool>(),
    ) {
        let prefix = " ".repeat(spaces);
        let content = if use_block_marker {
            format!("{prefix}<!-- persisting:block name=test -->")
        } else {
            format!("{prefix}---\nformat: persisting\n---\n")
        };
        prop_assert_eq!(
            detect_format(None, Some(&content)).unwrap(),
            Some(DocumentFormat::AgenticMd),
        );
    }

    #[test]
    fn public_content_fingerprint_wins_over_any_recognized_path(path_kind in 0usize..4) {
        let paths = [
            "mismatch.storyline.json",
            "mismatch.actf.json",
            "mismatch.md",
            "event-log.lance",
        ];
        let claude = r#"{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        prop_assert_eq!(
            detect_format(Some(Path::new(paths[path_kind])), Some(claude)).unwrap(),
            Some(DocumentFormat::ClaudeCode),
        );
    }

    #[test]
    fn public_no_hint_and_empty_content_never_guess_a_format(
        whitespace in proptest::collection::vec(
            prop::sample::select(vec![' ', '\n', '\r', '\t']),
            0..64,
        ),
    ) {
        let content = whitespace.into_iter().collect::<String>();
        prop_assert_eq!(detect_format(None, Some(&content)).unwrap(), None);
    }
}
