//! Jujutsu-managed directory uppers.
//!
//! Each overlay fork is a Jujutsu workspace. All workspaces point at the same
//! repository, so their working-copy commits, operation log, and Git objects
//! live in one store while the writable POSIX directories remain independent.

use jj_lib::config::StackedConfig;
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::lock::FileLock;
use jj_lib::matchers::EverythingMatcher;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::{WorkspaceName, WorkspaceNameBuf};
use jj_lib::repo::{Repo as _, StoreFactories};
use jj_lib::settings::UserSettings;
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::workspace::{Workspace, default_working_copy_factories, default_working_copy_factory};
use pollster::FutureExt as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CONTROL_DIR: &str = "control";
const WORKSPACES_DIR: &str = "workspaces";
const UPPER_DIR: &str = "upper";

fn io_other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn settings() -> io::Result<UserSettings> {
    UserSettings::from_config(StackedConfig::with_defaults()).map_err(io_other)
}

fn validate_fork(fork: &str) -> io::Result<()> {
    if fork.is_empty()
        || fork == "."
        || fork == ".."
        || fork.contains('/')
        || fork.contains('\\')
        || fork.as_bytes().contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid Jujutsu overlay workspace name: {fork:?}"),
        ));
    }
    Ok(())
}

fn load_workspace(settings: &UserSettings, root: &Path) -> io::Result<Workspace> {
    Workspace::load(
        settings,
        root,
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .map_err(io_other)
}

/// One writable OverlayFS fork backed by a workspace in a shared Jujutsu repo.
pub(crate) struct JujutsuWorkspace {
    store_path: PathBuf,
    workspace_root: PathBuf,
    upper_dir: PathBuf,
    fork: WorkspaceNameBuf,
    // Held for the lifetime of a writable mount. Jujutsu's normal working-copy
    // lock only covers an individual snapshot, while FUSE writes happen between
    // snapshots and must not have two writers for the same workspace.
    _mount_lock: Option<FileLock>,
}

impl std::fmt::Debug for JujutsuWorkspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JujutsuWorkspace")
            .field("store_path", &self.store_path)
            .field("workspace_root", &self.workspace_root)
            .field("upper_dir", &self.upper_dir)
            .field("fork", &self.fork.as_str())
            .finish_non_exhaustive()
    }
}

impl JujutsuWorkspace {
    pub(crate) fn open(store_path: PathBuf, fork: String, read_only: bool) -> io::Result<Self> {
        validate_fork(&fork)?;
        fs::create_dir_all(&store_path)?;
        let store_path = fs::canonicalize(store_path)?;
        let control_root = store_path.join(CONTROL_DIR);
        let workspace_root = store_path.join(WORKSPACES_DIR).join(&fork);
        let upper_dir = workspace_root.join(UPPER_DIR);
        let settings = settings()?;

        // Serialize repository/workspace creation across processes. The lock is
        // dropped before the mount begins; Jujutsu handles later op-log writes.
        let _init_lock = FileLock::lock(store_path.join("init.lock")).map_err(io_other)?;
        if !control_root.join(".jj").is_dir() {
            fs::create_dir_all(&control_root)?;
            Workspace::init_internal_git(&settings, &control_root)
                .block_on()
                .map_err(io_other)?;
        }
        if !workspace_root.join(".jj").is_dir() {
            fs::create_dir_all(&workspace_root)?;
            let control = load_workspace(&settings, &control_root)?;
            let repo = control
                .repo_loader()
                .load_at_head()
                .block_on()
                .map_err(io_other)?;
            Workspace::init_workspace_with_existing_repo(
                &workspace_root,
                control.repo_path(),
                &repo,
                &*default_working_copy_factory(),
                WorkspaceNameBuf::from(fork.clone()),
            )
            .block_on()
            .map_err(io_other)?;
        }
        fs::create_dir_all(&upper_dir)?;
        let _mount_lock = if read_only {
            None
        } else {
            Some(
                FileLock::try_lock(workspace_root.join(".jj").join("persisting-overlay.lock"))
                    .map_err(io_other)?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "Jujutsu overlay workspace {fork:?} is already mounted writable"
                            ),
                        )
                    })?,
            )
        };

        Ok(Self {
            store_path,
            workspace_root,
            upper_dir,
            fork: WorkspaceNameBuf::from(fork),
            _mount_lock,
        })
    }

    pub(crate) fn upper_dir(&self) -> &Path {
        &self.upper_dir
    }

    /// Snapshot the upper directory into this workspace's working-copy commit.
    pub(crate) fn snapshot(&self) -> io::Result<Option<String>> {
        snapshot_workspace(&self.workspace_root, &self.fork)
    }
}

