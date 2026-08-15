//! Private, crash-safe filesystem helpers for capture runtime state.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Create a directory and make it private to the current user.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    set_private_dir_permissions(path)
}

/// Open an append-only runtime-state file and make both it and its parent
/// private to the current user. Existing files are hardened as well.
pub(crate) fn open_private_append_file(path: &Path) -> Result<fs::File> {
    open_private_file(path, true)
}

/// Open and truncate a runtime-state file while preserving private ownership.
pub(crate) fn open_private_truncate_file(path: &Path) -> Result<fs::File> {
    open_private_file(path, false)
}

fn open_private_file(path: &Path, append: bool) -> Result<fs::File> {
    let parent = path
        .parent()
        .with_context(|| format!("private file has no parent: {}", path.display()))?;
    ensure_private_dir(parent)?;

    let mut options = OpenOptions::new();
    options.write(true).create(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options
        .open(path)
        .with_context(|| format!("open private file {}", path.display()))?;
    set_private_file_permissions(path)?;
    Ok(file)
}

/// Atomically replace a file with content readable only by the current user.
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("private file has no parent: {}", path.display()))?;
    ensure_private_dir(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("capture-state");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options
            .open(&temp)
            .with_context(|| format!("create private temporary file {}", temp.display()))?;
        file.write_all(contents)
            .with_context(|| format!("write private temporary file {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync private temporary file {}", temp.display()))?;
        drop(file);

        fs::rename(&temp, path).with_context(|| {
            format!(
                "atomically replace private file {} with {}",
                path.display(),
                temp.display()
            )
        })?;
        set_private_file_permissions(path)?;

        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("sync private directory {}", parent.display()))?;

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("set private directory permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set private file permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_write_hardens_existing_file_and_parent() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("capture");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let path = parent.join("secret.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, b"new-secret").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new-secret");
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_append_hardens_existing_file_and_parent() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("capture");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let path = parent.join("events.jsonl");
        fs::write(&path, b"old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let mut file = open_private_append_file(&path).unwrap();
        file.write_all(b"new\n").unwrap();
        drop(file);

        assert_eq!(fs::read(&path).unwrap(), b"old\nnew\n");
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_replacements_never_leave_partial_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(temp.path().join("capture").join("state.json"));
        let payloads = [
            vec![b'a'; 8 * 1024],
            vec![b'b'; 16 * 1024],
            vec![b'c'; 32 * 1024],
            vec![b'd'; 64 * 1024],
        ];

        std::thread::scope(|scope| {
            for payload in &payloads {
                let path = std::sync::Arc::clone(&path);
                scope.spawn(move || write_private_file(path.as_ref(), payload).unwrap());
            }
        });

        let actual = fs::read(path.as_ref()).unwrap();
        assert!(payloads.iter().any(|payload| payload == &actual));
    }
}
