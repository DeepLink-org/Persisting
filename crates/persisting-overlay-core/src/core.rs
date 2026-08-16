use crate::sys;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

pub const WHITEOUT_PREFIX: &str = ".wh.";
pub const OPAQUE_NAME: &str = ".wh..wh..opq";
const TEMP_PREFIX: &str = ".wh..persisting-copyup-";
const PREIMAGE_COMPLETE_MARKER: &str = "complete-v1";
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
    preimage_dir: Option<PathBuf>,
    preimage_lock: Mutex<()>,
}

/// Durable first-touch state of one apply target path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathPreimage {
    /// Raw Unix path bytes relative to the overlay root.
    pub path: Vec<u8>,
    pub state: PathFingerprint,
}

impl PathPreimage {
    pub fn relative_path(&self) -> PathBuf {
        PathBuf::from(OsString::from_vec(self.path.clone()))
    }
}

/// Content and metadata relevant to detecting a destructive apply conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathFingerprint {
    Absent,
    File {
        sha256: String,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Directory {
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_seconds: i64,
        mtime_nanoseconds: i64,
    },
    Symlink {
        target: Vec<u8>,
        uid: u32,
        gid: u32,
    },
    Other {
        mode: u32,
        uid: u32,
        gid: u32,
        rdev: u64,
    },
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

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

/// Fingerprint one path without following its final symlink.
pub fn fingerprint_at(root: &Path, rel: &Path) -> io::Result<PathFingerprint> {
    OverlayCore::validate_rel(rel)?;
    let path = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PathFingerprint::Absent)
        }
        Err(error) => return Err(error),
    };
    let kind = metadata.file_type();
    if kind.is_file() {
        return Ok(PathFingerprint::File {
            sha256: sha256_hex(&fs::read(&path)?),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        });
    }
    if kind.is_dir() {
        return Ok(PathFingerprint::Directory {
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mtime_seconds: metadata.mtime(),
            mtime_nanoseconds: metadata.mtime_nsec(),
        });
    }
    if kind.is_symlink() {
        return Ok(PathFingerprint::Symlink {
            target: fs::read_link(&path)?.into_os_string().into_vec(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        });
    }
    Ok(PathFingerprint::Other {
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev(),
    })
}

