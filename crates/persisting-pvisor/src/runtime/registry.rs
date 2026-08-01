//! Durable Run identity and liveness metadata colocated with an Overlay workspace.

use super::overlay::{
    load_overlay_record, mount_overlay_record_read_only, overlay_status, OverlayRecord,
    OverlayUpper, ReadOnlyOverlayMount,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub const RUN_META_FILENAME: &str = "run.json";
pub const LEASE_FILENAME: &str = "lease.lock";
pub const CONTROL_FILENAME: &str = "control.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub session_id: String,
    pub agent: String,
    pub pid: u32,
    pub command: Vec<String>,
    pub state: String,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: Option<u64>,
    pub storage: PathBuf,
    #[serde(default)]
    pub overlaynet_listen: Option<String>,
    pub gateway_listen: Option<String>,
    pub network: serde_json::Value,
    pub overlay: Option<OverlayRecord>,
    #[serde(default)]
    pub overlay_lowers: Vec<PathBuf>,
}

impl RunRecord {
    pub fn stage_dir(&self) -> PathBuf {
        self.overlay
            .as_ref()
            .map(|record| record.stage_dir.clone())
            .unwrap_or_else(|| self.storage.clone())
    }

    pub fn write(&self) -> anyhow::Result<()> {
        let stage = self.stage_dir();
        fs::create_dir_all(&stage)?;
        let path = stage.join(RUN_META_FILENAME);
        let temporary = stage.join(format!(".{RUN_META_FILENAME}.tmp"));
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)?;

