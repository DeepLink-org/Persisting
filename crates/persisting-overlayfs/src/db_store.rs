use bincode::config::standard;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

const SCHEMA_VERSION: u64 = 1;
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const NODES: TableDefinition<u64, &[u8]> = TableDefinition::new("nodes");
const PATHS: TableDefinition<&[u8], u64> = TableDefinition::new("paths");
const WHITEOUTS: TableDefinition<&[u8], u8> = TableDefinition::new("whiteouts");
const OPAQUE: TableDefinition<&[u8], u8> = TableDefinition::new("opaque");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum StoredKind {
    File,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredNode {
    pub kind: StoredKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub size: u64,
    pub atime_sec: i64,
    pub atime_nsec: u32,
    pub mtime_sec: i64,
    pub mtime_nsec: u32,
    pub ctime_sec: i64,
    pub ctime_nsec: u32,
    pub crtime_sec: i64,
    pub crtime_nsec: u32,
    pub flags: u32,
    /// Regular-file bytes or symlink target. Other node kinds keep this empty.
    pub data: Vec<u8>,
    pub xattrs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl StoredNode {
    pub fn directory(mode: u32, uid: u32, gid: u32, now: (i64, u32)) -> Self {
        Self {
            kind: StoredKind::Directory,
            mode: u32::from(libc::S_IFDIR) | (mode & 0o7777),
            uid,
            gid,
            rdev: 0,
            size: 0,
            atime_sec: now.0,
            atime_nsec: now.1,
            mtime_sec: now.0,
            mtime_nsec: now.1,
            ctime_sec: now.0,
            ctime_nsec: now.1,
            crtime_sec: now.0,
            crtime_nsec: now.1,
            flags: 0,
            data: Vec::new(),
            xattrs: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RedbStore {
    database: Arc<Database>,
    path: PathBuf,
}

static OPEN_DATABASES: LazyLock<Mutex<HashMap<PathBuf, Weak<Database>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct StoreSnapshot {
    pub entries: Vec<(PathBuf, u64, StoredNode)>,
    pub whiteouts: Vec<PathBuf>,
    pub opaque: Vec<PathBuf>,
    pub generation: u64,
}

fn io_other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn path_key(path: &Path) -> io::Result<Vec<u8>> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(path.as_os_str().as_bytes().to_vec())
}

fn is_path_or_child(candidate: &[u8], prefix: &[u8]) -> bool {
    candidate == prefix
        || (!prefix.is_empty()
            && candidate.starts_with(prefix)
            && candidate.get(prefix.len()) == Some(&b'/'))
}

fn is_direct_child(candidate: &[u8], parent: &[u8]) -> bool {
    if parent.is_empty() {
        return !candidate.is_empty() && !candidate.contains(&b'/');
    }
    candidate
        .strip_prefix(parent)
        .and_then(|tail| tail.strip_prefix(b"/"))
        .is_some_and(|tail| !tail.is_empty() && !tail.contains(&b'/'))
}

fn encode_node(node: &StoredNode) -> io::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(node, standard()).map_err(io_other)
}

fn decode_node(bytes: &[u8]) -> io::Result<StoredNode> {
    bincode::serde::decode_from_slice(bytes, standard())
        .map(|(node, _)| node)
        .map_err(io_other)
}

impl RedbStore {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let mut open = OPEN_DATABASES.lock().map_err(io_other)?;
        let (database, initialize) = if let Some(database) = open.get(&path).and_then(Weak::upgrade)
        {
            (database, false)
        } else {
            let database = Arc::new(Database::create(&path).map_err(io_other)?);
            open.insert(path.clone(), Arc::downgrade(&database));
            (database, true)
        };
        drop(open);
        if !initialize {
            return Ok(Self { database, path });
        }
        let write = database.begin_write().map_err(io_other)?;
        {
            let mut meta = write.open_table(META).map_err(io_other)?;
            let schema_version = meta
                .get("schema_version")
                .map_err(io_other)?
                .map(|version| version.value());
            match schema_version {
                Some(version) if version != SCHEMA_VERSION => {
                    return Err(io::Error::other(format!(
                        "unsupported overlay database schema {}; expected {SCHEMA_VERSION}",
                        version
                    )));
                }
                None => {
                    meta.insert("schema_version", SCHEMA_VERSION)
                        .map_err(io_other)?;
                    meta.insert("next_inode", 2).map_err(io_other)?;
                    meta.insert("generation", 0).map_err(io_other)?;
                }
                _ => {}
            }
            write.open_table(NODES).map_err(io_other)?;
            write.open_table(PATHS).map_err(io_other)?;
            write.open_table(WHITEOUTS).map_err(io_other)?;
            write.open_table(OPAQUE).map_err(io_other)?;
        }
        write.commit().map_err(io_other)?;
        Ok(Self { database, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lookup(&self, path: &Path) -> io::Result<Option<(u64, StoredNode)>> {
        let key = path_key(path)?;
        let read = self.database.begin_read().map_err(io_other)?;
        let paths = read.open_table(PATHS).map_err(io_other)?;
        let Some(id) = paths.get(key.as_slice()).map_err(io_other)? else {
            return Ok(None);
        };
        let id = id.value();
        let nodes = read.open_table(NODES).map_err(io_other)?;
        let node = nodes
            .get(id)
            .map_err(io_other)?
            .ok_or_else(|| io::Error::other(format!("missing redb inode {id}")))?;
        Ok(Some((id, decode_node(node.value())?)))
    }

    pub fn get_node(&self, id: u64) -> io::Result<StoredNode> {
        let read = self.database.begin_read().map_err(io_other)?;
        let nodes = read.open_table(NODES).map_err(io_other)?;
        let node = nodes
            .get(id)
            .map_err(io_other)?
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        decode_node(node.value())
    }

    pub fn put_node(&self, id: u64, node: &StoredNode) -> io::Result<()> {
        let bytes = encode_node(node)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut nodes = write.open_table(NODES).map_err(io_other)?;
            nodes.insert(id, bytes.as_slice()).map_err(io_other)?;
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn create(&self, path: &Path, node: &StoredNode) -> io::Result<u64> {
        let key = path_key(path)?;
        let bytes = encode_node(node)?;
        let write = self.database.begin_write().map_err(io_other)?;
        let id;
        {
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            if paths.get(key.as_slice()).map_err(io_other)?.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST));
            }
            let mut meta = write.open_table(META).map_err(io_other)?;
            id = meta
                .get("next_inode")
                .map_err(io_other)?
                .map(|value| value.value())
                .unwrap_or(2);
            meta.insert("next_inode", id.saturating_add(1))
                .map_err(io_other)?;
            let generation = meta
                .get("generation")
                .map_err(io_other)?
                .map(|value| value.value())
                .unwrap_or(0);
            meta.insert("generation", generation.saturating_add(1))
                .map_err(io_other)?;
            let mut nodes = write.open_table(NODES).map_err(io_other)?;
            nodes.insert(id, bytes.as_slice()).map_err(io_other)?;
            paths.insert(key.as_slice(), id).map_err(io_other)?;
            let mut whiteouts = write.open_table(WHITEOUTS).map_err(io_other)?;
            whiteouts.remove(key.as_slice()).map_err(io_other)?;
        }
        write.commit().map_err(io_other)?;
        Ok(id)
    }

    pub fn link(&self, path: &Path, id: u64) -> io::Result<()> {
        let key = path_key(path)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let nodes = write.open_table(NODES).map_err(io_other)?;
            if nodes.get(id).map_err(io_other)?.is_none() {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            if paths.get(key.as_slice()).map_err(io_other)?.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST));
            }
            paths.insert(key.as_slice(), id).map_err(io_other)?;
            write
                .open_table(WHITEOUTS)
                .map_err(io_other)?
                .remove(key.as_slice())
                .map_err(io_other)?;
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn remove_path(&self, path: &Path) -> io::Result<()> {
        let key = path_key(path)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            let removed = paths
                .remove(key.as_slice())
                .map_err(io_other)?
                .map(|removed| removed.value());
            if let Some(id) = removed {
                let still_linked = paths
                    .iter()
                    .map_err(io_other)?
                    .any(|entry| entry.is_ok_and(|(_, value)| value.value() == id));
                if !still_linked {
                    write
                        .open_table(NODES)
                        .map_err(io_other)?
                        .remove(id)
                        .map_err(io_other)?;
                }
                Self::bump_generation(&write)?;
            }
            write
                .open_table(OPAQUE)
                .map_err(io_other)?
                .remove(key.as_slice())
                .map_err(io_other)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn remove_prefix(&self, path: &Path) -> io::Result<()> {
        let prefix = path_key(path)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            let keys: Vec<Vec<u8>> = paths
                .iter()
                .map_err(io_other)?
                .filter_map(|entry| entry.ok())
                .map(|(key, _)| key.value().to_vec())
                .filter(|key| is_path_or_child(key, &prefix))
                .collect();
            let mut removed_ids = Vec::new();
            for key in &keys {
                if let Some(value) = paths.remove(key.as_slice()).map_err(io_other)? {
                    removed_ids.push(value.value());
                }
            }
            for id in removed_ids {
                let still_linked = paths
                    .iter()
                    .map_err(io_other)?
                    .any(|entry| entry.is_ok_and(|(_, value)| value.value() == id));
                if !still_linked {
                    write
                        .open_table(NODES)
                        .map_err(io_other)?
                        .remove(id)
                        .map_err(io_other)?;
                }
            }
            for definition in [WHITEOUTS, OPAQUE] {
                let mut table = write.open_table(definition).map_err(io_other)?;
                let keys: Vec<Vec<u8>> = table
                    .iter()
                    .map_err(io_other)?
                    .filter_map(|entry| entry.ok())
                    .map(|(key, _)| key.value().to_vec())
                    .filter(|key| is_path_or_child(key, &prefix))
                    .collect();
                for key in keys {
                    table.remove(key.as_slice()).map_err(io_other)?;
                }
            }
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn rename_prefix(&self, old: &Path, new: &Path) -> io::Result<()> {
        let old = path_key(old)?;
        let new = path_key(new)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            let mappings: Vec<(Vec<u8>, Vec<u8>, u64)> = paths
                .iter()
                .map_err(io_other)?
                .filter_map(|entry| entry.ok())
                .filter_map(|(key, value)| {
                    let key = key.value();
                    is_path_or_child(key, &old).then(|| {
                        let suffix = &key[old.len()..];
                        let mut destination = new.clone();
                        destination.extend_from_slice(suffix);
                        (key.to_vec(), destination, value.value())
                    })
                })
                .collect();
            if mappings.is_empty() {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            for (source, _, _) in &mappings {
                paths.remove(source.as_slice()).map_err(io_other)?;
            }
            for (_, destination, id) in &mappings {
                paths
                    .insert(destination.as_slice(), *id)
                    .map_err(io_other)?;
            }
            for definition in [WHITEOUTS, OPAQUE] {
                let mut table = write.open_table(definition).map_err(io_other)?;
                let mappings: Vec<(Vec<u8>, Vec<u8>, u8)> = table
                    .iter()
                    .map_err(io_other)?
                    .filter_map(|entry| entry.ok())
                    .filter_map(|(key, value)| {
                        let key = key.value();
                        is_path_or_child(key, &old).then(|| {
                            let suffix = &key[old.len()..];
                            let mut destination = new.clone();
                            destination.extend_from_slice(suffix);
                            (key.to_vec(), destination, value.value())
                        })
                    })
                    .collect();
                for (source, _, _) in &mappings {
                    table.remove(source.as_slice()).map_err(io_other)?;
                }
                for (_, destination, value) in mappings {
                    table
                        .insert(destination.as_slice(), value)
                        .map_err(io_other)?;
                }
            }
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn exchange_prefixes(&self, first: &Path, second: &Path) -> io::Result<()> {
        let first = path_key(first)?;
        let second = path_key(second)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut paths = write.open_table(PATHS).map_err(io_other)?;
            let entries: Vec<(Vec<u8>, Vec<u8>, u64)> = paths
                .iter()
                .map_err(io_other)?
                .filter_map(|entry| entry.ok())
                .filter_map(|(key, value)| {
                    let key = key.value();
                    let (source, destination) = if is_path_or_child(key, &first) {
                        (&first, &second)
                    } else if is_path_or_child(key, &second) {
                        (&second, &first)
                    } else {
                        return None;
                    };
                    let mut target = destination.clone();
                    target.extend_from_slice(&key[source.len()..]);
                    Some((key.to_vec(), target, value.value()))
                })
                .collect();
            let first_found = entries
                .iter()
                .any(|(source, _, _)| is_path_or_child(source, &first));
            let second_found = entries
                .iter()
                .any(|(source, _, _)| is_path_or_child(source, &second));
            if !first_found || !second_found {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            for (source, _, _) in &entries {
                paths.remove(source.as_slice()).map_err(io_other)?;
            }
            for (_, destination, id) in &entries {
                paths
                    .insert(destination.as_slice(), *id)
                    .map_err(io_other)?;
            }
            for definition in [WHITEOUTS, OPAQUE] {
                let mut table = write.open_table(definition).map_err(io_other)?;
                let entries: Vec<(Vec<u8>, Vec<u8>, u8)> = table
                    .iter()
                    .map_err(io_other)?
                    .filter_map(|entry| entry.ok())
                    .filter_map(|(key, value)| {
                        let key = key.value();
                        let (source, destination) = if is_path_or_child(key, &first) {
                            (&first, &second)
                        } else if is_path_or_child(key, &second) {
                            (&second, &first)
                        } else {
                            return None;
                        };
                        let mut target = destination.clone();
                        target.extend_from_slice(&key[source.len()..]);
                        Some((key.to_vec(), target, value.value()))
                    })
                    .collect();
                for (source, _, _) in &entries {
                    table.remove(source.as_slice()).map_err(io_other)?;
                }
                for (_, destination, value) in &entries {
                    table
                        .insert(destination.as_slice(), *value)
                        .map_err(io_other)?;
                }
            }
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    pub fn list_children(&self, parent: &Path) -> io::Result<Vec<(Vec<u8>, u64)>> {
        let parent = path_key(parent)?;
        let read = self.database.begin_read().map_err(io_other)?;
        let paths = read.open_table(PATHS).map_err(io_other)?;
        let mut children = Vec::new();
        for entry in paths.iter().map_err(io_other)? {
            let (key, value) = entry.map_err(io_other)?;
            let key = key.value();
            if is_direct_child(key, &parent) {
                let name = if parent.is_empty() {
                    key.to_vec()
                } else {
                    key[parent.len() + 1..].to_vec()
                };
                children.push((name, value.value()));
            }
        }
        Ok(children)
    }

    pub fn paths_for_node(&self, id: u64) -> io::Result<Vec<PathBuf>> {
        let read = self.database.begin_read().map_err(io_other)?;
        let paths = read.open_table(PATHS).map_err(io_other)?;
        let mut result = Vec::new();
        for entry in paths.iter().map_err(io_other)? {
            let (key, value) = entry.map_err(io_other)?;
            if value.value() == id {
                result.push(PathBuf::from(OsStr::from_bytes(key.value())));
            }
        }
        Ok(result)
    }

    pub fn set_whiteout(&self, path: &Path, value: bool) -> io::Result<()> {
        self.set_marker(WHITEOUTS, path, value)
    }

    pub fn is_whiteout(&self, path: &Path) -> io::Result<bool> {
        self.has_marker(WHITEOUTS, path)
    }

    pub fn set_opaque(&self, path: &Path, value: bool) -> io::Result<()> {
        self.set_marker(OPAQUE, path, value)
    }

    pub fn is_opaque(&self, path: &Path) -> io::Result<bool> {
        self.has_marker(OPAQUE, path)
    }

    #[cfg(test)]
    pub fn generation(&self) -> io::Result<u64> {
        let read = self.database.begin_read().map_err(io_other)?;
        let meta = read.open_table(META).map_err(io_other)?;
        Ok(meta
            .get("generation")
            .map_err(io_other)?
            .map(|value| value.value())
            .unwrap_or(0))
    }

    pub fn snapshot(&self) -> io::Result<StoreSnapshot> {
        let read = self.database.begin_read().map_err(io_other)?;
        let paths = read.open_table(PATHS).map_err(io_other)?;
        let nodes = read.open_table(NODES).map_err(io_other)?;
        let mut entries = Vec::new();
        for entry in paths.iter().map_err(io_other)? {
            let (key, id) = entry.map_err(io_other)?;
            let id = id.value();
            let node = nodes
                .get(id)
                .map_err(io_other)?
                .ok_or_else(|| io::Error::other(format!("missing redb inode {id}")))?;
            entries.push((
                PathBuf::from(OsStr::from_bytes(key.value())),
                id,
                decode_node(node.value())?,
            ));
        }
        drop(nodes);
        drop(paths);
        let whiteouts = {
            let table = read.open_table(WHITEOUTS).map_err(io_other)?;
            table
                .iter()
                .map_err(io_other)?
                .map(|entry| {
                    entry
                        .map(|(key, _)| PathBuf::from(OsStr::from_bytes(key.value())))
                        .map_err(io_other)
                })
                .collect::<io::Result<Vec<_>>>()?
        };
        let opaque = {
            let table = read.open_table(OPAQUE).map_err(io_other)?;
            table
                .iter()
                .map_err(io_other)?
                .map(|entry| {
                    entry
                        .map(|(key, _)| PathBuf::from(OsStr::from_bytes(key.value())))
                        .map_err(io_other)
                })
                .collect::<io::Result<Vec<_>>>()?
        };
        let meta = read.open_table(META).map_err(io_other)?;
        let generation = meta
            .get("generation")
            .map_err(io_other)?
            .map(|value| value.value())
            .unwrap_or(0);
        Ok(StoreSnapshot {
            entries,
            whiteouts,
            opaque,
            generation,
        })
    }

    fn set_marker(
        &self,
        definition: TableDefinition<&[u8], u8>,
        path: &Path,
        value: bool,
    ) -> io::Result<()> {
        let key = path_key(path)?;
        let write = self.database.begin_write().map_err(io_other)?;
        {
            let mut table = write.open_table(definition).map_err(io_other)?;
            if value {
                table.insert(key.as_slice(), 1).map_err(io_other)?;
            } else {
                table.remove(key.as_slice()).map_err(io_other)?;
            }
            Self::bump_generation(&write)?;
        }
        write.commit().map_err(io_other)
    }

    fn has_marker(&self, definition: TableDefinition<&[u8], u8>, path: &Path) -> io::Result<bool> {
        let key = path_key(path)?;
        let read = self.database.begin_read().map_err(io_other)?;
        let table = read.open_table(definition).map_err(io_other)?;
        Ok(table.get(key.as_slice()).map_err(io_other)?.is_some())
    }

    fn bump_generation(write: &redb::WriteTransaction) -> io::Result<()> {
        let mut meta = write.open_table(META).map_err(io_other)?;
        let generation = meta
            .get("generation")
            .map_err(io_other)?
            .map(|value| value.value())
            .unwrap_or(0);
        meta.insert("generation", generation.saturating_add(1))
            .map_err(io_other)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_metadata_content_links_and_markers() {
        let temp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(temp.path().join("upper.redb")).unwrap();
        let mut node = StoredNode::directory(0o755, 501, 20, (1, 2));
        node.kind = StoredKind::File;
        node.mode = u32::from(libc::S_IFREG) | 0o640;
        node.data = b"hello".to_vec();
        node.size = 5;
        node.xattrs.push((b"user.test".to_vec(), b"value".to_vec()));

        let id = store.create(Path::new("a"), &node).unwrap();
        store.link(Path::new("b"), id).unwrap();
        store.set_whiteout(Path::new("gone"), true).unwrap();
        store.set_opaque(Path::new("dir"), true).unwrap();

        assert_eq!(
            store.lookup(Path::new("a")).unwrap().unwrap().1.data,
            b"hello"
        );
        assert_eq!(store.paths_for_node(id).unwrap().len(), 2);
        assert!(store.is_whiteout(Path::new("gone")).unwrap());
        assert!(store.is_opaque(Path::new("dir")).unwrap());
        assert!(store.generation().unwrap() >= 4);
    }

    #[test]
    fn rename_moves_entire_prefix_without_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(temp.path().join("upper.redb")).unwrap();
        let dir = StoredNode::directory(0o755, 1, 1, (1, 0));
        let mut file = dir.clone();
        file.kind = StoredKind::File;
        file.mode = u32::from(libc::S_IFREG) | 0o644;
        file.data = b"x".to_vec();
        file.size = 1;
        store.create(Path::new("old"), &dir).unwrap();
        store.create(Path::new("old/file"), &file).unwrap();
        store.set_opaque(Path::new("old"), true).unwrap();

        store
            .rename_prefix(Path::new("old"), Path::new("new"))
            .unwrap();
        assert!(store.lookup(Path::new("old")).unwrap().is_none());
        assert!(store.lookup(Path::new("new/file")).unwrap().is_some());
        assert!(store.is_opaque(Path::new("new")).unwrap());
    }

    #[test]
    fn exchange_swaps_prefixes_in_one_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(temp.path().join("upper.redb")).unwrap();
        let directory = StoredNode::directory(0o755, 501, 20, (1, 0));
        let mut first = directory.clone();
        first.kind = StoredKind::File;
        first.data = b"first".to_vec();
        first.size = 5;
        let mut second = first.clone();
        second.data = b"second".to_vec();
        second.size = 6;
        store.create(Path::new("a"), &directory).unwrap();
        store.create(Path::new("a/file"), &first).unwrap();
        store.create(Path::new("b"), &directory).unwrap();
        store.create(Path::new("b/file"), &second).unwrap();

        store
            .exchange_prefixes(Path::new("a"), Path::new("b"))
            .unwrap();
        assert_eq!(
            store.lookup(Path::new("a/file")).unwrap().unwrap().1.data,
            b"second"
        );
        assert_eq!(
            store.lookup(Path::new("b/file")).unwrap().unwrap().1.data,
            b"first"
        );
    }

    #[test]
    fn database_reopen_preserves_content_metadata_links_and_markers() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("upper.redb");
        let mut file = StoredNode::directory(0o750, 501, 20, (10, 20));
        file.kind = StoredKind::File;
        file.mode = u32::from(libc::S_IFREG) | 0o640;
        file.data = b"durable".to_vec();
        file.size = file.data.len() as u64;
        file.mtime_sec = 1234;
        file.xattrs
            .push((b"user.persisting".to_vec(), b"snapshot".to_vec()));

        let generation = {
            let store = RedbStore::open(database.clone()).unwrap();
            let id = store.create(Path::new("primary"), &file).unwrap();
            store.link(Path::new("alias"), id).unwrap();
            store.set_whiteout(Path::new("removed"), true).unwrap();
            store.set_opaque(Path::new("opaque"), true).unwrap();
            store.generation().unwrap()
        };

        let reopened = RedbStore::open(database).unwrap();
        let (primary_id, primary) = reopened.lookup(Path::new("primary")).unwrap().unwrap();
        let (alias_id, _) = reopened.lookup(Path::new("alias")).unwrap().unwrap();
        assert_eq!(primary_id, alias_id);
        assert_eq!(primary.data, b"durable");
        assert_eq!(primary.mode & 0o7777, 0o640);
        assert_eq!(primary.mtime_sec, 1234);
        assert_eq!(
            primary.xattrs,
            vec![(b"user.persisting".to_vec(), b"snapshot".to_vec())]
        );
        assert!(reopened.is_whiteout(Path::new("removed")).unwrap());
        assert!(reopened.is_opaque(Path::new("opaque")).unwrap());
        assert_eq!(reopened.generation().unwrap(), generation);
    }

    #[test]
    fn database_handle_is_shared_for_owner_mediated_read_only_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("upper.redb");
        let first = RedbStore::open(database.clone()).unwrap();
        let second = RedbStore::open(database).unwrap();
        let node = StoredNode::directory(0o755, 501, 20, (1, 0));
        first.create(Path::new("shared"), &node).unwrap();
        assert!(second.lookup(Path::new("shared")).unwrap().is_some());
    }
}