fn snapshot_workspace(
    workspace_root: &Path,
    expected_name: &WorkspaceName,
) -> io::Result<Option<String>> {
    let settings = settings()?;
    let mut workspace = load_workspace(&settings, workspace_root)?;
    if workspace.workspace_name() != expected_name {
        return Err(io::Error::other(format!(
            "Jujutsu workspace name mismatch: expected {:?}, found {:?}",
            expected_name.as_str(),
            workspace.workspace_name().as_str()
        )));
    }
    let repo = workspace
        .repo_loader()
        .load_at_head()
        .block_on()
        .map_err(io_other)?;
    let wc_commit_id = repo
        .view()
        .get_wc_commit_id(expected_name)
        .ok_or_else(|| io::Error::other("Jujutsu workspace has no working-copy commit"))?
        .clone();
    let old_commit = repo.store().get_commit(&wc_commit_id).map_err(io_other)?;

    let everything = EverythingMatcher;
    let options = SnapshotOptions {
        base_ignores: GitIgnoreFile::empty(),
        progress: None,
        start_tracking_matcher: &everything,
        // Overlay snapshots are exact filesystem state, not source-control
        // intent. A file hidden by an upper-layer .gitignore must still be
        // recoverable from this workspace head.
        force_tracking_matcher: &everything,
        max_new_file_size: u64::MAX,
    };
    let mut locked = workspace
        .start_working_copy_mutation()
        .block_on()
        .map_err(io_other)?;
    let (new_tree, _stats) = locked
        .locked_wc()
        .snapshot(&options)
        .block_on()
        .map_err(io_other)?;

    if new_tree.tree_ids() == old_commit.tree().tree_ids() {
        locked
            .finish(repo.operation().id().clone())
            .block_on()
            .map_err(io_other)?;
        return Ok(None);
    }

    let mut transaction = repo.start_transaction();
    transaction.set_workspace_name(expected_name);
    transaction.set_is_snapshot(true);
    let new_commit = transaction
        .repo_mut()
        .rewrite_commit(&old_commit)
        .set_tree(new_tree)
        .write()
        .block_on()
        .map_err(io_other)?;
    transaction
        .repo_mut()
        .rebase_descendants()
        .block_on()
        .map_err(io_other)?;
    let new_repo = transaction
        .commit(format!(
            "snapshot persisting OverlayFS workspace {}",
            expected_name.as_str()
        ))
        .block_on()
        .map_err(io_other)?;
    locked
        .finish(new_repo.operation().id().clone())
        .block_on()
        .map_err(io_other)?;
    Ok(Some(new_commit.id().hex()))
}

/// Snapshot a named fork after an out-of-band apply or discard operation.
pub fn snapshot_jujutsu_upper(store_path: &Path, fork: &str) -> io::Result<Option<String>> {
    let workspace = JujutsuWorkspace::open(store_path.to_owned(), fork.to_owned(), false)?;
    workspace.snapshot()
}

/// Deterministic directory used as the live upper for a named fork.
pub fn jujutsu_upper_dir(store_path: &Path, fork: &str) -> io::Result<PathBuf> {
    validate_fork(fork)?;
    Ok(store_path.join(WORKSPACES_DIR).join(fork).join(UPPER_DIR))
}

/// Initialize a Jujutsu-backed upper for a mountless virtio-fs consumer.
pub fn prepare_jujutsu_upper(store_path: &Path, fork: &str) -> io::Result<PathBuf> {
    let workspace = JujutsuWorkspace::open(store_path.to_owned(), fork.to_owned(), false)?;
    Ok(workspace.upper_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_forks_share_one_repository_and_keep_independent_heads() {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("overlay.jj");
        let first = JujutsuWorkspace::open(store.clone(), "first".into(), false).unwrap();
        fs::write(first.upper_dir().join("value"), b"first").unwrap();
        let first_commit = first.snapshot().unwrap().unwrap();
        drop(first);

        let second = JujutsuWorkspace::open(store.clone(), "second".into(), false).unwrap();
        fs::write(second.upper_dir().join("value"), b"second").unwrap();
        let second_commit = second.snapshot().unwrap().unwrap();
        assert_ne!(first_commit, second_commit);
        assert_eq!(
            fs::read(store.join("workspaces/first/upper/value")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(store.join("workspaces/second/upper/value")).unwrap(),
            b"second"
        );

        let settings = settings().unwrap();
        let control = load_workspace(&settings, &store.join(CONTROL_DIR)).unwrap();
        let repo = control.repo_loader().load_at_head().block_on().unwrap();
        let workspaces = repo.view().wc_commit_ids();
        assert!(workspaces.contains_key(&WorkspaceNameBuf::from("first")));
        assert!(workspaces.contains_key(&WorkspaceNameBuf::from("second")));
        assert_eq!(control.repo_path(), store.join("control/.jj/repo"));
    }

    #[test]
    fn fork_name_cannot_escape_store() {
        let temp = tempfile::tempdir().unwrap();
        assert!(JujutsuWorkspace::open(temp.path().into(), "../escape".into(), false).is_err());
        assert!(jujutsu_upper_dir(temp.path(), "nested/name").is_err());
    }
}
