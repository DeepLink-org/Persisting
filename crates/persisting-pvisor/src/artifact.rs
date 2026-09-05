//! Resolution and validation of the delegated pVisor runtime injected into a
//! guest. The running executable is the default; an explicit path is only
//! needed when the guest ABI differs from the host, which is why a packaged
//! cross-target pVisor stays configurable.

use anyhow::Context;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_pvisor_binary(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_binary(path);
    }
    let current = std::env::current_exe().context(
        "delegated execution needs a pVisor runtime, but the running executable path is \
         unavailable; set the executor pvisor_binary",
    )?;
    validate_binary(&current).with_context(|| {
        format!(
            "the running pVisor at {} cannot be injected into the guest; set the executor \
             pvisor_binary to a guest-compatible build",
            current.display()
        )
    })
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
        assert!(resolve_pvisor_binary(Some(&binary)).is_err());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_pvisor_binary(Some(&binary)).unwrap(),
            binary.canonicalize().unwrap()
        );
    }

    #[test]
    fn omitted_artifact_defaults_to_the_running_executable() {
        let expected = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(resolve_pvisor_binary(None).unwrap(), expected);
    }
}
