//! Snapshot-oriented FUSE filesystem backed by a single durable store.

use crate::db_store::{RedbStore, StoredKind, StoredNode};
use crate::sys;
#[cfg(target_os = "macos")]
use fuser::ReplyXTimes;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyLseek, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow, FUSE_ROOT_ID,
};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

fn error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

fn errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

fn no_xattr() -> io::Error {
    error(
        #[cfg(target_os = "macos")]
        libc::ENOATTR,
        #[cfg(not(target_os = "macos"))]
        libc::ENODATA,
    )
}

fn now() -> (i64, u32) {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (
        duration.as_secs().min(i64::MAX as u64) as i64,
        duration.subsec_nanos(),
    )
}

fn system_time(seconds: i64, nanos: u32) -> SystemTime {
    if seconds < 0 {
        UNIX_EPOCH
    } else {
        UNIX_EPOCH + Duration::new(seconds as u64, nanos.min(999_999_999))
    }
}

fn stored_kind(kind: &fs::FileType) -> StoredKind {
    use std::os::unix::fs::FileTypeExt;
    if kind.is_dir() {
        StoredKind::Directory
    } else if kind.is_symlink() {
        StoredKind::Symlink
    } else if kind.is_block_device() {
        StoredKind::BlockDevice
    } else if kind.is_char_device() {
        StoredKind::CharDevice
    } else if kind.is_fifo() {
        StoredKind::Fifo
    } else if kind.is_socket() {
        StoredKind::Socket
    } else {
        StoredKind::File
    }
}

fn fuse_kind(kind: StoredKind) -> FileType {
    match kind {
        StoredKind::File => FileType::RegularFile,
        StoredKind::Directory => FileType::Directory,
        StoredKind::Symlink => FileType::Symlink,
        StoredKind::BlockDevice => FileType::BlockDevice,
        StoredKind::CharDevice => FileType::CharDevice,
        StoredKind::Fifo => FileType::NamedPipe,
        StoredKind::Socket => FileType::Socket,
    }
}

fn lower_node_metadata(metadata: &Metadata) -> StoredNode {
    let kind = stored_kind(&metadata.file_type());
    #[cfg(target_os = "macos")]
    let flags = {
        use std::os::macos::fs::MetadataExt as MacMetadataExt;
        MacMetadataExt::st_flags(metadata)
    };
    #[cfg(not(target_os = "macos"))]
    let flags = 0;
    StoredNode {
        kind,
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev() as u32,
        size: metadata.len(),
        atime_sec: metadata.atime(),
        atime_nsec: metadata.atime_nsec().max(0) as u32,
        mtime_sec: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec().max(0) as u32,
        ctime_sec: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec().max(0) as u32,
        crtime_sec: metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_secs() as i64)
            .unwrap_or_else(|| metadata.ctime()),
        crtime_nsec: metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.subsec_nanos())
            .unwrap_or_else(|| metadata.ctime_nsec().max(0) as u32),
        flags,
        // Lower payload and xattrs are intentionally lazy. Directory listing
        // and getattr must never read complete file contents.
        data: Vec::new(),
        xattrs: Vec::new(),
    }
}

fn lower_node_for_copy_up(path: &Path, metadata: &Metadata) -> io::Result<StoredNode> {
    let mut node = lower_node_metadata(metadata);
    node.data = match node.kind {
        StoredKind::File => fs::read(path)?,
        StoredKind::Symlink => fs::read_link(path)?.as_os_str().as_bytes().to_vec(),
        _ => Vec::new(),
    };
    node.xattrs = sys::list_xattrs(path)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| {
            sys::get_xattr(path, OsStr::from_bytes(&name))
                .ok()
                .map(|value| (name, value))
        })
        .collect();
    Ok(node)
}

#[derive(Clone, Debug)]
enum Resolved {
    Upper(u64, StoredNode),
    Lower(PathBuf, StoredNode),
}

impl Resolved {
    fn node(&self) -> &StoredNode {
        match self {
            Self::Upper(_, node) | Self::Lower(_, node) => node,
        }
    }
}

struct SnapshotCore {
    lowers: Vec<PathBuf>,
    store: RedbStore,
}

impl SnapshotCore {
    fn new(lowers: Vec<PathBuf>, database_path: PathBuf) -> io::Result<Self> {
        if lowers.is_empty() || lowers.iter().any(|path| !path.is_dir()) {
            return Err(error(libc::ENOTDIR));
        }
        Ok(Self {
            lowers,
            store: RedbStore::open(database_path)?,
        })
    }

    fn validate_rel(path: &Path) -> io::Result<()> {
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    fn validate_name(name: &OsStr) -> io::Result<()> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    fn child(parent: &Path, name: &OsStr) -> io::Result<PathBuf> {
        Self::validate_rel(parent)?;
        Self::validate_name(name)?;
        Ok(if parent.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            parent.join(name)
        })
    }