/// Load all durable first-touch entries from a preimage journal.
pub fn load_preimages(directory: &Path) -> io::Result<Vec<PathPreimage>> {
    let entries = directory.join("entries");
    let iterator = match fs::read_dir(&entries) {
        Ok(iterator) => iterator,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut preimages = Vec::new();
    for entry in iterator {
        let entry = entry?;
        if entry.path().extension() != Some(OsStr::new("json")) {
            continue;
        }
        let preimage = serde_json::from_slice::<PathPreimage>(&fs::read(entry.path())?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        OverlayCore::validate_rel(&preimage.relative_path())?;
        preimages.push(preimage);
    }
    preimages.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(preimages)
}

pub fn preimage_journal_is_complete(directory: &Path) -> bool {
    directory.join(PREIMAGE_COMPLETE_MARKER).is_file()
}

/// Consume journal entries after their corresponding target paths commit.
pub fn remove_preimages(directory: &Path, paths: &[PathBuf]) -> io::Result<()> {
    let entries = directory.join("entries");
    for path in paths {
        OverlayCore::validate_rel(path)?;
        let journal_path =
            entries.join(format!("{}.json", sha256_hex(path.as_os_str().as_bytes())));
        match fs::remove_file(journal_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match File::open(entries) {
        Ok(directory) => directory.sync_all(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
        Self::new_with_exclusions_and_preimages(lowers, upper, work, excluded, None)
    }

    pub fn new_with_exclusions_and_preimages(
        lowers: Vec<PathBuf>,
        upper: PathBuf,
        work: Option<PathBuf>,
        excluded: Vec<PathBuf>,
        preimage_dir: Option<PathBuf>,
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
        let upper_was_empty = fs::read_dir(&upper)?.next().is_none();
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
        if let Some(directory) = &preimage_dir {
            fs::create_dir_all(directory.join("entries"))?;
            if upper_was_empty && !preimage_journal_is_complete(directory) {
                let marker = directory.join(PREIMAGE_COMPLETE_MARKER);
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(marker)?;
                file.write_all(b"pvisor-overlay-preimage-journal-v1\n")?;
                file.sync_all()?;
                File::open(directory)?.sync_all()?;
            }
        }
        let core = Self {
            lowers,
            upper,
            work,
            excluded,
            copied_hard_links: Mutex::new(HashMap::new()),
            preimage_dir,
            preimage_lock: Mutex::new(()),
        };
        if fs::read_dir(&core.upper)?.next().is_none() {
            if let Some(root) = core.lowers.first() {
                let metadata = fs::symlink_metadata(root)?;
                core.copy_metadata(root, &core.upper, &metadata)?;
            }
        }
        Ok(core)
    }

    fn record_preimage(&self, rel: &Path) -> io::Result<()> {
        let Some(directory) = &self.preimage_dir else {
            return Ok(());
        };
        Self::validate_rel(rel)?;
        let _guard = self
            .preimage_lock
            .lock()
            .map_err(|_| io::Error::other("preimage journal lock poisoned"))?;
        let path_bytes = rel.as_os_str().as_bytes();
        let destination = directory
            .join("entries")
            .join(format!("{}.json", sha256_hex(path_bytes)));
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let target = self.lowers.last().ok_or_else(|| error(libc::EINVAL))?;
        let preimage = PathPreimage {
            path: path_bytes.to_vec(),
            state: fingerprint_at(target, rel)?,
        };
        let body = serde_json::to_vec_pretty(&preimage)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&destination)?;
        file.write_all(&body)?;
        file.sync_all()?;
        File::open(directory.join("entries"))?.sync_all()
    }

    fn record_logical_tree_mapping(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.record_preimage(destination)?;
        if !self.metadata(source)?.is_dir() {
            return Ok(());
        }
        for name in self.list_names(source)? {
            let source_child = Self::child(source, &name)?;
            let destination_child = Self::child(destination, &name)?;
            self.record_logical_tree_mapping(&source_child, &destination_child)?;
        }
        Ok(())
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
            // Creating a child changes this copied-up directory's metadata,
            // and apply may promote that metadata even though the Agent did
            // not issue an explicit setattr on the parent.
            self.record_preimage(&current)?;
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
        self.record_preimage(rel)?;
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
        self.record_preimage(rel)?;
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
        self.record_preimage(rel)?;
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
        self.record_preimage(rel)?;
        self.ensure_upper_parents(rel)?;
        self.clear_whiteout(rel)?;
        std::os::unix::fs::symlink(target, self.upper_path(rel))
    }

    pub fn create_node(&self, rel: &Path, mode: u32, rdev: u32) -> io::Result<()> {
        self.require_visible(rel)?;
        if self.resolve(rel).is_some() {
            return Err(error(libc::EEXIST));
        }
        self.record_preimage(rel)?;
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
        self.record_logical_tree_mapping(rel, rel)?;

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
        self.record_logical_tree_mapping(old, old)?;
        self.record_logical_tree_mapping(old, new)?;
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
        self.record_preimage(destination)?;
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
        self.record_logical_tree_mapping(first, first)?;
        self.record_logical_tree_mapping(second, second)?;
        self.record_logical_tree_mapping(first, second)?;
        self.record_logical_tree_mapping(second, first)?;
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

    #[test]
    fn first_touch_preimage_is_durable_and_never_rebased() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        let journal = temporary.path().join("preimages");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("value.txt"), b"original").unwrap();
        let original = fingerprint_at(&target, Path::new("value.txt")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            upper.clone(),
            Some(work),
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();

        core.copy_up(Path::new("value.txt")).unwrap();
        fs::write(upper.join("value.txt"), b"staged").unwrap();
        fs::write(target.join("value.txt"), b"concurrent").unwrap();
        core.copy_up(Path::new("value.txt")).unwrap();

        let entries = load_preimages(&journal).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path(), Path::new("value.txt"));
        assert_eq!(entries[0].state, original);

        remove_preimages(&journal, &[PathBuf::from("value.txt")]).unwrap();
        fs::remove_file(upper.join("value.txt")).unwrap();
        let rebased = fingerprint_at(&target, Path::new("value.txt")).unwrap();
        core.copy_up(Path::new("value.txt")).unwrap();
        let entries = load_preimages(&journal).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, rebased);
    }

    #[test]
    fn preimage_journal_covers_create_remove_and_rename_destinations() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let upper = temporary.path().join("upper");
        let work = temporary.path().join("work");
        let journal = temporary.path().join("preimages");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("source"), b"source").unwrap();
        fs::write(target.join("victim"), b"victim").unwrap();
        let source = fingerprint_at(&target, Path::new("source")).unwrap();
        let victim = fingerprint_at(&target, Path::new("victim")).unwrap();
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![target],
            upper,
            Some(work),
            Vec::new(),
            Some(journal.clone()),
        )
        .unwrap();

        drop(
            core.create_file(Path::new("created"), 0o600, libc::O_RDWR)
                .unwrap(),
        );
        core.remove(Path::new("victim"), false).unwrap();
        core.rename(Path::new("source"), Path::new("moved"), false)
            .unwrap();

        let entries = load_preimages(&journal)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.relative_path(), entry.state))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(entries[Path::new("created")], PathFingerprint::Absent);
        assert_eq!(entries[Path::new("moved")], PathFingerprint::Absent);
        assert_eq!(entries[Path::new("source")], source);
        assert_eq!(entries[Path::new("victim")], victim);
    }
}
