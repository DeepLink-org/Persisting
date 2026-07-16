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
    for placeholder in ALLOWED_PLACEHOLDERS {
        assert!(raw.contains(placeholder), "missing {placeholder}");
    }
    for token in raw.split_whitespace().filter(|token| token.contains("__")) {
        assert!(
            ALLOWED_PLACEHOLDERS
                .iter()
                .any(|placeholder| token.contains(placeholder)),
            "unexpected placeholder token: {token}"
        );
    }
    assert!(raw.contains("kimi-k2.5"));
    assert!(raw.contains("127.0.0.1:19081"));
    assert!(raw.contains("api_key = \"\""));
    assert!(!raw.contains("ailab-pj"));
    assert!(!raw.contains("0.0.0.0"));
}
