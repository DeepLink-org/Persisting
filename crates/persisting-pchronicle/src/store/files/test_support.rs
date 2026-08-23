use std::path::{Path, PathBuf};

pub(crate) fn fixture_path(relative_path: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative_path)
}
