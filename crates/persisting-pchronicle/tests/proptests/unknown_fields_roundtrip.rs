use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_root_unknown_fields_survive_atif_actf_atif_hops(
        key in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        value in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
    ) {
        let field = format!("vendor_{key}");
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "unknown-fields",
            "agent": {"name": "agent", "version": "1"},
            "steps": [{"step_id": 1, "source": "user", "message": "hello"}],
            field.clone(): value.clone(),
        });
        let atif = decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "source.json")
            .expect("decode generated ATIF");
        let actf = encode_json_storylines(DocumentFormat::Actf, &atif)
            .expect("encode generated ACTF");
        let restored = decode_json_storylines(DocumentFormat::Actf, &actf.to_string(), "carrier.json")
            .expect("decode ACTF carrier");
        let recovered = encode_json_storylines(DocumentFormat::Atif, &restored)
            .expect("re-encode ATIF");
        prop_assert_eq!(&recovered[&field], &serde_json::Value::String(value));
        prop_assert_eq!(&restored[0].unknown_fields, &atif[0].unknown_fields);
    }

    #[test]
    fn public_step_unknown_fields_survive_atif_actf_atif_hops(
        key in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        value in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
    ) {
        let field = format!("vendor_{key}");
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "unknown-step-fields",
            "agent": {"name": "agent", "version": "1"},
            "steps": [{
                "step_id": 1,
                "source": "user",
                "message": "hello",
                field.clone(): value.clone()
            }]
        });
        let atif = decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "source.json")
            .expect("decode generated ATIF");
        let actf = encode_json_storylines(DocumentFormat::Actf, &atif)
            .expect("encode generated ACTF");
        let restored = decode_json_storylines(DocumentFormat::Actf, &actf.to_string(), "carrier.json")
            .expect("decode ACTF carrier");
        let recovered = encode_json_storylines(DocumentFormat::Atif, &restored)
            .expect("re-encode ATIF");
        prop_assert_eq!(&recovered["steps"][0][&field], &serde_json::Value::String(value));
    }
}
