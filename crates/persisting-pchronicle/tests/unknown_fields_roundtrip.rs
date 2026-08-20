use anyhow::Result;
use persisting_pchronicle::document::{
    decode_json_storylines, encode_json_storylines, DocumentFormat,
};
use serde_json::{json, Value};

#[test]
fn atif_unknown_fields_survive_an_actf_hop() -> Result<()> {
    let input = json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "unknown-fields",
        "session_id": null,
        "agent": {
            "name": "agent",
            "version": "1",
            "vendor_agent": {"enabled": true}
        },
        "steps": [{
            "step_id": 1,
            "source": "user",
            "message": "hello",
            "vendor_step": [1, {"nested": "value"}],
            "vendor_null": null
        }],
        "vendor/root": {"tilde~key": 7}
    });

    let atif_stories =
        decode_json_storylines(DocumentFormat::Atif, &input.to_string(), "source.json")?;

    let actf = encode_json_storylines(DocumentFormat::Actf, &atif_stories)?;
    assert_eq!(actf["_storyline"]["unknown_fields"]["version"], 1);

    let restored =
        decode_json_storylines(DocumentFormat::Actf, &actf.to_string(), "carrier.actf.json")?;
    assert_eq!(
        restored[0].unknown_fields.sources["atif"],
        atif_stories[0].unknown_fields.sources["atif"]
    );
    assert_eq!(
        restored[0].unknown_key_counts["atif"]["/steps/*/vendor_step"],
        1
    );
    assert_eq!(restored[0].unknown_key_counts["atif"]["/vendor~1root"], 1);

    let recovered_atif = encode_json_storylines(DocumentFormat::Atif, &restored)?;
    assert_eq!(
        recovered_atif["agent"]["vendor_agent"],
        json!({"enabled": true})
    );
    assert_eq!(
        recovered_atif["steps"][0]["vendor_step"],
        json!([1, {"nested": "value"}])
    );
    assert_eq!(recovered_atif["steps"][0]["vendor_null"], Value::Null);
    assert_eq!(recovered_atif["vendor/root"], json!({"tilde~key": 7}));
    Ok(())
}
