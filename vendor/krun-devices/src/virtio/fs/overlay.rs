//! Portable copy-on-write overlay served directly over virtio-fs.
//!
//! The union semantics live in `persisting-overlay-core`; the existing
//! platform passthrough implementation is retained for Linux permission
//! emulation and for the actual FUSE request I/O on each resolved layer.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use persisting_overlay_core::OverlayCore;

use super::bindings;
use super::filesystem::{
    Context, DirEntry, Entry, Extensions, FileSystem, FsOptions, GetxattrReply, ListxattrReply,
    OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::fuse;
use super::inode_alloc::InodeAllocator;
use super::passthrough::{self, PassthroughFs};

const TTL: Duration = Duration::from_secs(1);
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;

#[derive(Clone, Debug)]
pub struct Config {
    pub lower_dirs: Vec<String>,
    pub upper_dir: String,
    pub work_dir: Option<String>,
    pub excluded_paths: Vec<String>,
    pub semantics: passthrough::PermissionSemantics,
}

#[derive(Clone, Copy, Debug)]
struct Layer(usize);

#[derive(Debug)]
struct FileHandle {
    layer: Layer,
    inode: u64,
    handle: u64,
}

#[derive(Debug)]
struct DirectoryItem {
    ino: u64,
    name: Vec<u8>,
    type_: u32,
}

#[derive(Debug)]
enum Handle {
    File(FileHandle),
    Directory(Vec<DirectoryItem>),
}

#[derive(Default)]
struct Nodes {
    by_inode: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
}

pub struct OverlayFs {
    core: OverlayCore,
    roots: Vec<PathBuf>,
    layers: Vec<PassthroughFs>,
    nodes: Mutex<Nodes>,
    handles: Mutex<HashMap<u64, Handle>>,
    next_inode: AtomicU64,
    next_handle: AtomicU64,
}

impl OverlayFs {
    pub fn new(cfg: Config, inode_alloc: Arc<InodeAllocator>) -> io::Result<Self> {
        if cfg.lower_dirs.is_empty() {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let lowers = cfg.lower_dirs.iter().map(PathBuf::from).collect::<Vec<_>>();
        let upper = PathBuf::from(&cfg.upper_dir);
        let work = cfg.work_dir.as_ref().map(PathBuf::from);
        let excluded = cfg.excluded_paths.iter().map(PathBuf::from).collect();
        let core = OverlayCore::new_with_exclusions(lowers.clone(), upper.clone(), work, excluded)?;

        let mut roots = Vec::with_capacity(lowers.len() + 1);
        roots.push(upper);
        roots.extend(lowers);
        let layers = roots
            .iter()
            .map(|root| {
                PassthroughFs::new(
                    passthrough::Config {
                        root_dir: root.to_string_lossy().into_owned(),
                        semantics: cfg.semantics,
                        attr_timeout: TTL,
                        entry_timeout: TTL,
                        ..Default::default()
                    },
                    inode_alloc.clone(),
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut nodes = Nodes::default();
        nodes.by_inode.insert(fuse::ROOT_ID, PathBuf::new());
        nodes.by_path.insert(PathBuf::new(), fuse::ROOT_ID);
        Ok(Self {
            core,
            roots,
            layers,
            nodes: Mutex::new(nodes),
            handles: Mutex::new(HashMap::new()),
            next_inode: AtomicU64::new(fuse::ROOT_ID + 1),
            next_handle: AtomicU64::new(1),
        })
    }

    fn path(&self, inode: u64) -> io::Result<PathBuf> {
        self.nodes
            .lock()
            .unwrap()
            .by_inode
            .get(&inode)
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn child(&self, parent: u64, name: &CStr) -> io::Result<PathBuf> {
        OverlayCore::child(&self.path(parent)?, OsStr::from_bytes(name.to_bytes()))
    }

    fn allocate_inode(&self, path: PathBuf) -> u64 {
        let mut nodes = self.nodes.lock().unwrap();
        if let Some(inode) = nodes.by_path.get(&path) {
            return *inode;
        }
        let inode = self.next_inode.fetch_add(1, Ordering::Relaxed);
        nodes.by_path.insert(path.clone(), inode);
        nodes.by_inode.insert(inode, path);
        inode
    }

    fn remove_path(&self, prefix: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let paths = nodes
            .by_path
            .keys()
            .filter(|path| *path == prefix || path.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            if let Some(inode) = nodes.by_path.remove(&path) {
                nodes.by_inode.remove(&inode);
            }
        }
    }

    fn remap_path(&self, old: &Path, new: &Path) {
        let mut nodes = self.nodes.lock().unwrap();
        let changes = nodes
            .by_path
            .iter()
            .filter(|(path, _)| *path == old || path.starts_with(old))
            .map(|(path, inode)| {
                let suffix = path.strip_prefix(old).unwrap();
                let replacement = if suffix.as_os_str().is_empty() {
                    new.to_path_buf()
                } else {
                    new.join(suffix)
                };
                (path.clone(), replacement, *inode)
            })
            .collect::<Vec<_>>();
        for (old_path, _, _) in &changes {
            nodes.by_path.remove(old_path);
        }
        for (_, new_path, inode) in changes {
            nodes.by_path.insert(new_path.clone(), inode);
            nodes.by_inode.insert(inode, new_path);
        }
    }

    fn layer(&self, path: &Path) -> io::Result<Layer> {
        let resolved = self
            .core
            .resolve(path)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        if resolved.is_upper {
            return Ok(Layer(0));
        }
        self.roots
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, root)| resolved.path == **root || resolved.path.starts_with(root))
            .map(|(index, _)| Layer(index))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    fn inner_inode(&self, layer: Layer, path: &Path, ctx: Context) -> io::Result<u64> {
        let fs = &self.layers[layer.0];
        let mut inode = fuse::ROOT_ID;
        for component in path.components() {
            let name = CString::new(component.as_os_str().as_bytes())?;
            let entry = fs.lookup(ctx, inode, &name)?;
            if inode != fuse::ROOT_ID {
                fs.forget(ctx, inode, 1);
            }
            inode = entry.inode;
        }
        Ok(inode)
    }

    fn entry(&self, ctx: Context, path: &Path, inode: u64) -> io::Result<Entry> {
        let layer = self.layer(path)?;
        let inner = self.inner_inode(layer, path, ctx)?;
        let (mut attr, timeout) = self.layers[layer.0].getattr(ctx, inner, None)?;
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        attr.st_ino = inode as _;
        Ok(Entry {
            inode,
            generation: 0,
            attr,
            attr_flags: 0,
            attr_timeout: timeout,
            entry_timeout: TTL,
        })
    }

    fn writable_inner(&self, ctx: Context, path: &Path) -> io::Result<u64> {
        self.core.copy_up(path)?;
        self.inner_inode(Layer(0), path, ctx)
    }

    fn upper_parent(&self, ctx: Context, path: &Path) -> io::Result<(u64, CString)> {
        self.core.clear_whiteout(path)?;
        self.core.ensure_upper_parents(path)?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        Ok((
            self.inner_inode(Layer(0), parent, ctx)?,
            CString::new(name.as_bytes())?,
        ))
    }

    fn allocate_handle(&self, handle: Handle) -> u64 {
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(id, handle);
        id
    }

    fn with_file_handle<T>(
        &self,
        id: u64,
        f: impl FnOnce(&PassthroughFs, &FileHandle) -> io::Result<T>,
    ) -> io::Result<T> {
        let handles = self.handles.lock().unwrap();
        match handles.get(&id) {
            Some(Handle::File(handle)) => f(&self.layers[handle.layer.0], handle),
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn dtype(mode: libc::mode_t) -> u32 {
        ((mode & libc::S_IFMT) >> 12) as u32
    }
}

impl FileSystem for OverlayFs {
    type Inode = u64;
    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        let mut options = None;
        for layer in &self.layers {
            let layer_options = layer.init(capable)?;
            options = Some(options.map_or(layer_options, |current| current & layer_options));
        }
        Ok(options.unwrap_or_else(FsOptions::empty))
    }

    fn destroy(&self) {
        self.handles.lock().unwrap().clear();
        for layer in &self.layers {
            layer.destroy();
        }
    }

    fn lookup(&self, ctx: Context, parent: u64, name: &CStr) -> io::Result<Entry> {
        let path = self.child(parent, name)?;
        self.core.metadata(&path)?;
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn getattr(
        &self,
        ctx: Context,
        inode: u64,
        _handle: Option<u64>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let entry = self.entry(ctx, &self.path(inode)?, inode)?;
        Ok((entry.attr, entry.attr_timeout))
    }

    fn setattr(
        &self,
        ctx: Context,
        inode: u64,
        attr: bindings::stat64,
        _handle: Option<u64>,
        valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let path = self.path(inode)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].setattr(ctx, inner, attr, None, valid);
        self.layers[0].forget(ctx, inner, 1);
        let (mut attr, timeout) = result?;
        attr.st_ino = inode as _;
        Ok((attr, timeout))
    }

    fn readlink(&self, ctx: Context, inode: u64) -> io::Result<Vec<u8>> {
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].readlink(ctx, inner);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn symlink(
        &self,
        ctx: Context,
        linkname: &CStr,
        parent: u64,
        name: &CStr,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0].symlink(ctx, linkname, upper_parent, &upper_name, extensions)?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn mknod(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        rdev: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0].mknod(
            ctx,
            upper_parent,
            &upper_name,
            mode,
            rdev,
            umask,
            extensions,
        )?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn mkdir(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<Entry> {
        let path = self.child(parent, name)?;
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        self.layers[0].mkdir(ctx, upper_parent, &upper_name, mode, umask, extensions)?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inode = self.allocate_inode(path.clone());
        self.entry(ctx, &path, inode)
    }

    fn unlink(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let path = self.child(parent, name)?;
        self.core.remove(&path, false)?;
        self.remove_path(&path);
        Ok(())
    }

    fn rmdir(&self, _ctx: Context, parent: u64, name: &CStr) -> io::Result<()> {
        let path = self.child(parent, name)?;
        self.core.remove(&path, true)?;
        self.remove_path(&path);
        Ok(())
    }

    fn rename(
        &self,
        _ctx: Context,
        olddir: u64,
        oldname: &CStr,
        newdir: u64,
        newname: &CStr,
        flags: u32,
    ) -> io::Result<()> {
        let old = self.child(olddir, oldname)?;
        let new = self.child(newdir, newname)?;
        match flags {
            0 => self.core.rename(&old, &new, false)?,
            RENAME_NOREPLACE => self.core.rename(&old, &new, true)?,
            RENAME_EXCHANGE => self.core.exchange(&old, &new)?,
            _ => return Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
        }
        if flags == RENAME_EXCHANGE {
            let marker = PathBuf::from(format!(
                ".pvisor-exchange-{}",
                self.next_inode.load(Ordering::Relaxed)
            ));
            self.remap_path(&old, &marker);
            self.remap_path(&new, &old);
            self.remap_path(&marker, &new);
        } else {
            self.remove_path(&new);
            self.remap_path(&old, &new);
        }
        Ok(())
    }

    fn link(&self, ctx: Context, inode: u64, newparent: u64, newname: &CStr) -> io::Result<Entry> {
        let source = self.path(inode)?;
        let destination = self.child(newparent, newname)?;
        self.core.hard_link(&source, &destination)?;
        let new_inode = self.allocate_inode(destination.clone());
        self.entry(ctx, &destination, new_inode)
    }

    fn open(
        &self,
        ctx: Context,
        inode: u64,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let path = self.path(inode)?;
        let writing = flags as i32 & libc::O_ACCMODE != libc::O_RDONLY
            || flags as i32 & (libc::O_APPEND | libc::O_TRUNC) != 0;
        let layer = if writing {
            self.core.copy_up(&path)?;
            Layer(0)
        } else {
            self.layer(&path)?
        };
        let inner = self.inner_inode(layer, &path, ctx)?;
        let (handle, options) = self.layers[layer.0].open(ctx, inner, kill_priv, flags)?;
        let handle = handle.ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        let id = self.allocate_handle(Handle::File(FileHandle {
            layer,
            inode: inner,
            handle,
        }));
        Ok((Some(id), options))
    }

    fn create(
        &self,
        ctx: Context,
        parent: u64,
        name: &CStr,
        mode: u32,
        kill_priv: bool,
        flags: u32,
        umask: u32,
        extensions: Extensions,
    ) -> io::Result<(Entry, Option<u64>, OpenOptions)> {
        let path = self.child(parent, name)?;
        let inode = self.allocate_inode(path.clone());
        let (upper_parent, upper_name) = self.upper_parent(ctx, &path)?;
        let (mut entry, handle, options) = self.layers[0].create(
            ctx,
            upper_parent,
            &upper_name,
            mode,
            kill_priv,
            flags,
            umask,
            extensions,
        )?;
        if upper_parent != fuse::ROOT_ID {
            self.layers[0].forget(ctx, upper_parent, 1);
        }
        let inner_inode = entry.inode;
        entry.inode = inode;
        entry.attr.st_ino = inode as _;
        let handle = handle.ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        let id = self.allocate_handle(Handle::File(FileHandle {
            layer: Layer(0),
            inode: inner_inode,
            handle,
        }));
        Ok((entry, Some(id), options))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        w: W,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        flags: u32,
    ) -> io::Result<usize> {
        self.with_file_handle(handle, |fs, h| {
            fs.read(ctx, h.inode, h.handle, w, size, offset, lock_owner, flags)
        })
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        r: R,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        delayed_write: bool,
        kill_priv: bool,
        flags: u32,
    ) -> io::Result<usize> {
        self.with_file_handle(handle, |fs, h| {
            fs.write(
                ctx,
                h.inode,
                h.handle,
                r,
                size,
                offset,
                lock_owner,
                delayed_write,
                kill_priv,
                flags,
            )
        })
    }

    fn flush(&self, ctx: Context, _inode: u64, handle: u64, lock_owner: u64) -> io::Result<()> {
        self.with_file_handle(handle, |fs, h| fs.flush(ctx, h.inode, h.handle, lock_owner))
    }

    fn fsync(&self, ctx: Context, _inode: u64, datasync: bool, handle: u64) -> io::Result<()> {
        self.with_file_handle(handle, |fs, h| fs.fsync(ctx, h.inode, datasync, h.handle))
    }

    fn release(
        &self,
        ctx: Context,
        _inode: u64,
        flags: u32,
        handle: u64,
        flush: bool,
        flock_release: bool,
        lock_owner: Option<u64>,
    ) -> io::Result<()> {
        let handle = self.handles.lock().unwrap().remove(&handle);
        match handle {
            Some(Handle::File(h)) => {
                let result = self.layers[h.layer.0].release(
                    ctx,
                    h.inode,
                    flags,
                    h.handle,
                    flush,
                    flock_release,
                    lock_owner,
                );
                self.layers[h.layer.0].forget(ctx, h.inode, 1);
                result
            }
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn statfs(&self, ctx: Context, inode: u64) -> io::Result<bindings::statvfs64> {
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].statfs(ctx, inner);
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        result
    }

    fn setxattr(
        &self,
        ctx: Context,
        inode: u64,
        name: &CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        let path = self.path(inode)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].setxattr(ctx, inner, name, value, flags);
        self.layers[0].forget(ctx, inner, 1);
        result
    }

    fn getxattr(
        &self,
        ctx: Context,
        inode: u64,
        name: &CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].getxattr(ctx, inner, name, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn listxattr(&self, ctx: Context, inode: u64, size: u32) -> io::Result<ListxattrReply> {
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].listxattr(ctx, inner, size);
        self.layers[layer.0].forget(ctx, inner, 1);
        result
    }

    fn removexattr(&self, ctx: Context, inode: u64, name: &CStr) -> io::Result<()> {
        let path = self.path(inode)?;
        let inner = self.writable_inner(ctx, &path)?;
        let result = self.layers[0].removexattr(ctx, inner, name);
        self.layers[0].forget(ctx, inner, 1);
        result
    }

    fn opendir(
        &self,
        ctx: Context,
        inode: u64,
        _flags: u32,
    ) -> io::Result<(Option<u64>, OpenOptions)> {
        let path = self.path(inode)?;
        let mut items = Vec::new();
        for name in self.core.list_names(&path)? {
            let child = OverlayCore::child(&path, &name)?;
            let child_inode = self.allocate_inode(child.clone());
            let entry = self.entry(ctx, &child, child_inode)?;
            items.push(DirectoryItem {
                ino: child_inode,
                name: name.as_bytes().to_vec(),
                type_: Self::dtype(entry.attr.st_mode),
            });
        }
        let handle = self.allocate_handle(Handle::Directory(items));
        Ok((Some(handle), OpenOptions::empty()))
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        _inode: u64,
        handle: u64,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        let handles = self.handles.lock().unwrap();
        let items = match handles.get(&handle) {
            Some(Handle::Directory(items)) => items,
            _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
        };
        for (index, item) in items.iter().enumerate().skip(offset as usize) {
            if add_entry(DirEntry {
                ino: item.ino as _,
                offset: (index + 1) as u64,
                type_: item.type_,
                name: &item.name,
            })? == 0
            {
                break;
            }
        }
        Ok(())
    }

    fn readdirplus<F>(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry, Entry) -> io::Result<usize>,
    {
        let handles = self.handles.lock().unwrap();
        let items = match handles.get(&handle) {
            Some(Handle::Directory(items)) => items,
            _ => return Err(io::Error::from_raw_os_error(libc::EBADF)),
        };
        for (index, item) in items.iter().enumerate().skip(offset as usize) {
            let path = self.path(item.ino)?;
            let entry = self.entry(ctx, &path, item.ino)?;
            if add_entry(
                DirEntry {
                    ino: item.ino as _,
                    offset: (index + 1) as u64,
                    type_: item.type_,
                    name: &item.name,
                },
                entry,
            )? == 0
            {
                break;
            }
        }
        Ok(())
    }

    fn releasedir(&self, _ctx: Context, _inode: u64, _flags: u32, handle: u64) -> io::Result<()> {
        match self.handles.lock().unwrap().remove(&handle) {
            Some(Handle::Directory(_)) => Ok(()),
            _ => Err(io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    fn access(&self, ctx: Context, inode: u64, mask: u32) -> io::Result<()> {
        let path = self.path(inode)?;
        let layer = self.layer(&path)?;
        let inner = self.inner_inode(layer, &path, ctx)?;
        let result = self.layers[layer.0].access(ctx, inner, mask);
        if inner != fuse::ROOT_ID {
            self.layers[layer.0].forget(ctx, inner, 1);
        }
        result
    }

    fn lseek(
        &self,
        ctx: Context,
        _inode: u64,
        handle: u64,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        self.with_file_handle(handle, |fs, h| {
            fs.lseek(ctx, h.inode, h.handle, offset, whence)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_land_in_upper_and_lower_stays_immutable() {
        let temp = tempfile::tempdir().unwrap();
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&lower).unwrap();
        std::fs::write(lower.join("original"), b"lower").unwrap();
        let fs = OverlayFs::new(
            Config {
                lower_dirs: vec![lower.to_string_lossy().into_owned()],
                upper_dir: upper.to_string_lossy().into_owned(),
                work_dir: Some(work.to_string_lossy().into_owned()),
                excluded_paths: Vec::new(),
                semantics: passthrough::PermissionSemantics::LinuxComplete,
            },
            Arc::new(InodeAllocator::new()),
        )
        .unwrap();
        fs.init(FsOptions::empty()).unwrap();
        let ctx = Context {
            uid: 0,
            gid: 0,
            pid: 1,
        };
        let original = CString::new("original").unwrap();
        fs.lookup(ctx, fuse::ROOT_ID, &original).unwrap();
        fs.unlink(ctx, fuse::ROOT_ID, &original).unwrap();
        assert_eq!(std::fs::read(lower.join("original")).unwrap(), b"lower");
        assert!(upper.join(".wh.original").is_file());

        let created = CString::new("created").unwrap();
        let (entry, handle, _) = fs
            .create(
                ctx,
                fuse::ROOT_ID,
                &created,
                libc::S_IFREG as u32 | 0o640,
                false,
                libc::O_RDWR as u32,
                0,
                Extensions::default(),
            )
            .unwrap();
        fs.release(
            ctx,
            entry.inode,
            libc::O_RDWR as u32,
            handle.unwrap(),
            false,
            false,
            None,
        )
        .unwrap();
        assert!(upper.join("created").is_file());
        assert!(!lower.join("created").exists());
    }
}
