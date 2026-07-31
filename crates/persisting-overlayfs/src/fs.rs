use crate::core::OverlayCore;
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
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

#[derive(Clone, Debug)]
struct Node {
    paths: BTreeSet<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ObjectKey {
    device: u64,
    inode: u64,
}

#[derive(Debug)]
struct DirectoryEntry {
    ino: u64,
    kind: FileType,
    name: OsString,
    attr: FileAttr,
}

pub struct OverlayFs {
    core: OverlayCore,
    nodes: HashMap<u64, Node>,
    by_path: HashMap<PathBuf, u64>,
    by_object: HashMap<ObjectKey, u64>,
    next_ino: u64,
    open_files: HashMap<u64, File>,
    open_directories: HashMap<u64, Vec<DirectoryEntry>>,
    next_handle: u64,
}

fn errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

fn file_type(metadata: &fs::Metadata) -> FileType {
    let kind = metadata.file_type();
    if kind.is_dir() {
        FileType::Directory
    } else if kind.is_symlink() {
        FileType::Symlink
    } else if kind.is_block_device() {
        FileType::BlockDevice
    } else if kind.is_char_device() {
        FileType::CharDevice
    } else if kind.is_fifo() {
        FileType::NamedPipe
    } else if kind.is_socket() {
        FileType::Socket
    } else {
        FileType::RegularFile
    }
}

fn time_value(value: TimeOrNow) -> SystemTime {
    match value {
        TimeOrNow::SpecificTime(time) => time,
        TimeOrNow::Now => SystemTime::now(),
    }
}

impl OverlayFs {
    pub fn new(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let core = OverlayCore::new(lowers, upper, work)?;
        let mut root_paths = BTreeSet::new();
        root_paths.insert(PathBuf::new());
        let mut nodes = HashMap::new();
        nodes.insert(FUSE_ROOT_ID, Node { paths: root_paths });
        let mut by_path = HashMap::new();
        by_path.insert(PathBuf::new(), FUSE_ROOT_ID);
        Ok(Self {
            core,
            nodes,
            by_path,
            by_object: HashMap::new(),
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
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn allocate_inode(&mut self, path: PathBuf, metadata: &fs::Metadata) -> u64 {
        if let Some(ino) = self.by_path.get(&path) {
            return *ino;
        }
        let object = (!metadata.is_dir() && metadata.nlink() > 1).then_some(ObjectKey {
            device: metadata.dev(),
            inode: metadata.ino(),
        });
        if let Some(ino) = object.and_then(|key| self.by_object.get(&key).copied()) {
            self.add_inode_alias(ino, path);
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        let mut paths = BTreeSet::new();
        paths.insert(path.clone());
        self.nodes.insert(ino, Node { paths });
        self.by_path.insert(path, ino);
        if let Some(object) = object {
            self.by_object.insert(object, ino);
        }
        ino
    }

    fn add_inode_alias(&mut self, ino: u64, path: PathBuf) {
        self.by_path.insert(path.clone(), ino);
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.paths.insert(path);
        }
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
        for (old_path, ino) in mappings {
            let suffix = old_path.strip_prefix(old).unwrap_or_else(|_| Path::new(""));
            let new_path = if suffix.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(suffix)
            };
            self.by_path.remove(&old_path);
            self.by_path.insert(new_path.clone(), ino);
            if let Some(node) = self.nodes.get_mut(&ino) {
                node.paths.remove(&old_path);
                node.paths.insert(new_path);
            }
        }
    }

    fn exchange_inode_prefixes(&mut self, first: &Path, second: &Path) {
        let mappings: Vec<_> = self
            .by_path
            .iter()
            .filter_map(|(path, ino)| {
                if path == first || path.starts_with(first) {
                    let suffix = path.strip_prefix(first).ok()?;
                    Some((path.clone(), *ino, second.join(suffix)))
                } else if path == second || path.starts_with(second) {
                    let suffix = path.strip_prefix(second).ok()?;
                    Some((path.clone(), *ino, first.join(suffix)))
                } else {
                    None
                }
            })
            .collect();
        for (old, ino, _) in &mappings {
            self.by_path.remove(old);
            if let Some(node) = self.nodes.get_mut(ino) {
                node.paths.remove(old);
            }
        }
        for (_, ino, new) in mappings {
            self.by_path.insert(new.clone(), ino);
            if let Some(node) = self.nodes.get_mut(&ino) {
                node.paths.insert(new);
            }
        }
    }

    fn allocate_handle(&mut self) -> u64 {
        let handle = self.next_handle;
        self.next_handle += 1;
        handle
    }

    fn attr_from_metadata(ino: u64, metadata: &fs::Metadata) -> FileAttr {
        let mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
        let atime = metadata.accessed().unwrap_or(mtime);
        let ctime = UNIX_EPOCH
            + Duration::new(
                metadata.ctime().max(0) as u64,
                metadata.ctime_nsec().max(0) as u32,
            );
        #[cfg(target_os = "macos")]
        let flags = {
            use std::os::macos::fs::MetadataExt as MacMetadataExt;
            MacMetadataExt::st_flags(metadata)
        };
        #[cfg(not(target_os = "macos"))]
        let flags = 0;
        FileAttr {
            ino,
            size: metadata.len(),
            blocks: metadata.blocks(),
            atime,
            mtime,
            ctime,
            crtime: metadata.created().unwrap_or(ctime),
            kind: file_type(metadata),
            perm: (metadata.mode() & 0o7777) as u16,
            nlink: metadata.nlink().min(u32::MAX as u64) as u32,
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: metadata.rdev() as u32,
            blksize: metadata.blksize().min(u32::MAX as u64) as u32,
            flags,
        }
    }

    fn attr(&self, ino: u64, path: &Path) -> io::Result<FileAttr> {
        Ok(Self::attr_from_metadata(ino, &self.core.metadata(path)?))
    }

    fn child_path(&self, parent: u64, name: &OsStr) -> io::Result<PathBuf> {
        OverlayCore::child(&self.node_path(parent)?, name)
    }

    fn copy_up_inode(&self, ino: u64) -> io::Result<PathBuf> {
        let path = self.node_path(ino)?;
        let aliases = self
            .nodes
            .get(&ino)
            .map(|node| node.paths.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![path.clone()]);
        for alias in aliases {
            self.core.copy_up(&alias)?;
        }
        Ok(path)
    }

    fn directory_snapshot(&mut self, ino: u64) -> io::Result<Vec<DirectoryEntry>> {
        let path = self.node_path(ino)?;
        let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
        let parent_ino = self
            .by_path
            .get(parent_path)
            .copied()
            .unwrap_or(FUSE_ROOT_ID);
        let mut entries = vec![
            DirectoryEntry {
                ino,
                kind: FileType::Directory,
                name: OsString::from("."),
                attr: self.attr(ino, &path)?,
            },
            DirectoryEntry {
                ino: parent_ino,
                kind: FileType::Directory,
                name: OsString::from(".."),
                attr: self
                    .attr(parent_ino, parent_path)
                    .or_else(|_| self.attr(FUSE_ROOT_ID, Path::new("")))?,
            },
        ];
        for name in self.core.list_names(&path)? {
            let child = OverlayCore::child(&path, &name)?;
            let metadata = self.core.metadata(&child)?;
            let child_ino = self.allocate_inode(child, &metadata);
            entries.push(DirectoryEntry {
                ino: child_ino,
                kind: file_type(&metadata),
                name,
                attr: Self::attr_from_metadata(child_ino, &metadata),
            });
        }
        Ok(entries)
    }

    fn open_path(&self, path: &Path, flags: i32) -> io::Result<File> {
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let real = if writing {
            self.core.copy_up(path)?
        } else {
            self.core
                .resolve(path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?
                .path
        };
        let access_mode = flags & libc::O_ACCMODE;
        let mut options = OpenOptions::new();
        options
            .read(access_mode != libc::O_WRONLY)
            .write(access_mode != libc::O_RDONLY)
            .append(flags & libc::O_APPEND != 0)
            .truncate(flags & libc::O_TRUNC != 0)
            .custom_flags(
                flags
                    & !(libc::O_ACCMODE
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_TRUNC
                        | libc::O_APPEND),
            );
        options.open(real)
    }
}

impl Filesystem for OverlayFs {
    fn lookup(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path, &metadata);
            Ok((ino, metadata))
        })();
        match result {
            Ok((ino, metadata)) => reply.entry(&TTL, &Self::attr_from_metadata(ino, &metadata), 0),
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
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            let path = self.copy_up_inode(ino)?;
            let upper = self.core.copy_up(&path)?;
            if let Some(size) = size {
                OpenOptions::new().write(true).open(&upper)?.set_len(size)?;
            }
            if let Some(mode) = mode {
                fs::set_permissions(&upper, fs::Permissions::from_mode(mode & 0o7777))?;
            }
            if uid.is_some() || gid.is_some() {
                let metadata = fs::symlink_metadata(&upper)?;
                sys::chown(
                    &upper,
                    uid.unwrap_or_else(|| metadata.uid()),
                    gid.unwrap_or_else(|| metadata.gid()),
                    metadata.file_type().is_symlink(),
                )?;
            }
            if atime.is_some() || mtime.is_some() {
                let nofollow = fs::symlink_metadata(&upper)?.file_type().is_symlink();
                sys::set_times(
                    &upper,
                    atime.map(time_value),
                    mtime.map(time_value),
                    nofollow,
                )?;
            }
            if let Some(flags) = flags {
                #[cfg(target_os = "macos")]
                sys::set_flags(&upper, flags)?;
                #[cfg(not(target_os = "macos"))]
                if flags != 0 {
                    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
                }
            }
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
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            fs::read_link(resolved.path)
        });
        match result {
            Ok(target) => reply.data(target.as_os_str().as_encoded_bytes()),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mknod(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_node(&path, mode & !umask, rdev)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            self.attr(ino, &path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn mkdir(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_dir(&path, mode & !umask)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
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
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            self.core.create_symlink(&path, target)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
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
        if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0
            || flags == (RENAME_NOREPLACE | RENAME_EXCHANGE)
        {
            reply.error(libc::ENOTSUP);
            return;
        }
        let result = (|| {
            let old = self.child_path(parent, name)?;
            let new = self.child_path(newparent, newname)?;
            if flags & RENAME_EXCHANGE != 0 {
                self.core.exchange(&old, &new)?;
                Ok((old, new, true))
            } else {
                self.core
                    .rename(&old, &new, flags & RENAME_NOREPLACE != 0)?;
                Ok((old, new, false))
            }
        })();
        match result {
            Ok((old, new, exchange)) => {
                if exchange {
                    self.exchange_inode_prefixes(&old, &new);
                } else {
                    self.remap_inode_prefix(&old, &new);
                }
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
            self.core.hard_link(&source, &destination)?;
            self.add_inode_alias(ino, destination.clone());
            self.attr(ino, &destination)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn open(&mut self, _request: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY
            || flags & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let result = (if writing {
            self.copy_up_inode(ino)
        } else {
            self.node_path(ino)
        })
        .and_then(|path| self.open_path(&path, flags));
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
        let Some(file) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let mut data = vec![0; size as usize];
        match file.read_at(&mut data, offset as u64) {
            Ok(read) => {
                data.truncate(read);
                reply.data(&data);
            }
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
        let Some(file) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        match file.write_at(data, offset as u64) {
            Ok(written) => reply.written(written as u32),
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
            Some(file) => match sys::fsync(file, datasync) {
                Ok(()) => reply.ok(),
                Err(error) => reply.error(errno(&error)),
            },
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
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
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
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        let Some(entries) = self.open_directories.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        for (index, entry) in entries.iter().enumerate().skip(offset as usize) {
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
        ino: u64,
        _fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let result = self.node_path(ino).and_then(|path| {
            let upper = self.core.copy_up(&path)?;
            let directory = File::open(upper)?;
            sys::fsync(&directory, datasync)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn statfs(&mut self, _request: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match sys::statfs(self.core.upper()) {
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
        let result = self
            .copy_up_inode(ino)
            .and_then(|path| self.core.copy_up(&path))
            .and_then(|path| sys::set_xattr(&path, name, value, flags));
        match result {
            Ok(()) => reply.ok(),
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
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            sys::get_xattr(&real.path, name)
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
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            let names = sys::list_xattrs(&real.path)?;
            let mut encoded = Vec::new();
            for name in names {
                encoded.extend_from_slice(&name);
                encoded.push(0);
            }
            Ok(encoded)
        });
        match result {
            Ok(value) if size == 0 => reply.size(value.len() as u32),
            Ok(value) if value.len() <= size as usize => reply.data(&value),
            Ok(_) => reply.error(libc::ERANGE),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn removexattr(&mut self, _request: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .copy_up_inode(ino)
            .and_then(|path| self.core.copy_up(&path))
            .and_then(|path| sys::remove_xattr(&path, name));
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn access(&mut self, _request: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        let result = self.node_path(ino).and_then(|path| {
            let real = self
                .core
                .resolve(&path)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
            sys::access(&real.path, mask)
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn create(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let file = self.core.create_file(&path, mode & !umask, flags)?;
            let metadata = self.core.metadata(&path)?;
            let ino = self.allocate_inode(path.clone(), &metadata);
            let attr = self.attr(ino, &path)?;
            Ok((file, attr))
        })();
        match result {
            Ok((file, attr)) => {
                let handle = self.allocate_handle();
                self.open_files.insert(handle, file);
                reply.created(&TTL, &attr, 0, handle, 0);
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
        if offset < 0 || length < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        if mode != 0 {
            reply.error(libc::ENOTSUP);
            return;
        }
        let Some(file) = self.open_files.get(&fh) else {
            reply.error(libc::EBADF);
            return;
        };
        let end = (offset as u64).saturating_add(length as u64);
        let result = file.metadata().and_then(|metadata| {
            if metadata.len() < end {
                file.set_len(end)
            } else {
                Ok(())
            }
        });
        match result {
            Ok(()) => reply.ok(),
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
        match self.open_files.get(&fh) {
            Some(file) => match sys::seek(file, offset, whence) {
                Ok(offset) => reply.offset(offset),
                Err(error) => reply.error(errno(&error)),
            },
            None => reply.error(libc::EBADF),
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
        let Some(input) = self
            .open_files
            .get(&fh_in)
            .and_then(|file| file.try_clone().ok())
        else {
            reply.error(libc::EBADF);
            return;
        };
        let Some(output) = self
            .open_files
            .get(&fh_out)
            .and_then(|file| file.try_clone().ok())
        else {
            reply.error(libc::EBADF);
            return;
        };
        let mut copied = 0_u64;
        let mut buffer = vec![0_u8; (len.min(128 * 1024)) as usize];
        let result = (|| {
            while copied < len {
                let wanted = (len - copied).min(buffer.len() as u64) as usize;
                let read = input.read_at(
                    &mut buffer[..wanted],
                    (offset_in as u64).saturating_add(copied),
                )?;
                if read == 0 {
                    break;
                }
                let mut written = 0;
                while written < read {
                    let amount = output.write_at(
                        &buffer[written..read],
                        (offset_out as u64)
                            .saturating_add(copied)
                            .saturating_add(written as u64),
                    )?;
                    if amount == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "copy_file_range made no progress",
                        ));
                    }
                    written += amount;
                }
                copied += read as u64;
            }
            Ok::<(), io::Error>(())
        })();
        match result {
            Ok(()) => reply.written(copied.min(u32::MAX as u64) as u32),
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
        let result = self
            .node_path(ino)
            .and_then(|path| self.core.metadata(&path));
        match result {
            Ok(metadata) => {
                let created = metadata.created().unwrap_or(UNIX_EPOCH);
                reply.xtimes(created, created);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }
}
