use persisting_pchronicle::document::DocumentFormat;
use proptest::prelude::*;
use std::str::FromStr;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_document_formats_roundtrip_through_canonical_names(
        format in prop::sample::select(DocumentFormat::ALL.to_vec()),
        leading in 0usize..4,
        trailing in 0usize..4,
        uppercase in any::<bool>(),
    ) {
        let canonical = format.as_str();
        let name = if uppercase { canonical.to_ascii_uppercase() } else { canonical.to_string() };
        let decorated = format!("{}{}{}", " ".repeat(leading), name, "\t".repeat(trailing));
        prop_assert_eq!(DocumentFormat::from_str(&decorated).unwrap(), format);
        prop_assert_eq!(format.to_string(), canonical);
    }

    #[test]
    fn public_unknown_document_format_names_are_rejected(
        suffix in proptest::string::string_regex("[a-z0-9-]{0,24}").unwrap(),
    ) {
        let unknown = format!("unknown-format-{suffix}");
        let error = DocumentFormat::from_str(&unknown).unwrap_err();
        prop_assert_eq!(error.kind(), persisting_pchronicle::document::InputIssueKind::Invalid);
        prop_assert!(error.message().contains(&unknown));
    }

    #[test]
    fn public_document_format_names_are_path_safe(
        format in prop::sample::select(DocumentFormat::ALL.to_vec()),
    ) {
        let name = format.as_str();
        prop_assert!(!name.contains('/'));
        prop_assert!(!name.contains('\\'));
        prop_assert!(!name.chars().any(char::is_whitespace));
    }
}
