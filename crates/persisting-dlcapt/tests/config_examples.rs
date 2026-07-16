use persisting_dlcapt::config::ProxyConfig;
use std::path::{Path, PathBuf};

fn example_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("config");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|_| panic!("missing config dir: {}", root.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".example.toml") && !n.contains("openclaw-test"))
        })
        .collect();
    files.sort();
    files
}

#[test]
fn public_example_configs_parse() {
    let files = example_files();
    assert!(
        !files.is_empty(),
        "expected *.example.toml under config/ (excluding openclaw-test template)"
    );
    for example in files {
        ProxyConfig::load(&example)
            .unwrap_or_else(|error| panic!("{}: {error:#}", example.display()));
    }
}

#[test]
fn openclaw_test_template_has_placeholders() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("config/proxy.openclaw-test.example.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    assert!(raw.contains("__STORE_DIR__"));
    assert!(raw.contains("__UPSTREAM_BASE_URL__"));
    assert!(raw.contains("kimi-k2.5"));
    assert!(raw.contains("127.0.0.1:19081"));
    assert!(!raw.contains("ailab-pj"));
    assert!(!raw.contains("0.0.0.0"));
}