    fn exists_in_lower(&self, path: &Path) -> bool {
        self.lowers
            .iter()
            .any(|lower| fs::symlink_metadata(lower.join(path)).is_ok())
    }

    fn hidden_by_upper(&self, path: &Path) -> io::Result<bool> {
        let mut current = PathBuf::new();
        for component in path.components() {
            let parent = current.clone();
            current.push(component.as_os_str());
            if self.store.is_whiteout(&current)? || self.store.is_opaque(&parent)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve(&self, path: &Path) -> io::Result<Option<Resolved>> {
        Self::validate_rel(path)?;
        if let Some((id, node)) = self.store.lookup(path)? {
            return Ok(Some(Resolved::Upper(id, node)));
        }
        if self.hidden_by_upper(path)? {
            return Ok(None);
        }
        for lower in &self.lowers {
            let real = lower.join(path);
            if let Ok(metadata) = fs::symlink_metadata(&real) {
                return Ok(Some(Resolved::Lower(real, lower_node_metadata(&metadata))));
            }
        }
        Ok(None)
    }

    fn ensure_parents(&self, path: &Path) -> io::Result<()> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        let mut current = PathBuf::new();
        for component in parent.components() {
            current.push(component.as_os_str());
            if self.store.lookup(&current)?.is_some() {
                continue;
            }
            let Some(resolved) = self.resolve(&current)? else {
                return Err(error(libc::ENOENT));
            };
            if resolved.node().kind != StoredKind::Directory {
                return Err(error(libc::ENOTDIR));
            }
            self.copy_up(&current)?;
        }
        Ok(())
    }

    fn copy_up(&self, path: &Path) -> io::Result<(u64, StoredNode)> {
        if let Some((id, node)) = self.store.lookup(path)? {
            return Ok((id, node));
        }
        let Some(resolved) = self.resolve(path)? else {
            return Err(error(libc::ENOENT));
        };
        self.ensure_parents(path)?;
        let node = match resolved {
            Resolved::Lower(real, _) => {
                let metadata = fs::symlink_metadata(&real)?;
                lower_node_for_copy_up(&real, &metadata)?
            }
            Resolved::Upper(_, node) => node,
        };
        let id = self.store.create(path, &node)?;
        Ok((id, node))
    }

    fn materialize_tree(&self, path: &Path) -> io::Result<()> {
        let node = self
            .resolve(path)?
            .ok_or_else(|| error(libc::ENOENT))?
            .node()
            .clone();
        self.copy_up(path)?;
        if node.kind == StoredKind::Directory {
            for name in self.list_names(path)? {
                self.materialize_tree(&Self::child(path, &name)?)?;
            }
            self.store.set_opaque(path, true)?;
        }
        Ok(())
    }

    fn list_names(&self, path: &Path) -> io::Result<Vec<OsString>> {
        let node = self.resolve(path)?.ok_or_else(|| error(libc::ENOENT))?;
        if node.node().kind != StoredKind::Directory {
            return Err(error(libc::ENOTDIR));
        }
        let mut names = BTreeSet::new();
        if !self.store.is_opaque(path)? {
            for lower in &self.lowers {
                let Ok(entries) = fs::read_dir(lower.join(path)) else {
                    continue;
                };
                names.extend(entries.flatten().map(|entry| entry.file_name()));
            }
        }
        for (name, _) in self.store.list_children(path)? {
            names.insert(OsString::from_vec(name));
        }
        names.retain(|name| {
            Self::child(path, name)
                .ok()
                .and_then(|child| self.store.is_whiteout(&child).ok())
                != Some(true)
        });
        Ok(names.into_iter().collect())
    }

    fn create(&self, path: &Path, node: &StoredNode) -> io::Result<u64> {
        if self.resolve(path)?.is_some() {
            return Err(error(libc::EEXIST));
        }
        self.ensure_parents(path)?;
        self.store.set_whiteout(path, false)?;
        let shadows_lower = self.exists_in_lower(path);
        let id = self.store.create(path, node)?;
        if shadows_lower && node.kind == StoredKind::Directory {
            self.store.set_opaque(path, true)?;
        }
        Ok(id)
    }

    fn remove(&self, path: &Path, directory: bool) -> io::Result<()> {
        let resolved = self.resolve(path)?.ok_or_else(|| error(libc::ENOENT))?;
        if directory {
            if resolved.node().kind != StoredKind::Directory {
                return Err(error(libc::ENOTDIR));
            }
            if !self.list_names(path)?.is_empty() {
                return Err(error(libc::ENOTEMPTY));
            }
        } else if resolved.node().kind == StoredKind::Directory {
            return Err(error(libc::EISDIR));
        }
        match resolved {
            Resolved::Upper(_, node) if node.kind == StoredKind::Directory => {
                self.store.remove_prefix(path)?
            }
            Resolved::Upper(_, _) => self.store.remove_path(path)?,
            Resolved::Lower(_, _) => {}
        }
        if self.exists_in_lower(path) {
            self.store.set_whiteout(path, true)?;
        }
        Ok(())
    }

    fn rename(&self, old: &Path, new: &Path, no_replace: bool) -> io::Result<()> {
        if old == new {
            return Ok(());
        }
        let source = self.resolve(old)?.ok_or_else(|| error(libc::ENOENT))?;
        if source.node().kind == StoredKind::Directory && new.starts_with(old) {
            return Err(error(libc::EINVAL));
        }
        if let Some(destination) = self.resolve(new)? {
            if no_replace {
                return Err(error(libc::EEXIST));
            }
            if source.node().kind == StoredKind::Directory
                && destination.node().kind != StoredKind::Directory
            {
                return Err(error(libc::ENOTDIR));
            }
            if source.node().kind != StoredKind::Directory
                && destination.node().kind == StoredKind::Directory
            {
                return Err(error(libc::EISDIR));
            }
            if destination.node().kind == StoredKind::Directory && !self.list_names(new)?.is_empty()
            {
                return Err(error(libc::ENOTEMPTY));
            }
            self.store.remove_prefix(new)?;
        }
        self.ensure_parents(new)?;
        self.materialize_tree(old)?;
        self.store.set_whiteout(new, false)?;
        self.store.rename_prefix(old, new)?;
        if self.exists_in_lower(old) {
            self.store.set_whiteout(old, true)?;
        }
        Ok(())
    }

    fn exchange(&self, first: &Path, second: &Path) -> io::Result<()> {
        if first == second {
            return Ok(());
        }
        if first.starts_with(second) || second.starts_with(first) {
            return Err(error(libc::EINVAL));
        }
        self.resolve(first)?.ok_or_else(|| error(libc::ENOENT))?;
        self.resolve(second)?.ok_or_else(|| error(libc::ENOENT))?;
        self.materialize_tree(first)?;
        self.materialize_tree(second)?;
        self.store.exchange_prefixes(first, second)
    }
}

#[derive(Clone, Debug)]
struct Node {
    paths: BTreeSet<PathBuf>,
}

enum OpenFile {
    Lower(File),
    Upper { id: u64, flags: i32 },
}

#[derive(Clone)]
struct DirectoryEntry {
    ino: u64,
    kind: FileType,
    name: OsString,
    attr: FileAttr,
}

pub(crate) struct SnapshotFilesystem {
    core: SnapshotCore,
    nodes: HashMap<u64, Node>,
    by_path: HashMap<PathBuf, u64>,
    next_ino: u64,
    open_files: HashMap<u64, OpenFile>,
    open_directories: HashMap<u64, Vec<DirectoryEntry>>,
    next_handle: u64,
}

impl SnapshotFilesystem {
    pub fn new(lowers: Vec<PathBuf>, database_path: PathBuf) -> anyhow::Result<Self> {
        let core = SnapshotCore::new(lowers, database_path)?;
        let mut paths = BTreeSet::new();
        paths.insert(PathBuf::new());
        Ok(Self {
            core,
            nodes: HashMap::from([(FUSE_ROOT_ID, Node { paths })]),
            by_path: HashMap::from([(PathBuf::new(), FUSE_ROOT_ID)]),
            next_ino: FUSE_ROOT_ID + 1,
            open_files: HashMap::new(),
            open_directories: HashMap::new(),
            next_handle: 1,
        })
    }

