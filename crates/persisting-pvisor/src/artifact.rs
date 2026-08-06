//! Validation for an explicitly configured delegated pVisor runtime.

use std::path::{Path, PathBuf};

pub(crate) fn resolve_pvisor_binary(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    let path = explicit.ok_or_else(|| {
        anyhow::anyhow!(
            "delegated execution requires an explicit pVisor runtime; set the executor pvisor_binary"
        )
    })?;
    validate_binary(path)
}

fn validate_binary(path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        path.is_file(),
        "pVisor runtime is not a file: {}",
        path.display()
    );
    anyhow::ensure!(
        is_executable(path),
        "pVisor runtime is not executable: {}",
        path.display()
    );
    Ok(path.canonicalize()?)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn explicit_artifact_must_be_executable() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("pvisor");
        std::fs::write(&binary, b"runtime").unwrap();
        assert!(resolve_pvisor_binary(None).is_err());
        assert!(resolve_pvisor_binary(Some(&binary)).is_err());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_pvisor_binary(Some(&binary)).unwrap(),
            binary.canonicalize().unwrap()
        );
    }
}
