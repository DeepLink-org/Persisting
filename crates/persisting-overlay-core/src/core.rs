use crate::sys;
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

pub const WHITEOUT_PREFIX: &str = ".wh.";
pub const OPAQUE_NAME: &str = ".wh..wh..opq";
const TEMP_PREFIX: &str = ".wh..persisting-copyup-";
const OPAQUE_XATTRS: [&str; 3] = [
    "trusted.overlay.opaque",
    "user.overlay.opaque",
    "user.fuseoverlayfs.opaque",
];

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct Resolved {
    pub path: PathBuf,
    pub is_upper: bool,
}

#[derive(Debug)]
pub struct OverlayCore {
    lowers: Vec<PathBuf>,
    upper: PathBuf,
    work: Option<PathBuf>,
    excluded: BTreeSet<PathBuf>,
    copied_hard_links: Mutex<HashMap<(u64, u64), PathBuf>>,
}

fn error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn ignorable_metadata_error(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOTSUP)
    )
}

fn ignorable_ownership_error(err: &io::Error) -> bool {
    // A uid or gid outside the current user namespace is reported as EINVAL.
    // Ownership is best-effort for an unprivileged overlay, just like EPERM.
    ignorable_metadata_error(err) || err.raw_os_error() == Some(libc::EINVAL)
}

impl OverlayCore {
    pub fn new(lowers: Vec<PathBuf>, upper: PathBuf, work: Option<PathBuf>) -> io::Result<Self> {
        Self::new_with_exclusions(lowers, upper, work, Vec::new())
    }

