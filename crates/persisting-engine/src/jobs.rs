//! 异步 RON 任务：`submit` 只入队请求；`poll` 查询 `PersistingJobStatus`；`take_result` 取走 UTF-8 RON 响应。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use persisting_proto::{
    PersistingJobStatus, JOB_STATE_COMPLETE, JOB_STATE_PENDING, JOB_STATE_RUNNING,
};

pub(crate) enum SubmitError {
    NotUtf8,
    Spawn,
}

struct Job {
    state: AtomicI32,
    progress: AtomicI32,
    /// UTF-8 RON `RpcResponse` 或 transport error RON；仅在 `COMPLETE` 前写入，且先于 `state` 提交。
    response_utf8: Mutex<Option<Vec<u8>>>,
}

impl Job {
    fn new_pending() -> Self {
        Self {
            state: AtomicI32::new(JOB_STATE_PENDING),
            progress: AtomicI32::new(0),
            response_utf8: Mutex::new(None),
        }
    }

    fn snapshot_status(&self) -> PersistingJobStatus {
        PersistingJobStatus {
            state: self.state.load(Ordering::Acquire),
            progress_percent: self.progress.load(Ordering::Acquire),
            reserved: 0,
        }
    }

    fn set_running(&self, progress: i32) {
        self.state.store(JOB_STATE_RUNNING, Ordering::Release);
        self.progress.store(progress, Ordering::Release);
    }

    fn finish_with_response(&self, bytes: Vec<u8>) {
        *self
            .response_utf8
            .lock()
            .expect("job mutex poisoned") = Some(bytes);
        self.progress.store(100, Ordering::Release);
        self.state.store(JOB_STATE_COMPLETE, Ordering::Release);
    }
}

fn table() -> &'static Mutex<HashMap<u64, Arc<Job>>> {
    static T: OnceLock<Mutex<HashMap<u64, Arc<Job>>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_JOB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn spawn_worker(id: u64, job: Arc<Job>, request_ron: String) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(format!("persisting-job-{id}"))
        .spawn(move || {
            job.set_running(10);
            job.progress.store(40, Ordering::Release);
            let bytes = match crate::ron_api::invoke_ron(&request_ron) {
                Ok(s) => s.into_bytes(),
                Err(e) => crate::ron_api::transport_error_ron(&e.to_string()).into_bytes(),
            };
            job.progress.store(85, Ordering::Release);
            job.finish_with_response(bytes);
        })
        .map(|_| ())
}

pub(crate) fn submit_job(request_utf8: &[u8]) -> Result<u64, SubmitError> {
    let request_ron =
        std::str::from_utf8(request_utf8).map_err(|_| SubmitError::NotUtf8)?.to_owned();

    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    let job = Arc::new(Job::new_pending());
    {
        let mut g = table().lock().expect("jobs table mutex poisoned");
        g.insert(id, job.clone());
    }
    spawn_worker(id, job, request_ron).map_err(|_| SubmitError::Spawn)?;
    Ok(id)
}

pub(crate) fn poll_job(handle: u64, out: &mut PersistingJobStatus) -> Result<(), ()> {
    let g = table().lock().expect("jobs table mutex poisoned");
    let Some(j) = g.get(&handle) else {
        return Err(());
    };
    *out = j.snapshot_status();
    Ok(())
}

/// `take_result`：probe/fill 与旧同步 `invoke` 相同；**fill 成功**后从表中删除 job。
pub(crate) unsafe fn take_job_result(
    handle: u64,
    response_out: *mut u8,
    response_capacity: u64,
    response_len_out: *mut u64,
) -> i32 {
    use persisting_proto::{
        PERSISTING_ENGINE_TAKE_ERR_NO_JOB, PERSISTING_ENGINE_TAKE_ERR_NOT_READY,
        PERSISTING_ENGINE_TAKE_ERR_NULL, PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL,
        PERSISTING_ENGINE_TAKE_OK,
    };

    if response_len_out.is_null() {
        return PERSISTING_ENGINE_TAKE_ERR_NULL;
    }

    let job = {
        let g = match table().lock() {
            Ok(g) => g,
            Err(_) => {
                *response_len_out = 0;
                return PERSISTING_ENGINE_TAKE_ERR_NO_JOB;
            }
        };
        match g.get(&handle).cloned() {
            Some(j) => j,
            None => {
                *response_len_out = 0;
                return PERSISTING_ENGINE_TAKE_ERR_NO_JOB;
            }
        }
    };

    if job.state.load(Ordering::Acquire) != JOB_STATE_COMPLETE {
        *response_len_out = 0;
        return PERSISTING_ENGINE_TAKE_ERR_NOT_READY;
    }

    let payload: Vec<u8> = {
        let pl = job.response_utf8.lock().expect("job mutex poisoned");
        match pl.as_ref() {
            Some(b) => b.clone(),
            None => {
                *response_len_out = 0;
                return PERSISTING_ENGINE_TAKE_ERR_NOT_READY;
            }
        }
    };
    let need = payload.len() as u64;
    *response_len_out = need;

    if response_out.is_null() {
        return if response_capacity == 0 {
            PERSISTING_ENGINE_TAKE_OK
        } else {
            PERSISTING_ENGINE_TAKE_ERR_NULL
        };
    }

    if response_capacity < need {
        return PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(payload.as_ptr(), response_out, payload.len());
    }
    let mut g = table().lock().expect("jobs table mutex poisoned");
    g.remove(&handle);
    PERSISTING_ENGINE_TAKE_OK
}

pub(crate) fn release_job(handle: u64) -> i32 {
    use persisting_proto::{PERSISTING_ENGINE_RELEASE_ERR_POISONED, PERSISTING_ENGINE_RELEASE_OK};
    let mut g = match table().lock() {
        Ok(g) => g,
        Err(_) => return PERSISTING_ENGINE_RELEASE_ERR_POISONED,
    };
    g.remove(&handle);
    PERSISTING_ENGINE_RELEASE_OK
}