        let index_dir = self.storage.join(".pvisor").join("runs");
        fs::create_dir_all(&index_dir)?;
        fs::write(
            index_dir.join(format!("{}.json", encode_id(&self.run_id))),
            serde_json::to_vec_pretty(&RunIndex {
                run_id: self.run_id.clone(),
                stage_dir: stage,
            })?,
        )?;
        Ok(())
    }

    pub fn read(stage: &Path) -> anyhow::Result<Self> {
        let path = stage.join(RUN_META_FILENAME);
        Ok(serde_json::from_slice(&fs::read(&path)?)?)
    }

    pub fn remove_index(&self) -> anyhow::Result<()> {
        let path = self
            .storage
            .join(".pvisor")
            .join("runs")
            .join(format!("{}.json", encode_id(&self.run_id)));
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunIndex {
    run_id: String,
    stage_dir: PathBuf,
}

/// Exclusive process-lifetime lease. Its file is intentionally retained.
pub struct RunLease {
    file: File,
}

impl RunLease {
    pub fn acquire(stage_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(stage_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(stage_dir.join(LEASE_FILENAME))?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            anyhow::bail!("Run workspace is already leased: {}", stage_dir.display());
        }
        Ok(Self { file })
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ControlRequest {
    Ping,
    OverlayStatus,
    MountInspect,
    UnmountInspect { id: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlResponse {
    ok: bool,
    id: Option<String>,
    mountpoint: Option<PathBuf>,
    error: Option<String>,
    overlay_status: Option<ControlOverlayStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlOverlayStatus {
    pub changed_files: usize,
    pub whiteouts: usize,
    pub sample_paths: Vec<String>,
}

/// Attempt-scoped local control endpoint. The owning pVisor creates read-only
/// views so a second CLI process never interferes with a live writable mount.
pub struct RunControlServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    locator_path: PathBuf,
}

impl RunControlServer {
    pub fn start(record: &RunRecord) -> anyhow::Result<Option<Self>> {
        let Some(overlay) = record.overlay.clone() else {
            return Ok(None);
        };
        let lowers = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let locator_path = record.stage_dir().join(CONTROL_FILENAME);
        if locator_path.exists() || locator_path.is_symlink() {
            fs::remove_file(&locator_path)?;
        }
        // macOS sockaddr_un paths are short. Bind in the system temporary
        // directory and expose a stable stage-local symlink for discovery.
        let socket_path =
            std::env::temp_dir().join(format!("pvisor-{}.sock", uuid::Uuid::new_v4().simple()));
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        std::os::unix::fs::symlink(&socket_path, &locator_path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let stage = record.stage_dir();
        let join = std::thread::Builder::new()
            .name(format!("pvisor-control-{}", record.run_id))
            .spawn(move || {
                let mut mounts: HashMap<String, ReadOnlyOverlayMount> = HashMap::new();
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            serve_control(stream, &stage, &overlay, &lowers, &mut mounts);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Some(Self {
            stop,
            join: Some(join),
            socket_path,
            locator_path,
        }))
    }
}

impl Drop for RunControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.locator_path);
    }
}

fn serve_control(
    mut stream: std::os::unix::net::UnixStream,
    stage: &Path,
    overlay: &OverlayRecord,
    lowers: &[PathBuf],
    mounts: &mut HashMap<String, ReadOnlyOverlayMount>,
) {
    use std::io::{BufRead, Write};
    let request = (|| -> anyhow::Result<ControlRequest> {
        let mut line = String::new();
        std::io::BufReader::new(&stream).read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    })();
    let response = match request {
        Ok(ControlRequest::Ping) => ControlResponse {
            ok: true,
            id: None,
            mountpoint: None,
            error: None,
            overlay_status: None,
        },
        Ok(ControlRequest::OverlayStatus) => match overlay_status(overlay) {
            Ok(status) => ControlResponse {
                ok: true,
                id: None,
                mountpoint: None,
                error: None,
                overlay_status: Some(ControlOverlayStatus {
                    changed_files: status.changed_files,
                    whiteouts: status.whiteouts,
                    sample_paths: status.sample_paths,
                }),
            },
            Err(error) => control_error(error),
        },
        Ok(ControlRequest::MountInspect) => {
            let id = uuid::Uuid::new_v4().to_string();
            let mountpoint = stage.join("inspect").join(&id).join("merged");
            match mount_overlay_record_read_only(overlay, lowers, &mountpoint) {
                Ok(mount) => {
                    mounts.insert(id.clone(), mount);
                    ControlResponse {
                        ok: true,
                        id: Some(id),
                        mountpoint: Some(mountpoint),
                        error: None,
                        overlay_status: None,
                    }
                }
                Err(error) => control_error(error),
            }
        }
        Ok(ControlRequest::UnmountInspect { id }) => {
            if let Some(mount) = mounts.remove(&id) {
                match mount.unmount() {
                    Ok(()) => ControlResponse {
                        ok: true,
                        id: None,
                        mountpoint: None,
                        error: None,
                        overlay_status: None,
                    },
                    Err(error) => control_error(error),
                }
            } else {
                control_error(anyhow::anyhow!("unknown inspect session {id}"))
            }
        }
        Err(error) => control_error(error),
    };
    if let Ok(mut body) = serde_json::to_vec(&response) {
        body.push(b'\n');
        let _ = stream.write_all(&body);
    }
}

fn control_error(error: impl std::fmt::Display) -> ControlResponse {
    ControlResponse {
        ok: false,
        id: None,
        mountpoint: None,
        error: Some(error.to_string()),
        overlay_status: None,
    }
}

fn control_request(stage: &Path, request: &ControlRequest) -> anyhow::Result<ControlResponse> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(stage.join(CONTROL_FILENAME))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    let response: ControlResponse = serde_json::from_str(&line)?;
    if !response.ok {
        anyhow::bail!(
            "pVisor control request failed: {}",
            response.error.as_deref().unwrap_or("unknown error")
        );
    }
    Ok(response)
}

pub fn control_ping(stage: &Path) -> bool {
    control_request(stage, &ControlRequest::Ping).is_ok()
}

pub fn control_mount_inspect(stage: &Path) -> anyhow::Result<(String, PathBuf)> {
    let response = control_request(stage, &ControlRequest::MountInspect)?;
    Ok((
        response.id.context("control response missing inspect id")?,
        response
            .mountpoint
            .context("control response missing inspect mountpoint")?,
    ))
}

pub fn control_overlay_status(stage: &Path) -> anyhow::Result<ControlOverlayStatus> {
    control_request(stage, &ControlRequest::OverlayStatus)?
        .overlay_status
        .context("control response missing OverlayFS status")
}

pub fn control_unmount_inspect(stage: &Path, id: String) -> anyhow::Result<()> {
    control_request(stage, &ControlRequest::UnmountInspect { id })?;
    Ok(())
}

pub fn is_live(stage_dir: &Path) -> anyhow::Result<bool> {
    let path = stage_dir.join(LEASE_FILENAME);
    if !path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        Ok(false)
    } else {
        let error = std::io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(true)
        } else {
            Err(error.into())
        }
    }
}

/// Resolve a Run from a run id, stage, upper directory, database, or a path
/// inside the target/merged workspace.
pub fn resolve_run(selector: Option<&Path>, storage: &Path) -> anyhow::Result<RunRecord> {
    if let Some(selector) = selector {
        if selector.exists() || selector.components().count() > 1 {
            return resolve_path(selector);
        }
        let id = selector.to_string_lossy();
        let index = storage
            .join(".pvisor")
            .join("runs")
            .join(format!("{}.json", encode_id(&id)));
        if index.exists() {
            let index: RunIndex = serde_json::from_slice(&fs::read(index)?)?;
            return RunRecord::read(&index.stage_dir);
        }
        anyhow::bail!("pVisor Run not found: {}", selector.display());
    }

    if let Ok(current) = std::env::current_dir() {
        if let Ok(record) = resolve_path(&current) {
            return Ok(record);
        }
    }
    latest_run(storage)
}

fn resolve_path(path: &Path) -> anyhow::Result<RunRecord> {
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if absolute.join(".pvisor").join("runs").is_dir() {
        return latest_run(&absolute);
    }
    let mut candidates = Vec::new();
    if absolute.is_file() {
        if absolute
            .file_name()
            .is_some_and(|name| name == RUN_META_FILENAME)
        {
            candidates.push(absolute.parent().unwrap_or(Path::new(".")).to_path_buf());
        } else if let Some(parent) = absolute.parent() {
            candidates.push(parent.to_path_buf());
        }
    } else {
        candidates.push(absolute.clone());
        if absolute.file_name().is_some_and(|name| name == "upper") {
            if let Some(parent) = absolute.parent() {
                candidates.push(parent.to_path_buf());
            }
        }
    }
    candidates.extend(absolute.ancestors().map(Path::to_path_buf));
    for stage in candidates {
        if stage.join(RUN_META_FILENAME).is_file() {
            return RunRecord::read(&stage);
        }
        if stage.join("overlay.json").is_file() {
            let overlay = load_overlay_record(&stage)?;
            if let Ok(record) = RunRecord::read(&overlay.stage_dir) {
                return Ok(record);
            }
        }
    }

    // A target or merged path is not necessarily below stage_dir. Scan the
    // nearest project storage and compare canonical roots.
    for ancestor in absolute.ancestors() {
        let storage = ancestor.join(".persisting").join("capture");
        if storage.is_dir() {
            for record in all_runs(&storage)? {
                if record.overlay.as_ref().is_some_and(|overlay| {
                    path_within(&absolute, &overlay.target)
                        || path_within(&absolute, &overlay.merged_dir)
                        || match &overlay.upper {
                            OverlayUpper::Directory { upper_dir, .. } => {
                                path_within(&absolute, upper_dir)
                            }
                            OverlayUpper::Jujutsu {
                                store_path,
                                upper_dir,
                                ..
                            } => {
                                path_within(&absolute, upper_dir)
                                    || path_within(&absolute, store_path)
                            }
                        }
                }) {
                    return Ok(record);
                }
            }
        }
    }
    anyhow::bail!("no pVisor Run metadata found for {}", path.display())
}

fn path_within(path: &Path, root: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.starts_with(root)
}

pub fn all_runs(storage: &Path) -> anyhow::Result<Vec<RunRecord>> {
    let dir = storage.join(".pvisor").join("runs");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let index: RunIndex = match serde_json::from_slice(&fs::read(path)?) {
                Ok(index) => index,
                Err(_) => continue,
            };
            if let Ok(record) = RunRecord::read(&index.stage_dir) {
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at_unix_ms));
    Ok(records)
}

