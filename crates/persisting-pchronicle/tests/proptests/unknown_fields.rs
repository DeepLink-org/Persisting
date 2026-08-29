use persisting_pchronicle::model::{
    StorylineUnknownFields, UnknownFieldLimits, compute_unknown_key_counts, validate_json_pointer,
    validate_unknown_fields,
};
use proptest::prelude::*;
use serde_json::json;

fn pointer_token_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..64).prop_map(|chars| chars.into_iter().collect())
}

fn encode_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_unknown_field_limits_accept_exactly_positive_values(
        max_fields in 1usize..1_000_000,
        max_bytes in 1usize..1_000_000,
    ) {
        let positive = UnknownFieldLimits { max_fields, max_bytes };
        let zero_fields = UnknownFieldLimits { max_fields: 0, max_bytes };
        let zero_bytes = UnknownFieldLimits { max_fields, max_bytes: 0 };
        prop_assert!(positive.validate().is_ok());
        prop_assert!(zero_fields.validate().is_err());
        prop_assert!(zero_bytes.validate().is_err());
    }

    #[test]
    fn public_unknown_field_insert_and_counts_preserve_escaped_pointers(
        token in pointer_token_strategy(),
        value in any::<String>(),
    ) {
        let pointer = format!("/{}", encode_pointer_token(&token));
        prop_assert!(validate_json_pointer(&pointer).is_ok());

        let mut fields = StorylineUnknownFields::default();
        prop_assert!(fields.is_empty());
        fields.insert("vendor", "source", pointer.clone(), json!(value)).unwrap();
        prop_assert!(!fields.is_empty());

        let counts = compute_unknown_key_counts(&fields).unwrap();
        prop_assert_eq!(counts["vendor"][&pointer], 1);
        let validated = validate_unknown_fields(&fields, UnknownFieldLimits::default()).unwrap();
        prop_assert_eq!(validated, counts);
    }
}
