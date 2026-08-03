//! Target-specific pVisor runtime artifact discovery.

use crate::config::ContainerPlatform;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_pvisor_binary(
    explicit: Option<&Path>,
    platform: ContainerPlatform,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_binary(path);
    }

    let version = env!("CARGO_PKG_VERSION");
    let platform_dir = platform.artifact_directory();
    let mut candidates = Vec::new();
    if let Some(root) = std::env::var_os("PERSISTING_PVISOR_RUNTIME_DIR") {
        candidates.push(PathBuf::from(root).join(platform_dir).join("pvisor"));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(prefix) = executable.parent().and_then(Path::parent) {
            candidates.push(
                prefix
                    .join("libexec")
                    .join("persisting")
                    .join(version)
                    .join(platform_dir)
                    .join("pvisor"),
            );
        }
        if let Some(target) = executable.parent().and_then(Path::parent) {
            let triple = match platform {
                ContainerPlatform::LinuxAmd64 => "x86_64-unknown-linux-musl",
                ContainerPlatform::LinuxArm64 => "aarch64-unknown-linux-musl",
            };
            candidates.push(target.join(triple).join("release").join("pvisor"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".persisting")
                .join("runtimes")
                .join(version)
                .join(platform_dir)
                .join("pvisor"),
        );
    }

    candidates
        .iter()
        .find(|candidate| is_executable(candidate) && matches_linux_elf(candidate, platform))
        .map(|candidate| candidate.canonicalize())
        .transpose()?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no pVisor runtime artifact for {}; set the executor pvisor_binary or install it under ~/.persisting/runtimes/{version}/{platform_dir}/pvisor (searched: {})",
                platform.oci_value(),
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn matches_linux_elf(path: &Path, platform: ContainerPlatform) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0_u8; 20];
    if file.read_exact(&mut header).is_err()
        || &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
    {
        return false;
    }
    let machine = u16::from_le_bytes([header[18], header[19]]);
    match platform {
        ContainerPlatform::LinuxAmd64 => machine == 62,
        ContainerPlatform::LinuxArm64 => machine == 183,
    }
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
        assert!(resolve_pvisor_binary(Some(&binary), ContainerPlatform::LinuxAmd64).is_err());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            resolve_pvisor_binary(Some(&binary), ContainerPlatform::LinuxAmd64).unwrap(),
            binary.canonicalize().unwrap()
        );
    }
}