fn latest_run(storage: &Path) -> anyhow::Result<RunRecord> {
    all_runs(storage)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no pVisor Runs found under {}", storage.display()))
}

fn encode_id(id: &str) -> String {
    let mut encoded = String::with_capacity(id.len() * 2);
    for byte in id.as_bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(storage: &Path, stage: &Path, upper: &Path) -> RunRecord {
        RunRecord {
            schema_version: 1,
            run_id: "run-test".into(),
            session_id: "session-test".into(),
            agent: "test".into(),
            pid: 1,
            command: vec!["true".into()],
            state: "completed".into(),
            started_at_unix_ms: 1,
            finished_at_unix_ms: Some(2),
            storage: storage.to_path_buf(),
            overlaynet_listen: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            overlay: Some(OverlayRecord {
                id: "session-test".into(),
                target: storage.join("target"),
                upper: OverlayUpper::Directory {
                    upper_dir: upper.to_path_buf(),
                    work_dir: stage.join("work"),
                },
                merged_dir: stage.join("merged"),
                stage_dir: stage.to_path_buf(),
                auto_apply: false,
                auto_discard: false,
                state: super::super::overlay::OverlayState::Staged,
            }),
            overlay_lowers: vec![storage.join("target")],
        }
    }

    #[test]
    fn lease_reports_live_only_while_held() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_live(temp.path()).unwrap());
        let lease = RunLease::acquire(temp.path()).unwrap();
        assert!(is_live(temp.path()).unwrap());
        drop(lease);
        assert!(!is_live(temp.path()).unwrap());
    }

    #[test]
    fn run_resolves_from_id_stage_and_upper() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("store");
        let stage = storage.join(".overlay/session-test");
        let upper = stage.join("upper");
        fs::create_dir_all(&upper).unwrap();
        let record = record(&storage, &stage, &upper);
        record.write().unwrap();

        assert_eq!(
            resolve_run(Some(Path::new("run-test")), &storage)
                .unwrap()
                .run_id,
            "run-test"
        );
        assert_eq!(resolve_path(&stage).unwrap().run_id, "run-test");
        assert_eq!(resolve_path(&upper).unwrap().run_id, "run-test");
        assert_eq!(resolve_path(&storage).unwrap().run_id, "run-test");
    }
}