    pub fn new_with_exclusions(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
    ) -> io::Result<Self> {
        if lowers.is_empty() {
            return Err(error(libc::EINVAL));
        }
        for lower in &lowers {
            if !lower.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
        }
        fs::create_dir_all(&upper)?;
        if let Some(work) = &work {
            fs::create_dir_all(work)?;
            if work == &upper || work.starts_with(&upper) || upper.starts_with(work) {
                return Err(error(libc::EINVAL));
            }
            if fs::metadata(work)?.dev() != fs::metadata(&upper)?.dev() {
                return Err(error(libc::EXDEV));
            }
            for entry in fs::read_dir(work)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .as_bytes()
                    .starts_with(TEMP_PREFIX.as_bytes())
                {
                    let path = entry.path();
                    if fs::symlink_metadata(&path)?.is_dir() {
                        fs::remove_dir_all(path)?;
                    } else {
                        fs::remove_file(path)?;
                    }
                }
            }
        }
        let excluded = excluded
            .into_iter()
            .map(|path| {
                Self::validate_rel(&path)?;
                if path.as_os_str().is_empty() {
                    return Err(error(libc::EINVAL));
                }
                Ok(path)
            })
            .collect::<io::Result<BTreeSet<_>>>()?;
        let core = Self {
            lowers,
            upper,
            work,
            excluded,
            copied_hard_links: Mutex::new(HashMap::new()),
        };
        if fs::read_dir(&core.upper)?.next().is_none() {
            if let Some(root) = core.lowers.first() {
                let metadata = fs::symlink_metadata(root)?;
                core.copy_metadata(root, &core.upper, &metadata)?;
            }
        }
        Ok(core)
    }

    pub fn upper(&self) -> &Path {
        &self.upper
    }

    fn is_excluded(&self, rel: &Path) -> bool {
        self.excluded
            .iter()
            .any(|prefix| rel == prefix || rel.starts_with(prefix))
    }

    fn require_visible(&self, rel: &Path) -> io::Result<()> {
        Self::validate_rel(rel)?;
        if self.is_excluded(rel) {
            return Err(error(libc::ENOENT));
        }
        Ok(())
    }

    pub fn validate_rel(rel: &Path) -> io::Result<()> {
        if rel.is_absolute()
            || rel
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    pub fn validate_name(name: &OsStr) -> io::Result<()> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.starts_with(WHITEOUT_PREFIX.as_bytes())
        {
            return Err(error(libc::EINVAL));
        }
        Ok(())
    }

    pub fn child(parent: &Path, name: &OsStr) -> io::Result<PathBuf> {
        Self::validate_rel(parent)?;
        Self::validate_name(name)?;
        Ok(if parent.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            parent.join(name)
        })
    }

    pub fn upper_path(&self, rel: &Path) -> PathBuf {
        if rel.as_os_str().is_empty() {
            self.upper.clone()
        } else {
            self.upper.join(rel)
        }
    }

    fn whiteout_path(&self, parent: &Path, name: &OsStr) -> PathBuf {
        let mut marker = OsString::from(WHITEOUT_PREFIX);
        marker.push(name);
        self.upper_path(parent).join(marker)
    }

    pub fn is_whiteout_name(name: &OsStr) -> bool {
        name.as_bytes().starts_with(WHITEOUT_PREFIX.as_bytes())
    }

    fn is_whiteouted(&self, parent: &Path, name: &OsStr) -> bool {
        exists(&self.whiteout_path(parent, name))
    }

    pub fn is_opaque(&self, rel: &Path) -> bool {
        let path = self.upper_path(rel);
        exists(&path.join(OPAQUE_NAME))
            || OPAQUE_XATTRS.iter().any(|name| {
                sys::get_xattr(&path, OsStr::new(name)).is_ok_and(|value| value == b"y")
            })
    }

    fn resolve_component(&self, rel: &Path) -> Option<Resolved> {
        let upper = self.upper_path(rel);
        if exists(&upper) {
            return Some(Resolved {
                path: upper,
                is_upper: true,
            });
        }
        let name = rel.file_name()?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        if self.is_whiteouted(parent, name) || self.is_opaque(parent) {
            return None;
        }
        self.lowers.iter().find_map(|lower| {
            let path = lower.join(rel);
            exists(&path).then_some(Resolved {
                path,
                is_upper: false,
            })
        })
    }

    pub fn resolve(&self, rel: &Path) -> Option<Resolved> {
        if self.require_visible(rel).is_err() {
            return None;
        }
        if rel.as_os_str().is_empty() {
            return Some(Resolved {
                path: self.upper.clone(),
                is_upper: true,
            });
        }
        let mut current = PathBuf::new();
        let mut resolved = None;
        let count = rel.components().count();
        for (index, component) in rel.components().enumerate() {
            current.push(component.as_os_str());
            let item = self.resolve_component(&current)?;
            if index + 1 != count {
                let metadata = fs::symlink_metadata(&item.path).ok()?;
                if !metadata.is_dir() {
                    return None;
                }
            }
            resolved = Some(item);
        }
        resolved
    }

    pub fn metadata(&self, rel: &Path) -> io::Result<Metadata> {
        self.require_visible(rel)?;
        let resolved = self.resolve(rel).ok_or_else(|| error(libc::ENOENT))?;
        fs::symlink_metadata(resolved.path)
    }

    pub fn exists_in_lower(&self, rel: &Path) -> bool {
        if self.require_visible(rel).is_err() {
            return false;
        }
        self.lowers.iter().any(|lower| exists(&lower.join(rel)))
    }

    fn copy_metadata(
        &self,
        source: &Path,
        destination: &Path,
        metadata: &Metadata,
    ) -> io::Result<()> {
        let nofollow = metadata.file_type().is_symlink();
        if let Err(err) = sys::chown(destination, metadata.uid(), metadata.gid(), nofollow) {
            if !ignorable_ownership_error(&err) {
                return Err(err);
            }
        }
        if !nofollow {
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode(metadata.mode() & 0o7777),
            )?;
        }
        if let Err(err) = sys::copy_xattrs(source, destination) {
            if !ignorable_metadata_error(&err) {
                return Err(err);
            }
        }
        let atime = UNIX_EPOCH
            + Duration::new(metadata.atime().max(0) as u64, metadata.atime_nsec() as u32);
        let mtime = UNIX_EPOCH
            + Duration::new(metadata.mtime().max(0) as u64, metadata.mtime_nsec() as u32);
        if let Err(err) = sys::set_times(destination, Some(atime), Some(mtime), nofollow) {
            if !ignorable_metadata_error(&err) {
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn ensure_upper_parents(&self, rel: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        let Some(parent) = rel.parent() else {
            return Ok(());
        };
        let mut current = PathBuf::new();
        for component in parent.components() {
            current.push(component.as_os_str());
            let upper = self.upper_path(&current);
            if exists(&upper) {
                if !fs::symlink_metadata(&upper)?.is_dir() {
                    return Err(error(libc::ENOTDIR));
                }
                continue;
            }
            let resolved = self.resolve(&current).ok_or_else(|| error(libc::ENOENT))?;
            let metadata = fs::symlink_metadata(&resolved.path)?;
            if !metadata.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
            fs::create_dir(&upper)?;
            self.copy_metadata(&resolved.path, &upper, &metadata)?;
        }
        Ok(())
    }

    fn temporary_path(&self, parent: &Path) -> PathBuf {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        self.work
            .as_deref()
            .unwrap_or(parent)
            .join(format!("{TEMP_PREFIX}{}-{id}", std::process::id()))
    }

    pub fn copy_up(&self, rel: &Path) -> io::Result<PathBuf> {
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        let upper = self.upper_path(rel);
        if exists(&upper) {
            return Ok(upper);
        }
        let resolved = self.resolve(rel).ok_or_else(|| error(libc::ENOENT))?;
        if resolved.is_upper {
            return Ok(resolved.path);
        }
        self.ensure_upper_parents(rel)?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        let parent = upper.parent().ok_or_else(|| error(libc::EINVAL))?;
        let temporary = self.temporary_path(parent);
        let result = (|| {
            let kind = metadata.file_type();
            if kind.is_dir() {
                fs::create_dir(&temporary)?;
            } else if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&resolved.path)?, &temporary)?;
            } else if kind.is_file() {
                let identity = (metadata.dev(), metadata.ino());
                let existing = if metadata.nlink() > 1 {
                    self.copied_hard_links
                        .lock()
                        .ok()
                        .and_then(|links| links.get(&identity).cloned())
                        .filter(|path| exists(path))
                } else {
                    None
                };
                if let Some(existing) = existing {
                    fs::hard_link(existing, &temporary)?;
                } else {
                    let mut options = OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(metadata.mode() & 0o7777);
                    let mut destination = options.open(&temporary)?;
                    let mut source = File::open(&resolved.path)?;
                    io::copy(&mut source, &mut destination)?;
                }
            } else {
                sys::mknod(&temporary, metadata.mode(), metadata.rdev() as u32)?;
            }
            self.copy_metadata(&resolved.path, &temporary, &metadata)?;
            fs::rename(&temporary, &upper)?;
            if metadata.is_file() && metadata.nlink() > 1 {
                if let Ok(mut links) = self.copied_hard_links.lock() {
                    links.insert((metadata.dev(), metadata.ino()), upper.clone());
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = if temporary.is_dir() {
                fs::remove_dir_all(&temporary)
            } else {
                fs::remove_file(&temporary)
            };
        }
        result.map(|()| upper)
    }

    pub fn list_names(&self, rel: &Path) -> io::Result<Vec<OsString>> {
        self.require_visible(rel)?;
        let metadata = self.metadata(rel)?;
        if !metadata.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        let mut names = BTreeSet::new();
        if !self.is_opaque(rel) {
            for lower in &self.lowers {
                let directory = lower.join(rel);
                let Ok(entries) = fs::read_dir(directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if !Self::is_whiteout_name(&name) {
                        names.insert(name);
                    }
                }
            }
        }
        if let Ok(entries) = fs::read_dir(self.upper_path(rel)) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !Self::is_whiteout_name(&name) {
                    names.insert(name);
                }
            }
        }
        names.retain(|name| {
            !self.is_whiteouted(rel, name)
                && Self::child(rel, name).is_ok_and(|child| !self.is_excluded(&child))
        });
        Ok(names.into_iter().collect())
    }

    fn mark_opaque(&self, rel: &Path) -> io::Result<()> {
        let path = self.upper_path(rel);
        let marker = path.join(OPAQUE_NAME);
        if !exists(&marker) {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(marker)?;
        }
        for name in OPAQUE_XATTRS {
            match sys::set_xattr(&path, OsStr::new(name), b"y", 0) {
                Ok(()) => break,
                Err(error) if ignorable_metadata_error(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn create_whiteout(&self, rel: &Path) -> io::Result<()> {
        self.ensure_upper_parents(rel)?;
        let name = rel.file_name().ok_or_else(|| error(libc::EINVAL))?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let marker = self.whiteout_path(parent, name);
        if !exists(&marker) {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(marker)?;
        }
        Ok(())
    }

    pub fn clear_whiteout(&self, rel: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        let name = rel.file_name().ok_or_else(|| error(libc::EINVAL))?;
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let marker = self.whiteout_path(parent, name);
        match fs::remove_file(marker) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub fn create_file(&self, rel: &Path, mode: u32, flags: i32) -> io::Result<File> {
        self.require_visible(rel)?;
        Self::validate_rel(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        let access_mode = flags & libc::O_ACCMODE;
        let mut options = OpenOptions::new();
        options
            .read(access_mode != libc::O_WRONLY)
            .write(access_mode != libc::O_RDONLY)
            .append(flags & libc::O_APPEND != 0)
            .truncate(flags & libc::O_TRUNC != 0)
            .create_new(true)
            .mode(mode & 0o7777)
            .custom_flags(flags & !(libc::O_ACCMODE | libc::O_CREAT | libc::O_EXCL));
        options.open(self.upper_path(rel))
    }

    pub fn create_dir(&self, rel: &Path, mode: u32) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        let shadows_lower = self.exists_in_lower(rel);
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        let path = self.upper_path(rel);
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o7777))?;
        if shadows_lower {
            self.mark_opaque(rel)?;
        }
        Ok(())
    }

    pub fn create_symlink(&self, rel: &Path, target: &Path) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        std::os::unix::fs::symlink(target, self.upper_path(rel))
    }

    pub fn create_node(&self, rel: &Path, mode: u32, rdev: u32) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        sys::mknod(&self.upper_path(rel), mode, rdev)
    }

    pub fn remove(&self, rel: &Path, directory: bool) -> io::Result<()> {
        self.require_visible(rel)?;
        let resolved = self.resolve(rel).ok_or_else(|| error(libc::ENOENT))?;
        let metadata = fs::symlink_metadata(&resolved.path)?;
        if directory {
            if !metadata.is_dir() {
                return Err(error(libc::ENOTDIR));
            }
            if !self.list_names(rel)?.is_empty() {
                return Err(error(libc::ENOTEMPTY));
            }
        } else if metadata.is_dir() {
            return Err(error(libc::EISDIR));
        }

        if resolved.is_upper {
            if directory {
                fs::remove_dir_all(&resolved.path)?;
            } else {
                fs::remove_file(&resolved.path)?;
            }
        }
        if self.exists_in_lower(rel) {
            self.create_whiteout(rel)?;
        }
        Ok(())
    }

    /// Materialize the complete merged subtree and make its root opaque.
    ///
    /// This is required before renaming a lower-backed directory: a single-node
    /// copy-up would otherwise lose every child when the upper directory moves.
    pub fn materialize_tree(&self, rel: &Path) -> io::Result<PathBuf> {
        self.require_visible(rel)?;
        let metadata = self.metadata(rel)?;
        if !metadata.is_dir() {
            return self.copy_up(rel);
        }
        let names = self.list_names(rel)?;
        self.copy_up(rel)?;
        for name in names {
            let child = Self::child(rel, &name)?;
            if self.metadata(&child)?.is_dir() {
                self.materialize_tree(&child)?;
            } else {
                self.copy_up(&child)?;
            }
        }
        self.mark_opaque(rel)?;
        Ok(self.upper_path(rel))
    }

    fn validate_replacement(
        &self,
        old: &Path,
        new: &Path,
        no_replace: bool,
    ) -> io::Result<Option<PathBuf>> {
        let Some(destination) = self.resolve(new) else {
            return Ok(None);
        };
        if no_replace {
            return Err(error(libc::EEXIST));
        }
        let source_meta = self.metadata(old)?;
        let destination_meta = fs::symlink_metadata(&destination.path)?;
        match (source_meta.is_dir(), destination_meta.is_dir()) {
            (true, false) => return Err(error(libc::ENOTDIR)),
            (false, true) => return Err(error(libc::EISDIR)),
            (true, true) if !self.list_names(new)?.is_empty() => {
                return Err(error(libc::ENOTEMPTY));
            }
            _ => {}
        }
        Ok(destination.is_upper.then_some(destination.path))
    }

    fn remove_physical(path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path)?.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    fn remap_copied_hard_links(&self, old: &Path, new: &Path) {
        let Ok(mut links) = self.copied_hard_links.lock() else {
            return;
        };
        for path in links.values_mut() {
            if path == old || path.starts_with(old) {
                if let Ok(suffix) = path.strip_prefix(old) {
                    *path = if suffix.as_os_str().is_empty() {
                        new.to_path_buf()
                    } else {
                        new.join(suffix)
                    };
                }
            }
        }
    }

    fn exchange_copied_hard_links(&self, first: &Path, second: &Path) {
        let Ok(mut links) = self.copied_hard_links.lock() else {
            return;
        };
        for path in links.values_mut() {
            if path == first || path.starts_with(first) {
                if let Ok(suffix) = path.strip_prefix(first) {
                    *path = if suffix.as_os_str().is_empty() {
                        second.to_path_buf()
                    } else {
                        second.join(suffix)
                    };
                }
            } else if path == second || path.starts_with(second) {
                if let Ok(suffix) = path.strip_prefix(second) {
                    *path = if suffix.as_os_str().is_empty() {
                        first.to_path_buf()
                    } else {
                        first.join(suffix)
                    };
                }
            }
        }
    }

    pub fn rename(&self, old: &Path, new: &Path, no_replace: bool) -> io::Result<()> {
        self.require_visible(old)?;
        self.require_visible(new)?;
        Self::validate_rel(old)?;
        Self::validate_rel(new)?;
        if old == new {
            return Ok(());
        }
        let source_meta = self.metadata(old)?;
        if source_meta.is_dir() && new.starts_with(old) {
            return Err(error(libc::EINVAL));
        }
        let replaced_upper = self.validate_replacement(old, new, no_replace)?;
        let source = if source_meta.is_dir() {
            self.materialize_tree(old)?
        } else {
            self.copy_up(old)?
        };
        self.ensure_upper_parents(new)?;
        self.clear_whiteout(new)?;
        let source_needs_whiteout = self.exists_in_lower(old);
        if source_needs_whiteout {
            self.create_whiteout(old)?;
        }
        let backup = replaced_upper
            .as_ref()
            .map(|_| self.temporary_path(self.upper()));
        if let (Some(destination), Some(backup)) = (&replaced_upper, &backup) {
            if let Err(error) = fs::rename(destination, backup) {
                if source_needs_whiteout {
                    let _ = self.clear_whiteout(old);
                }
                return Err(error);
            }
        }
        if let Err(error) = fs::rename(&source, self.upper_path(new)) {
            if let (Some(destination), Some(backup)) = (&replaced_upper, &backup) {
                let _ = fs::rename(backup, destination);
            }
            if source_needs_whiteout {
                let _ = self.clear_whiteout(old);
            }
            return Err(error);
        }
        if let Some(backup) = backup {
            if let Err(error) = Self::remove_physical(&backup) {
                log::warn!(
                    "rename committed but cleanup of {} failed: {error}",
                    backup.display()
                );
            }
        }
        self.remap_copied_hard_links(&self.upper_path(old), &self.upper_path(new));
        Ok(())
    }

    pub fn hard_link(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.require_visible(source)?;
        self.require_visible(destination)?;
        let metadata = self.metadata(source)?;
        if metadata.is_dir() {
            return Err(error(libc::EPERM));
        }
        if self.resolve(destination).is_some() {
            return Err(error(libc::EEXIST));
        }
        let source = self.copy_up(source)?;
        self.ensure_upper_parents(destination)?;
        self.clear_whiteout(destination)?;
        fs::hard_link(source, self.upper_path(destination))
    }

    pub fn exchange(&self, first: &Path, second: &Path) -> io::Result<()> {
        self.require_visible(first)?;
        self.require_visible(second)?;
        Self::validate_rel(first)?;
        Self::validate_rel(second)?;
        if first == second {
            return Ok(());
        }
        if first.starts_with(second) || second.starts_with(first) {
            return Err(error(libc::EINVAL));
        }
        let first_meta = self.metadata(first)?;
        let second_meta = self.metadata(second)?;
        let first_upper = if first_meta.is_dir() {
            self.materialize_tree(first)?
        } else {
            self.copy_up(first)?
        };
        let second_upper = if second_meta.is_dir() {
            self.materialize_tree(second)?
        } else {
            self.copy_up(second)?
        };
        let temporary = self.temporary_path(self.upper());
        fs::rename(&first_upper, &temporary)?;
        if let Err(error) = fs::rename(&second_upper, &first_upper) {
            let _ = fs::rename(&temporary, &first_upper);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &second_upper) {
            let _ = fs::rename(&first_upper, &second_upper);
            let _ = fs::rename(&temporary, &first_upper);
            return Err(error);
        }
        self.exchange_copied_hard_links(&first_upper, &second_upper);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use tempfile::TempDir;

    #[test]
    fn unmapped_ownership_is_ignorable_but_other_invalid_metadata_is_not() {
        let invalid = io::Error::from_raw_os_error(libc::EINVAL);
        assert!(ignorable_ownership_error(&invalid));
        assert!(!ignorable_metadata_error(&invalid));
    }

    struct Fixture {
        _temp: TempDir,
        lower1: PathBuf,
        lower2: PathBuf,
        upper: PathBuf,
        core: OverlayCore,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let lower1 = temp.path().join("lower1");
            let lower2 = temp.path().join("lower2");
            let upper = temp.path().join("upper");
            let work = temp.path().join("work");
            for directory in [&lower1, &lower2, &upper, &work] {
                fs::create_dir(directory).expect("create layer");
            }
            let core = OverlayCore::new(
                vec![lower1.clone(), lower2.clone()],
                upper.clone(),
                Some(work),
            )
            .expect("core");
            Self {
                _temp: temp,
                lower1,
                lower2,
                upper,
                core,
            }
        }
    }

    #[test]
    fn top_lower_wins_and_directories_merge() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.lower1.join("dir")).expect("dir");
        fs::create_dir(fixture.lower2.join("dir")).expect("dir");
        fs::write(fixture.lower1.join("dir/a"), b"a").expect("a");
        fs::write(fixture.lower1.join("dir/shared"), b"top").expect("top");
        fs::write(fixture.lower2.join("dir/b"), b"b").expect("b");
        fs::write(fixture.lower2.join("dir/shared"), b"bottom").expect("bottom");

        let names = fixture.core.list_names(Path::new("dir")).expect("names");
        assert_eq!(
            names,
            vec![
                OsString::from("a"),
                OsString::from("b"),
                OsString::from("shared")
            ]
        );
        let resolved = fixture
            .core
            .resolve(Path::new("dir/shared"))
            .expect("resolved");
        assert_eq!(fs::read(resolved.path).expect("read"), b"top");
    }

    #[test]
    fn recreating_a_whiteouted_directory_is_opaque() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.lower2.join("old")).expect("old");
        fixture
            .core
            .remove(Path::new("old"), true)
            .expect("whiteout");
        fixture
            .core
            .create_dir(Path::new("old"), 0o755)
            .expect("mkdir");
        assert!(fixture.upper.join("old").join(OPAQUE_NAME).is_file());
        assert!(fixture
            .core
            .list_names(Path::new("old"))
            .expect("names")
            .is_empty());
    }

    #[test]
    fn renaming_lower_directory_keeps_complete_merged_tree() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.lower1.join("tree/nested")).expect("tree");
        fs::create_dir_all(fixture.lower2.join("tree/nested")).expect("tree");
        fs::write(fixture.lower1.join("tree/a"), b"a").expect("a");
        fs::write(fixture.lower2.join("tree/b"), b"b").expect("b");
        fs::write(fixture.lower1.join("tree/nested/c"), b"c").expect("c");

        fixture
            .core
            .rename(Path::new("tree"), Path::new("moved"), false)
            .expect("rename");

        assert!(fixture.core.resolve(Path::new("tree")).is_none());
        for path in ["moved/a", "moved/b", "moved/nested/c"] {
            assert!(fixture.core.resolve(Path::new(path)).is_some(), "{path}");
        }
        assert!(fixture.upper.join("moved").join(OPAQUE_NAME).is_file());
    }

    #[test]
    fn copy_up_preserves_contents_mode_and_xattrs_when_supported() {
        let fixture = Fixture::new();
        let source = fixture.lower2.join("file");
        fs::write(&source, b"payload").expect("write");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).expect("chmod");
        let xattr_supported =
            sys::set_xattr(&source, OsStr::new("user.persisting.test"), b"value", 0).is_ok();

        let copied = fixture.core.copy_up(Path::new("file")).expect("copy up");
        let mut contents = Vec::new();
        File::open(&copied)
            .expect("open")
            .read_to_end(&mut contents)
            .expect("read");
        assert_eq!(contents, b"payload");
        assert_eq!(
            fs::symlink_metadata(&copied).expect("meta").mode() & 0o777,
            0o751
        );
        if xattr_supported {
            assert_eq!(
                sys::get_xattr(&copied, OsStr::new("user.persisting.test")).expect("xattr"),
                b"value"
            );
        }
    }

    #[test]
    fn hard_link_copies_up_once_and_shares_data() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("source"), b"before").expect("source");
        fixture
            .core
            .hard_link(Path::new("source"), Path::new("linked"))
            .expect("link");
        let mut linked = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(fixture.upper.join("linked"))
            .expect("open");
        linked.write_all(b"after").expect("write");
        assert_eq!(
            fs::read(fixture.upper.join("source")).expect("read"),
            b"after"
        );
    }

    #[test]
    fn lower_hard_links_remain_linked_after_independent_copy_up() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("one"), b"before").expect("one");
        fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("two")).expect("link");
        fixture.core.copy_up(Path::new("one")).expect("copy one");
        fixture.core.copy_up(Path::new("two")).expect("copy two");
        fs::write(fixture.upper.join("one"), b"after").expect("write");
        assert_eq!(fs::read(fixture.upper.join("two")).expect("read"), b"after");
        assert_eq!(
            fs::metadata(fixture.upper.join("one")).expect("one").ino(),
            fs::metadata(fixture.upper.join("two")).expect("two").ino()
        );
    }

    #[test]
    fn lower_hard_link_index_survives_rename() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("one"), b"before").expect("one");
        fs::hard_link(fixture.lower2.join("one"), fixture.lower2.join("two")).expect("link");
        fixture.core.copy_up(Path::new("one")).expect("copy one");
        fixture
            .core
            .rename(Path::new("one"), Path::new("moved"), false)
            .expect("rename");
        fixture.core.copy_up(Path::new("two")).expect("copy two");
        fs::write(fixture.upper.join("moved"), b"after").expect("write");
        assert_eq!(fs::read(fixture.upper.join("two")).expect("read"), b"after");
    }

    #[test]
    fn exchange_materializes_and_swaps_lower_entries() {
        let fixture = Fixture::new();
        fs::write(fixture.lower2.join("a"), b"a").expect("a");
        fs::create_dir(fixture.lower2.join("b")).expect("b");
        fs::write(fixture.lower2.join("b/child"), b"b").expect("child");

        fixture
            .core
            .exchange(Path::new("a"), Path::new("b"))
            .expect("exchange");

        assert_eq!(
            fs::read(fixture.core.resolve(Path::new("b")).expect("b").path).expect("read"),
            b"a"
        );
        assert_eq!(
            fs::read(
                fixture
                    .core
                    .resolve(Path::new("a/child"))
                    .expect("child")
                    .path
            )
            .expect("read"),
            b"b"
        );
    }

    #[test]
    fn excluded_subtree_is_absent_and_cannot_be_recreated() {
        let temporary = tempfile::tempdir().unwrap();
        let lower = temporary.path().join("lower");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        fs::create_dir_all(lower.join("visible")).unwrap();
        fs::create_dir_all(lower.join("internal/nested")).unwrap();
        fs::write(lower.join("internal/nested/control"), b"secret").unwrap();
        let core = OverlayCore::new_with_exclusions(
            vec![lower],
            upper,
            Some(work),
            vec![PathBuf::from("internal")],
        )
        .unwrap();

        assert!(core.resolve(Path::new("internal")).is_none());
        assert!(core.resolve(Path::new("internal/nested/control")).is_none());
        assert!(!core
            .list_names(Path::new(""))
            .unwrap()
            .contains(&OsString::from("internal")));
        assert_eq!(
            core.create_dir(Path::new("internal"), 0o755)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        assert_eq!(
            core.rename(
                Path::new("visible"),
                Path::new("internal/replacement"),
                false
            )
            .unwrap_err()
            .raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[test]
    fn rename_replaces_empty_upper_directory_transactionally() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.lower2.join("source/child")).expect("source");
        fs::write(fixture.lower2.join("source/child/file"), b"data").expect("file");
        fixture
            .core
            .create_dir(Path::new("destination"), 0o755)
            .expect("destination");

        fixture
            .core
            .rename(Path::new("source"), Path::new("destination"), false)
            .expect("rename");

        assert!(fixture.core.resolve(Path::new("source")).is_none());
        assert_eq!(
            fs::read(
                fixture
                    .core
                    .resolve(Path::new("destination/child/file"))
                    .expect("file")
                    .path
            )
            .expect("read"),
            b"data"
        );
    }
}
