use persisting_pchronicle::document::{
    DocumentFormat, InputIssueKind, decode_json_storylines, encode_json_storylines,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_decode_only_formats_are_rejected_for_encoding(
        format in prop::sample::select(vec![
            DocumentFormat::CanonicalEvent,
            DocumentFormat::StorylineLance,
            DocumentFormat::Codex,
            DocumentFormat::ClaudeCode,
        ]),
    ) {
        let error = encode_json_storylines(format, &[]).unwrap_err();
        prop_assert!(error.to_string().contains(format.as_str()));
    }

    #[test]
    fn public_unregistered_formats_report_a_structured_input_issue(
        format in prop::sample::select(vec![
            DocumentFormat::CanonicalEvent,
            DocumentFormat::StorylineLance,
        ]),
    ) {
        let error = decode_json_storylines(format, "{}", "input.json").unwrap_err();
        prop_assert_eq!(error.kind(), InputIssueKind::Unsupported);
        prop_assert!(error.message().contains(format.as_str()));
        prop_assert_eq!(error.location(), None);
    }

    #[test]
    fn public_atif_decoder_reports_invalid_json_as_an_input_issue(
        suffix in proptest::string::string_regex("[A-Za-z0-9_-]{1,32}").unwrap(),
    ) {
        let input = format!("{{invalid-{suffix}}}");
        let error = decode_json_storylines(DocumentFormat::Atif, &input, "input.json").unwrap_err();
        prop_assert_eq!(error.kind(), InputIssueKind::Invalid);
    }
}
