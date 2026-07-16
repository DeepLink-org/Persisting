use std::path::{Path, PathBuf};

const OPENCLAW_TEMPLATE: &str = "proxy.openclaw-test.example.toml";
const ALLOWED_PLACEHOLDERS: [&str; 2] = ["__STORE_DIR__", "__UPSTREAM_BASE_URL__"];

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("config")
}

fn config_files() -> Vec<PathBuf> {
    let root = config_root();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|_| panic!("missing config dir: {}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

fn placeholders(value: &toml::Value, values: &mut Vec<String>) {
    match value {
        toml::Value::String(value) if value.contains("__") => values.push(value.clone()),
        toml::Value::Array(values_in_array) => {
            for value in values_in_array {
                placeholders(value, values);
            }
        }
        toml::Value::Table(values_in_table) => {
            for value in values_in_table.values() {
                placeholders(value, values);
            }
        }
        _ => {}
    }
}

#[test]
fn config_directory_contains_only_the_openclaw_template() {
    assert_eq!(
        config_files(),
        vec![config_root().join(OPENCLAW_TEMPLATE)],
        "the OpenClaw template is the only supported config example"
    );
}

#[test]
fn openclaw_template_uses_only_safe_placeholders() {
    let path = config_root().join(OPENCLAW_TEMPLATE);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    let parsed: toml::Value = toml::from_str(&raw).unwrap_or_else(|e| panic!("{e}"));
    let mut found_placeholders = Vec::new();
    placeholders(&parsed, &mut found_placeholders);
    found_placeholders.sort();
    assert!(
        found_placeholders
            .iter()
            .all(|value| ALLOWED_PLACEHOLDERS.contains(&value.as_str()))
    );
    for placeholder in ALLOWED_PLACEHOLDERS {
        assert!(found_placeholders.iter().any(|value| value == placeholder));
    }
    assert_eq!(
        parsed.get("store_dir").and_then(toml::Value::as_str),
        Some("__STORE_DIR__")
    );
    let models = parsed
        .get("models")
        .and_then(toml::Value::as_array)
        .expect("models array");
    for model in models {
        assert_eq!(
            model.get("upstream_base_url").and_then(toml::Value::as_str),
            Some("__UPSTREAM_BASE_URL__")
        );
        assert_eq!(model.get("api_key").and_then(toml::Value::as_str), Some(""));
    }
    assert!(raw.contains("kimi-k2.5"));
    assert!(raw.contains("127.0.0.1:19081"));
    assert!(!raw.contains("ailab-pj"));
    assert!(!raw.contains("0.0.0.0"));
}
