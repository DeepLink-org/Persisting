use persisting_dlcapt::config::ProxyConfig;
use std::path::{Path, PathBuf};

const PUBLIC_EXAMPLES: [&str; 4] = [
    "proxy.example.toml",
    "proxy.lance-local.example.toml",
    "proxy.lance-s3.deploy.example.toml",
    "proxy.lance-s3.example.toml",
];
const OPENCLAW_TEMPLATE: &str = "proxy.openclaw-test.example.toml";

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("config")
}

fn example_files() -> Vec<PathBuf> {
    let root = config_root();
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
    let expected: Vec<PathBuf> = PUBLIC_EXAMPLES
        .iter()
        .map(|name| config_root().join(name))
        .collect();
    assert_eq!(files, expected, "public example config contract changed");
    for example in &files {
        ProxyConfig::load(example)
            .unwrap_or_else(|error| panic!("{}: {error:#}", example.display()));
    }
}

#[test]
fn openclaw_template_exists_and_exclusively_uses_placeholders() {
    let path = config_root().join(OPENCLAW_TEMPLATE);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    assert!(raw.contains("__STORE_DIR__"));
    assert!(raw.contains("__UPSTREAM_BASE_URL__"));
    assert!(raw.contains("kimi-k2.5"));
    assert!(raw.contains("127.0.0.1:19081"));
    assert!(!raw.contains("ailab-pj"));
    assert!(!raw.contains("0.0.0.0"));

    for example in example_files() {
        let contents = std::fs::read_to_string(&example).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !contents.contains("__"),
            "placeholders are reserved for {}: {}",
            OPENCLAW_TEMPLATE,
            example.display()
        );
    }
}