    fn node_path(&self, ino: u64) -> io::Result<PathBuf> {
        self.nodes
            .get(&ino)
            .and_then(|node| node.paths.iter().next())
            .cloned()
            .ok_or_else(|| error(libc::ENOENT))
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> io::Result<PathBuf> {
        SnapshotCore::child(&self.node_path(parent)?, name)
    }

    fn allocate_inode(&mut self, path: PathBuf) -> u64 {
        if let Some(ino) = self.by_path.get(&path) {
            return *ino;
        }
        if let Ok(Some(Resolved::Upper(id, _))) = self.core.resolve(&path) {
            for (ino, node) in &self.nodes {
                if node.paths.iter().any(|alias| {
                    self.core
                        .store
                        .lookup(alias)
                        .ok()
                        .flatten()
                        .is_some_and(|(candidate, _)| candidate == id)
                }) {
                    let ino = *ino;
                    self.by_path.insert(path.clone(), ino);
                    self.nodes.get_mut(&ino).unwrap().paths.insert(path);
                    return ino;
                }
            }
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.nodes.insert(
            ino,
            Node {
                paths: BTreeSet::from([path.clone()]),
            },
        );
        self.by_path.insert(path, ino);
        ino
    }

    fn allocate_handle(&mut self) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        handle
    }

    fn nlink(&self, path: &Path, resolved: &Resolved) -> u32 {
        match resolved {
            Resolved::Upper(_, node) if node.kind == StoredKind::Directory => {
                2 + self
                    .core
                    .list_names(path)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|name| {
                        self.core
                            .resolve(&SnapshotCore::child(path, name).unwrap_or_default())
                            .ok()
                            .flatten()
                            .is_some_and(|item| item.node().kind == StoredKind::Directory)
                    })
                    .count() as u32
            }
            Resolved::Upper(id, _) => self
                .core
                .store
                .paths_for_node(*id)
                .map(|paths| paths.len().min(u32::MAX as usize) as u32)
                .unwrap_or(1),
            Resolved::Lower(_, _) => 1,
        }
    }

