//! Parse the design-doc example with the same `toml` crate as the CLI.

use std::path::PathBuf;

fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/design/examples/trajectory_complex_markdown.toml")
}

#[test]
fn parse_design_trajectory_complex_markdown_toml() {
    let path = example_path();
    let s = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let v: toml::Value = toml::from_str(&s).unwrap_or_else(|e| panic!("toml parse: {e}"));
    let recs = v
        .get("records")
        .and_then(toml::Value::as_array)
        .expect("`records` must be a TOML array");
    assert_eq!(recs.len(), 4, "expected 4 [[records]] segments");

    // Spot-check nested keys exist
    assert!(recs[0].get("meta").is_some());
    assert!(recs[1].get("body_md").is_some());
}
