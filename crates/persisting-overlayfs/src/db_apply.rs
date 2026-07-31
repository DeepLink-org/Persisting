use crate::db_store::{RedbStore, StoredKind, StoredNode};
use crate::sys;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct RedbUpperStatus {
    pub changed_paths: usize,
    pub whiteouts: usize,
    pub opaque_directories: usize,
    pub generation: u64,
    pub sample_paths: Vec<PathBuf>,
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => {
            remove_path(path)?;
            fs::create_dir(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path),
        Err(error) => Err(error),
    }
}

fn apply_metadata(path: &Path, node: &StoredNode) -> io::Result<()> {
    let nofollow = node.kind == StoredKind::Symlink;
    if let Err(error) = sys::chown(path, node.uid, node.gid, nofollow) {
        if !matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES)) {
            return Err(error);
        }
    }
    if !nofollow {
        fs::set_permissions(path, fs::Permissions::from_mode(node.mode & 0o7777))?;
    }
    let stored_names: Vec<&[u8]> = node
        .xattrs
        .iter()
        .map(|(name, _)| name.as_slice())
        .collect();
    for name in sys::list_xattrs(path).unwrap_or_default() {
        if !stored_names.contains(&name.as_slice()) {
            let _ = sys::remove_xattr(path, OsStr::from_bytes(&name));
        }
    }
    for (name, value) in &node.xattrs {
        if let Err(error) = sys::set_xattr(path, OsStr::from_bytes(name), value, 0) {
            if !matches!(
                error.raw_os_error(),
                Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
            ) {
                return Err(error);
            }
        }
    }
    let atime = UNIX_EPOCH
        + Duration::new(
            node.atime_sec.max(0) as u64,
            node.atime_nsec.min(999_999_999),
        );
    let mtime = UNIX_EPOCH
        + Duration::new(
            node.mtime_sec.max(0) as u64,
            node.mtime_nsec.min(999_999_999),
        );
    if let Err(error) = sys::set_times(path, Some(atime), Some(mtime), nofollow) {
        if !matches!(
            error.raw_os_error(),
            Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
        ) {
            return Err(error);
        }
    }
    #[cfg(target_os = "macos")]
    if !nofollow {
        sys::set_flags(path, node.flags)?;
    }
    Ok(())
}

pub fn redb_upper_status(database_path: &Path) -> io::Result<RedbUpperStatus> {
    if !database_path.exists() {
        return Ok(RedbUpperStatus {
            changed_paths: 0,
            whiteouts: 0,
            opaque_directories: 0,
            generation: 0,
            sample_paths: Vec::new(),
        });
    }
    let store = RedbStore::open(database_path.to_path_buf())?;
    let snapshot = store.snapshot()?;
    let mut sample_paths = snapshot
        .entries
        .iter()
        .map(|(path, _, _)| path.clone())
        .chain(snapshot.whiteouts.iter().cloned())
        .take(32)
        .collect::<Vec<_>>();
    sample_paths.sort();
    Ok(RedbUpperStatus {
        changed_paths: snapshot.entries.len(),
        whiteouts: snapshot.whiteouts.len(),
        opaque_directories: snapshot.opaque.len(),
        generation: snapshot.generation,
        sample_paths,
    })
}