    fn attr(&self, ino: u64, path: &Path) -> io::Result<FileAttr> {
        let resolved = self
            .core
            .resolve(path)?
            .ok_or_else(|| error(libc::ENOENT))?;
        let node = resolved.node();
        Ok(FileAttr {
            ino,
            size: node.size,
            blocks: node.size.div_ceil(512),
            atime: system_time(node.atime_sec, node.atime_nsec),
            mtime: system_time(node.mtime_sec, node.mtime_nsec),
            ctime: system_time(node.ctime_sec, node.ctime_nsec),
            crtime: system_time(node.crtime_sec, node.crtime_nsec),
            kind: fuse_kind(node.kind),
            perm: (node.mode & 0o7777) as u16,
            nlink: self.nlink(path, &resolved),
            uid: node.uid,
            gid: node.gid,
            rdev: node.rdev,
            blksize: 4096,
            flags: node.flags,
        })
    }

    fn remove_inode_prefix(&mut self, prefix: &Path) {
        let paths: Vec<_> = self
            .by_path
            .keys()
            .filter(|path| *path == prefix || path.starts_with(prefix))
            .cloned()
            .collect();
        for path in paths {
            if let Some(ino) = self.by_path.remove(&path) {
                if let Some(node) = self.nodes.get_mut(&ino) {
                    node.paths.remove(&path);
                }
            }
        }
    }

    fn remap_inode_prefix(&mut self, old: &Path, new: &Path) {
        self.remove_inode_prefix(new);
        let mappings: Vec<_> = self
            .by_path
            .iter()
            .filter(|(path, _)| *path == old || path.starts_with(old))
            .map(|(path, ino)| (path.clone(), *ino))
            .collect();
        for (path, ino) in mappings {
            let suffix = path.strip_prefix(old).unwrap_or(Path::new(""));
            let destination = if suffix.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(suffix)
            };
            self.by_path.remove(&path);
            self.by_path.insert(destination.clone(), ino);
            if let Some(node) = self.nodes.get_mut(&ino) {
                node.paths.remove(&path);
                node.paths.insert(destination);
            }
        }
    }

    fn exchange_inode_prefixes(&mut self, first: &Path, second: &Path) {
        let temporary = PathBuf::from(format!(".persisting-exchange-{}", self.next_ino));
        self.remap_inode_prefix(first, &temporary);
        self.remap_inode_prefix(second, first);
        self.remap_inode_prefix(&temporary, second);
    }

    fn copy_up_inode(&self, ino: u64) -> io::Result<(PathBuf, u64)> {
        let path = self.node_path(ino)?;
        let aliases = self
            .nodes
            .get(&ino)
            .map(|node| node.paths.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![path.clone()]);
        let mut primary = None;
        for alias in aliases {
            if let Some(id) = primary {
                if self.core.store.lookup(&alias)?.is_none() {
                    self.core.ensure_parents(&alias)?;
                    self.core.store.link(&alias, id)?;
                }
            } else {
                let (id, _) = self.core.copy_up(&alias)?;
                primary = Some(id);
            }
        }
        Ok((path, primary.ok_or_else(|| error(libc::ENOENT))?))
    }

    fn directory_snapshot(&mut self, ino: u64) -> io::Result<Vec<DirectoryEntry>> {
        let path = self.node_path(ino)?;
        let parent_path = path.parent().unwrap_or(Path::new(""));
        let parent_ino = self
            .by_path
            .get(parent_path)
            .copied()
            .unwrap_or(FUSE_ROOT_ID);
        let mut entries = vec![
            DirectoryEntry {
                ino,
                kind: FileType::Directory,
                name: ".".into(),
                attr: self.attr(ino, &path)?,
            },
            DirectoryEntry {
                ino: parent_ino,
                kind: FileType::Directory,
                name: "..".into(),
                attr: self.attr(parent_ino, parent_path)?,
            },
        ];
        for name in self.core.list_names(&path)? {
            let child = SnapshotCore::child(&path, &name)?;
            let child_ino = self.allocate_inode(child.clone());
            let attr = self.attr(child_ino, &child)?;
            entries.push(DirectoryEntry {
                ino: child_ino,
                kind: attr.kind,
                name,
                attr,
            });
        }
        Ok(entries)
    }

    fn update_node(
        &self,
        id: u64,
        change: impl FnOnce(&mut StoredNode) -> io::Result<()>,
    ) -> io::Result<StoredNode> {
        let mut node = self.core.store.get_node(id)?;
        change(&mut node)?;
        let timestamp = now();
        node.ctime_sec = timestamp.0;
        node.ctime_nsec = timestamp.1;
        self.core.store.put_node(id, &node)?;
        Ok(node)
    }
}

impl Filesystem for SnapshotFilesystem {
    fn lookup(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core
                .resolve(&path)?
                .ok_or_else(|| error(libc::ENOENT))?;
            let ino = self.allocate_inode(path.clone());
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn getattr(&mut self, _request: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let result = self.node_path(ino).and_then(|path| self.attr(ino, &path));
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            let (path, id) = self.copy_up_inode(ino)?;
            self.update_node(id, |node| {
                if let Some(mode) = mode {
                    node.mode = (node.mode & u32::from(libc::S_IFMT)) | (mode & 0o7777);
                }
                if let Some(uid) = uid {
                    node.uid = uid;
                }
                if let Some(gid) = gid {
                    node.gid = gid;
                }
                if let Some(size) = size {
                    if node.kind != StoredKind::File {
                        return Err(error(libc::EINVAL));
                    }
                    node.data.resize(size as usize, 0);
                    node.size = size;
                }
                if let Some(time) = atime {
                    let duration = match time {
                        TimeOrNow::SpecificTime(time) => time,
                        TimeOrNow::Now => SystemTime::now(),
                    }
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                    node.atime_sec = duration.as_secs() as i64;
                    node.atime_nsec = duration.subsec_nanos();
                }
                if let Some(time) = mtime {
                    let duration = match time {
                        TimeOrNow::SpecificTime(time) => time,
                        TimeOrNow::Now => SystemTime::now(),
                    }
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                    node.mtime_sec = duration.as_secs() as i64;
                    node.mtime_nsec = duration.subsec_nanos();
                }
                if let Some(time) = crtime {
                    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
                    node.crtime_sec = duration.as_secs() as i64;
                    node.crtime_nsec = duration.subsec_nanos();
                }
                if let Some(flags) = flags {
                    node.flags = flags;
                }
                Ok(())
            })?;
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn readlink(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyData) {
        let result = self.node_path(ino).and_then(|path| {
            let resolved = self
                .core
                .resolve(&path)?
                .ok_or_else(|| error(libc::ENOENT))?;
            if resolved.node().kind != StoredKind::Symlink {
                return Err(error(libc::EINVAL));
            }
            match resolved {
                Resolved::Upper(_, node) => Ok(node.data),
                Resolved::Lower(path, _) => {
                    Ok(fs::read_link(path)?.as_os_str().as_bytes().to_vec())
                }
            }
        });
        match result {
            Ok(target) => reply.data(&target),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mkdir(
        &mut self,
        request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let node = StoredNode::directory(mode & !umask, request.uid(), request.gid(), now());
            self.core.create(&path, &node)?;
            let ino = self.allocate_inode(path.clone());
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mknod(
        &mut self,
        request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let timestamp = now();
            let kind = match mode & u32::from(libc::S_IFMT) {
                value if value == u32::from(libc::S_IFIFO) => StoredKind::Fifo,
                value if value == u32::from(libc::S_IFCHR) => StoredKind::CharDevice,
                value if value == u32::from(libc::S_IFBLK) => StoredKind::BlockDevice,
                value if value == u32::from(libc::S_IFSOCK) => StoredKind::Socket,
                _ => StoredKind::File,
            };
            let mut node =
                StoredNode::directory(mode & !umask, request.uid(), request.gid(), timestamp);
            node.kind = kind;
            node.mode = (mode & u32::from(libc::S_IFMT)) | (mode & !umask & 0o7777);
            node.rdev = rdev;
            self.core.create(&path, &node)?;
            let ino = self.allocate_inode(path.clone());
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn unlink(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.core.remove(&path, false).map(|()| path));
        match result {
            Ok(path) => {
                self.remove_inode_prefix(&path);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rmdir(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child_path(parent, name)
            .and_then(|path| self.core.remove(&path, true).map(|()| path));
        match result {
            Ok(path) => {
                self.remove_inode_prefix(&path);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn symlink(
        &mut self,
        request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let mut node = StoredNode::directory(0o777, request.uid(), request.gid(), now());
            node.kind = StoredKind::Symlink;
            node.mode = u32::from(libc::S_IFLNK) | 0o777;
            node.data = target.as_os_str().as_bytes().to_vec();
            node.size = node.data.len() as u64;
            self.core.create(&path, &node)?;
            let ino = self.allocate_inode(path.clone());
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rename(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0 || flags & RENAME_EXCHANGE != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
        let result = (|| {
            let old = self.child_path(parent, name)?;
            let new = self.child_path(newparent, newname)?;
            self.core
                .rename(&old, &new, flags & RENAME_NOREPLACE != 0)?;
            Ok((old, new))
        })();
        match result {
            Ok((old, new)) => {
                self.remap_inode_prefix(&old, &new);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn link(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let source = self.node_path(ino)?;
            let destination = self.child_path(newparent, newname)?;
            if self.core.resolve(&destination)?.is_some() {
                return Err(error(libc::EEXIST));
            }
            let (_, id) = self.copy_up_inode(ino)?;
            self.core.ensure_parents(&destination)?;
            self.core.store.link(&destination, id)?;
            self.by_path.insert(destination.clone(), ino);
            self.nodes
                .get_mut(&ino)
                .unwrap()
                .paths
                .insert(destination.clone());
            self.attr(ino, &source)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn open(&mut self, _request: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let result = (|| {
            let path = self.node_path(ino)?;
            let handle = if writing {
                let (_, id) = self.copy_up_inode(ino)?;
                if flags & libc::O_TRUNC != 0 {
                    self.update_node(id, |node| {
                        node.data.clear();
                        node.size = 0;
                        Ok(())
                    })?;
                }
                OpenFile::Upper { id, flags }
            } else {
                match self
                    .core
                    .resolve(&path)?
                    .ok_or_else(|| error(libc::ENOENT))?
                {
                    Resolved::Upper(id, _) => OpenFile::Upper { id, flags },
                    Resolved::Lower(path, _) => OpenFile::Lower(File::open(path)?),
                }
            };
            Ok(handle)
        })();
        match result {
            Ok(file) => {
                let handle = self.allocate_handle();
                self.open_files.insert(handle, file);
                reply.opened(handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn read(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let result = match self.open_files.get(&fh) {
            Some(OpenFile::Lower(file)) => {
                let mut data = vec![0; size as usize];
                file.read_at(&mut data, offset as u64).map(|read| {
                    data.truncate(read);
                    data
                })
            }
            Some(OpenFile::Upper { id, .. }) => self.core.store.get_node(*id).map(|node| {
                let start = (offset as usize).min(node.data.len());
                let end = start.saturating_add(size as usize).min(node.data.len());
                node.data[start..end].to_vec()
            }),
            None => Err(error(libc::EBADF)),
        };
        match result {
            Ok(data) => reply.data(&data),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn write(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(OpenFile::Upper { id, flags }) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let id = *id;
        let append = flags & libc::O_APPEND != 0;
        let result = self.update_node(id, |node| {
            if node.kind != StoredKind::File {
                return Err(error(libc::EINVAL));
            }
            let start = if append {
                node.data.len()
            } else {
                offset as usize
            };
            let end = start
                .checked_add(data.len())
                .ok_or_else(|| error(libc::EFBIG))?;
            if node.data.len() < end {
                node.data.resize(end, 0);
            }
            node.data[start..end].copy_from_slice(data);
            node.size = node.data.len() as u64;
            let timestamp = now();
            node.mtime_sec = timestamp.0;
            node.mtime_nsec = timestamp.1;
            Ok(())
        });
        match result {
            Ok(_) => reply.written(data.len().min(u32::MAX as usize) as u32),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn flush(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        if self.open_files.contains_key(&fh) {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn release(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if self.open_files.remove(&fh).is_some() {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn fsync(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.open_files.get(&fh) {
            Some(OpenFile::Lower(file)) => match sys::fsync(file, datasync) {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            },
            Some(OpenFile::Upper { .. }) => reply.ok(),
            None => reply.error(libc::EBADF),
        }
    }

    fn opendir(&mut self, _request: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.directory_snapshot(ino) {
            Ok(entries) => {
                let handle = self.allocate_handle();
                self.open_directories.insert(handle, entries);
                reply.opened(handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn readdir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset.max(0) as usize) {
            if reply.add(entry.ino, (index + 1) as i64, entry.kind, &entry.name) {
                break;
            }
        }
        reply.ok();
    }

    fn readdirplus(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset.max(0) as usize) {
            if reply.add(
                entry.ino,
                (index + 1) as i64,
                &entry.name,
                &TTL,
                &entry.attr,
                0,
            ) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        if self.open_directories.remove(&fh).is_some() {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn fsyncdir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn statfs(&mut self, _request: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match sys::statfs(self.core.store.path()) {
            Ok(stat) => reply.statfs(
                stat.blocks,
                stat.bfree,
                stat.bavail,
                stat.files,
                stat.ffree,
                stat.bsize,
                stat.namelen,
                stat.frsize,
            ),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn setxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if position != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
        let result = (|| {
            let (_, id) = self.copy_up_inode(ino)?;
            self.update_node(id, |node| {
                let key = name.as_bytes();
                let existing = node.xattrs.iter().position(|(name, _)| name == key);
                if flags & libc::XATTR_CREATE != 0 && existing.is_some() {
                    return Err(error(libc::EEXIST));
                }
                if flags & libc::XATTR_REPLACE != 0 && existing.is_none() {
                    return Err(error(
                        #[cfg(target_os = "macos")]
                        libc::ENOATTR,
                        #[cfg(not(target_os = "macos"))]
                        libc::ENODATA,
                    ));
                }
                if let Some(index) = existing {
                    node.xattrs[index].1 = value.to_vec();
                } else {
                    node.xattrs.push((key.to_vec(), value.to_vec()));
                }
                Ok(())
            })
        })();
        match result {
            Ok(_) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn getxattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let result = self.node_path(ino).and_then(|path| {
            let resolved = self
                .core
                .resolve(&path)?
                .ok_or_else(|| error(libc::ENOENT))?;
            match resolved {
                Resolved::Upper(_, node) => node
                    .xattrs
                    .iter()
                    .find(|(key, _)| key == name.as_bytes())
                    .map(|(_, value)| value.clone())
                    .ok_or_else(no_xattr),
                Resolved::Lower(path, _) => sys::get_xattr(&path, name),
            }
        });
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn listxattr(&mut self, _request: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let result = self.node_path(ino).and_then(|path| {
            let resolved = self
                .core
                .resolve(&path)?
                .ok_or_else(|| error(libc::ENOENT))?;
            let xattrs: Vec<Vec<u8>> = match resolved {
                Resolved::Upper(_, node) => node.xattrs.into_iter().map(|(name, _)| name).collect(),
                Resolved::Lower(path, _) => sys::list_xattrs(&path)?,
            };
            let mut names = Vec::new();
            for name in xattrs {
                names.extend_from_slice(&name);
                names.push(0);
            }
            Ok(names)
        });
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn removexattr(&mut self, _request: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            let (_, id) = self.copy_up_inode(ino)?;
            self.update_node(id, |node| {
                let Some(index) = node
                    .xattrs
                    .iter()
                    .position(|(key, _)| key == name.as_bytes())
                else {
                    return Err(error(
                        #[cfg(target_os = "macos")]
                        libc::ENOATTR,
                        #[cfg(not(target_os = "macos"))]
                        libc::ENODATA,
                    ));
                };
                node.xattrs.remove(index);
                Ok(())
            })
        })();
        match result {
            Ok(_) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn access(&mut self, _request: &Request<'_>, ino: u64, _mask: i32, reply: ReplyEmpty) {
        match self
            .node_path(ino)
            .and_then(|path| self.core.resolve(&path).map(|node| node.is_some()))
        {
            Ok(true) => reply.ok(),
            Ok(false) => reply.error(libc::ENOENT),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn create(
        &mut self,
        request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let mut node =
                StoredNode::directory(mode & !umask, request.uid(), request.gid(), now());
            node.kind = StoredKind::File;
            node.mode = u32::from(libc::S_IFREG) | (mode & !umask & 0o7777);
            let id = self.core.create(&path, &node)?;
            let ino = self.allocate_inode(path.clone());
            Ok((id, ino, self.attr(ino, &path)?))
        })();
        match result {
            Ok((id, ino, attr)) => {
                let handle = self.allocate_handle();
                self.open_files
                    .insert(handle, OpenFile::Upper { id, flags });
                reply.created(&TTL, &attr, 0, handle, 0);
                let _ = ino;
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn fallocate(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        if offset < 0 || length < 0 || mode != 0 {
            reply.error(if mode == 0 {
                libc::EINVAL
            } else {
                libc::ENOTSUP
            });
            return;
        }
        let Some(OpenFile::Upper { id, .. }) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let id = *id;
        let end = (offset as u64).saturating_add(length as u64);
        match self.update_node(id, |node| {
            if node.data.len() < end as usize {
                node.data.resize(end as usize, 0);
                node.size = end;
            }
            Ok(())
        }) {
            Ok(_) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn lseek(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        let result = match self.open_files.get(&fh) {
            Some(OpenFile::Lower(file)) => sys::seek(file, offset, whence),
            Some(OpenFile::Upper { id, .. }) => {
                self.core.store.get_node(*id).and_then(|node| match whence {
                    libc::SEEK_SET | libc::SEEK_DATA if offset >= 0 => Ok(offset),
                    libc::SEEK_END => Ok((node.size as i64).saturating_add(offset)),
                    libc::SEEK_HOLE if offset >= 0 && offset as u64 <= node.size => {
                        Ok(node.size as i64)
                    }
                    _ => Err(error(libc::EINVAL)),
                })
            }
            None => Err(error(libc::EBADF)),
        };
        match result {
            Ok(offset) if offset >= 0 => reply.offset(offset),
            Ok(_) => reply.error(libc::EINVAL),
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_file_range(
        &mut self,
        _request: &Request<'_>,
        _ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        _ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        if offset_in < 0 || offset_out < 0 || flags != 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let input = match self.open_files.get(&fh_in) {
            Some(OpenFile::Lower(file)) => {
                let mut data = vec![0; len.min(u32::MAX as u64) as usize];
                match file.read_at(&mut data, offset_in as u64) {
                    Ok(read) => {
                        data.truncate(read);
                        Ok(data)
                    }
                    Err(error) => Err(error),
                }
            }
            Some(OpenFile::Upper { id, .. }) => self.core.store.get_node(*id).map(|node| {
                let start = (offset_in as usize).min(node.data.len());
                let end = start.saturating_add(len as usize).min(node.data.len());
                node.data[start..end].to_vec()
            }),
            None => Err(error(libc::EBADF)),
        };
        let Some(OpenFile::Upper { id, .. }) = self.open_files.get(&fh_out) else {
            reply.error(libc::EBADF);
            return;
        };
        let output_id = *id;
        let result = input.and_then(|data| {
            self.update_node(output_id, |node| {
                let start = offset_out as usize;
                let end = start
                    .checked_add(data.len())
                    .ok_or_else(|| error(libc::EFBIG))?;
                if node.data.len() < end {
                    node.data.resize(end, 0);
                }
                node.data[start..end].copy_from_slice(&data);
                node.size = node.data.len() as u64;
                let timestamp = now();
                node.mtime_sec = timestamp.0;
                node.mtime_nsec = timestamp.1;
                Ok(())
            })?;
            Ok(data.len())
        });
        match result {
            Ok(written) => reply.written(written.min(u32::MAX as usize) as u32),
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[cfg(target_os = "macos")]
    fn exchange(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            let first = self.child_path(parent, name)?;
            let second = self.child_path(newparent, newname)?;
            self.core.exchange(&first, &second)?;
            Ok((first, second))
        })();
        match result {
            Ok((first, second)) => {
                self.exchange_inode_prefixes(&first, &second);
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    #[cfg(target_os = "macos")]
    fn getxtimes(&mut self, _request: &Request<'_>, ino: u64, reply: ReplyXTimes) {
        let result = self.node_path(ino).and_then(|path| {
            self.core
                .resolve(&path)?
                .map(|resolved| {
                    let node = resolved.node();
                    (
                        system_time(node.crtime_sec, node.crtime_nsec),
                        system_time(node.crtime_sec, node.crtime_nsec),
                    )
                })
                .ok_or_else(|| error(libc::ENOENT))
        });
        match result {
            Ok((backup, created)) => reply.xtimes(backup, created),
            Err(error) => reply.error(errno(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redb_core_merges_lower_and_persists_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("base"), b"before").unwrap();
        let core = SnapshotCore::new(vec![lower], temp.path().join("upper.redb")).unwrap();

        let (id, mut node) = core.copy_up(Path::new("base")).unwrap();
        node.data = b"after".to_vec();
        node.size = 5;
        node.mode = u32::from(libc::S_IFREG) | 0o600;
        node.xattrs.push((b"user.test".to_vec(), b"value".to_vec()));
        core.store.put_node(id, &node).unwrap();

        let resolved = core.resolve(Path::new("base")).unwrap().unwrap();
        assert_eq!(resolved.node().data, b"after");
        assert_eq!(resolved.node().mode & 0o777, 0o600);
        assert_eq!(
            resolved
                .node()
                .xattrs
                .iter()
                .find(|(name, _)| name == b"user.test")
                .map(|(_, value)| value.as_slice()),
            Some(b"value".as_slice())
        );
        assert!(!temp.path().join("upper").exists());
    }

    #[test]
    fn lower_payload_is_loaded_only_when_copied_up() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("base"), b"payload").unwrap();
        let core = SnapshotCore::new(vec![lower], temp.path().join("upper.redb")).unwrap();

        let resolved = core.resolve(Path::new("base")).unwrap().unwrap();
        assert_eq!(resolved.node().size, 7);
        assert!(resolved.node().data.is_empty());

        let (_, copied) = core.copy_up(Path::new("base")).unwrap();
        assert_eq!(copied.data, b"payload");
    }

    #[test]
    fn redb_core_whiteout_hides_lower() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("gone"), b"x").unwrap();
        let core = SnapshotCore::new(vec![lower], temp.path().join("upper.redb")).unwrap();
        core.remove(Path::new("gone"), false).unwrap();
        assert!(core.resolve(Path::new("gone")).unwrap().is_none());
        assert!(core.store.is_whiteout(Path::new("gone")).unwrap());
    }

    #[test]
    fn renaming_lower_directory_materializes_complete_snapshot_tree() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        fs::create_dir_all(lower.join("tree/nested")).unwrap();
        fs::write(lower.join("tree/a"), b"a").unwrap();
        fs::write(lower.join("tree/nested/b"), b"b").unwrap();
        let database = temp.path().join("upper.redb");
        let core = SnapshotCore::new(vec![lower], database).unwrap();

        core.rename(Path::new("tree"), Path::new("moved"), false)
            .unwrap();

        assert!(core.resolve(Path::new("tree")).unwrap().is_none());
        assert!(core.store.is_whiteout(Path::new("tree")).unwrap());
        assert!(core.store.is_opaque(Path::new("moved")).unwrap());
        assert_eq!(
            core.resolve(Path::new("moved/a"))
                .unwrap()
                .unwrap()
                .node()
                .data,
            b"a"
        );
        assert_eq!(
            core.resolve(Path::new("moved/nested/b"))
                .unwrap()
                .unwrap()
                .node()
                .data,
            b"b"
        );
        assert!(!temp.path().join("upper").exists());
    }
}