pub fn apply_redb_upper(database_path: &Path, target: &Path) -> io::Result<()> {
    ensure_directory(target)?;
    let store = RedbStore::open(database_path.to_path_buf())?;
    let snapshot = store.snapshot()?;
    let mut entries = snapshot.entries;

    for relative in snapshot.whiteouts {
        remove_path(&target.join(relative))?;
    }
    for relative in snapshot.opaque {
        let directory = target.join(relative);
        ensure_directory(&directory)?;
        for entry in fs::read_dir(&directory)? {
            remove_path(&entry?.path())?;
        }
    }

    entries.sort_by_key(|(path, _, _)| path.components().count());
    for (relative, _, _node) in entries
        .iter()
        .filter(|(_, _, node)| node.kind == StoredKind::Directory)
    {
        ensure_directory(&target.join(relative))?;
    }

    let mut hard_links: HashMap<u64, PathBuf> = HashMap::new();
    for (relative, inode, node) in entries
        .iter()
        .filter(|(_, _, node)| node.kind != StoredKind::Directory)
    {
        let destination = target.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        remove_path(&destination)?;
        match node.kind {
            StoredKind::File => {
                if let Some(existing) = hard_links.get(inode) {
                    fs::hard_link(existing, &destination)?;
                } else {
                    fs::write(&destination, &node.data)?;
                    hard_links.insert(*inode, destination.clone());
                }
            }
            StoredKind::Symlink => {
                std::os::unix::fs::symlink(OsStr::from_bytes(&node.data), &destination)?;
            }
            StoredKind::Fifo
            | StoredKind::BlockDevice
            | StoredKind::CharDevice
            | StoredKind::Socket => sys::mknod(&destination, node.mode, node.rdev)?,
            StoredKind::Directory => unreachable!(),
        }
        apply_metadata(&destination, node)?;
    }

    entries.sort_by_key(|(path, _, _)| std::cmp::Reverse(path.components().count()));
    for (relative, _, node) in entries
        .iter()
        .filter(|(_, _, node)| node.kind == StoredKind::Directory)
    {
        apply_metadata(&target.join(relative), node)?;
    }
    Ok(())
}

pub fn discard_redb_upper(database_path: &Path) -> io::Result<()> {
    match fs::remove_file(database_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn file_node(data: &[u8], mode: u32, uid: u32, gid: u32) -> StoredNode {
        let mut node = StoredNode::directory(mode, uid, gid, (100, 0));
        node.kind = StoredKind::File;
        node.mode = u32::from(libc::S_IFREG) | mode;
        node.data = data.to_vec();
        node.size = data.len() as u64;
        node
    }

    #[test]
    fn apply_restores_snapshot_semantics_and_discard_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let database = temp.path().join("upper.redb");
        fs::create_dir_all(target.join("opaque")).unwrap();
        fs::write(target.join("opaque/lower-only"), b"remove").unwrap();
        fs::write(target.join("deleted"), b"remove").unwrap();
        let target_metadata = fs::metadata(&target).unwrap();
        let uid = target_metadata.uid();
        let gid = target_metadata.gid();

        let store = RedbStore::open(database.clone()).unwrap();
        let directory = StoredNode::directory(0o750, uid, gid, (100, 0));
        store.create(Path::new("opaque"), &directory).unwrap();
        store.set_opaque(Path::new("opaque"), true).unwrap();
        let file = file_node(b"snapshot-data", 0o640, uid, gid);
        let id = store.create(Path::new("opaque/file"), &file).unwrap();
        store.link(Path::new("opaque/hard-link"), id).unwrap();
        let mut symlink = file_node(b"file", 0o777, uid, gid);
        symlink.kind = StoredKind::Symlink;
        symlink.mode = u32::from(libc::S_IFLNK) | 0o777;
        symlink.size = 4;
        store.create(Path::new("opaque/symlink"), &symlink).unwrap();
        store.set_whiteout(Path::new("deleted"), true).unwrap();
        drop(store);

        let status = redb_upper_status(&database).unwrap();
        assert_eq!(status.changed_paths, 4);
        assert_eq!(status.whiteouts, 1);
        assert_eq!(status.opaque_directories, 1);
        assert!(status.generation > 0);

        apply_redb_upper(&database, &target).unwrap();
        assert!(!target.join("deleted").exists());
        assert!(!target.join("opaque/lower-only").exists());
        assert_eq!(
            fs::read(target.join("opaque/file")).unwrap(),
            b"snapshot-data"
        );
        assert_eq!(
            fs::metadata(target.join("opaque/file")).unwrap().mode() & 0o7777,
            0o640
        );
        assert_eq!(
            fs::metadata(target.join("opaque/file")).unwrap().ino(),
            fs::metadata(target.join("opaque/hard-link")).unwrap().ino()
        );
        assert_eq!(
            fs::read_link(target.join("opaque/symlink")).unwrap(),
            PathBuf::from("file")
        );

        discard_redb_upper(&database).unwrap();
        discard_redb_upper(&database).unwrap();
        assert!(!database.exists());
        let empty = redb_upper_status(&database).unwrap();
        assert_eq!(empty.changed_paths, 0);
        assert!(
            !database.exists(),
            "status must not recreate a discarded DB"
        );
    }
}
