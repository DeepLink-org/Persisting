//! 异步 C 契约：`persisting_engine_submit` → `handle`，再 `job_poll` / `job_take_result` / `job_release`。
//! 请求 **仅经 submit 传入**；响应 **仅经 `job_take_result` 缓冲** 取回（probe + fill）。返回码与 `PersistingJobStatus` 供宿主与 Rust 封装共用。

use std::time::Duration;

use anyhow::{bail, Context, Result};

// ---------------------------------------------------------------------------
// Job status (C-visible POD)
// ---------------------------------------------------------------------------

/// C 布局与 `persisting_engine_job_poll` 的 `status_out` 一致。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersistingJobStatus {
    /// 见 `JOB_STATE_*`。
    pub state: i32,
    /// 0–100；当前为粗粒度进度，后续可接 Lance 真实回调。
    pub progress_percent: i32,
    pub reserved: u64,
}

pub const JOB_STATE_PENDING: i32 = 0;
pub const JOB_STATE_RUNNING: i32 = 1;
pub const JOB_STATE_COMPLETE: i32 = 2;

// ---------------------------------------------------------------------------
// submit
// ---------------------------------------------------------------------------

pub type PersistingEngineSubmitStatus = i32;

pub const PERSISTING_ENGINE_SUBMIT_OK: PersistingEngineSubmitStatus = 0;
pub const PERSISTING_ENGINE_SUBMIT_ERR_NULL: PersistingEngineSubmitStatus = -1;
pub const PERSISTING_ENGINE_SUBMIT_ERR_UTF8: PersistingEngineSubmitStatus = -2;
pub const PERSISTING_ENGINE_SUBMIT_ERR_THREAD: PersistingEngineSubmitStatus = -3;

pub type PersistingEngineSubmitFn = unsafe extern "C" fn(
    request: *const u8,
    request_len: u64,
    handle_out: *mut u64,
) -> PersistingEngineSubmitStatus;

// ---------------------------------------------------------------------------
// job_poll
// ---------------------------------------------------------------------------

pub type PersistingEngineJobPollStatus = i32;

pub const PERSISTING_ENGINE_POLL_OK: PersistingEngineJobPollStatus = 0;
pub const PERSISTING_ENGINE_POLL_ERR_NULL: PersistingEngineJobPollStatus = -1;
pub const PERSISTING_ENGINE_POLL_ERR_NO_JOB: PersistingEngineJobPollStatus = -2;

pub type PersistingEngineJobPollFn = unsafe extern "C" fn(
    job: u64,
    status_out: *mut PersistingJobStatus,
) -> PersistingEngineJobPollStatus;

// ---------------------------------------------------------------------------
// job_take_result（与旧同步 invoke 相同的 probe/fill）
// ---------------------------------------------------------------------------

pub type PersistingEngineJobTakeStatus = i32;

pub const PERSISTING_ENGINE_TAKE_OK: PersistingEngineJobTakeStatus = 0;
pub const PERSISTING_ENGINE_TAKE_ERR_NULL: PersistingEngineJobTakeStatus = -1;
pub const PERSISTING_ENGINE_TAKE_ERR_NOT_READY: PersistingEngineJobTakeStatus = -2;
pub const PERSISTING_ENGINE_TAKE_ERR_NO_JOB: PersistingEngineJobTakeStatus = -3;
pub const PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL: PersistingEngineJobTakeStatus = -4;

pub type PersistingEngineJobTakeResultFn = unsafe extern "C" fn(
    job: u64,
    response_out: *mut u8,
    response_capacity: u64,
    response_len_out: *mut u64,
) -> PersistingEngineJobTakeStatus;

// ---------------------------------------------------------------------------
// job_release
// ---------------------------------------------------------------------------

pub type PersistingEngineJobReleaseStatus = i32;

pub const PERSISTING_ENGINE_RELEASE_OK: PersistingEngineJobReleaseStatus = 0;
pub const PERSISTING_ENGINE_RELEASE_ERR_POISONED: PersistingEngineJobReleaseStatus = -1;

pub type PersistingEngineJobReleaseFn =
    unsafe extern "C" fn(job: u64) -> PersistingEngineJobReleaseStatus;

// ---------------------------------------------------------------------------
// Labels & helpers
// ---------------------------------------------------------------------------

pub const fn submit_status_label(code: PersistingEngineSubmitStatus) -> &'static str {
    match code {
        PERSISTING_ENGINE_SUBMIT_OK => "OK",
        PERSISTING_ENGINE_SUBMIT_ERR_NULL => "ERR_NULL",
        PERSISTING_ENGINE_SUBMIT_ERR_UTF8 => "ERR_UTF8",
        PERSISTING_ENGINE_SUBMIT_ERR_THREAD => "ERR_THREAD",
        _ => "ERR_UNKNOWN",
    }
}

pub const fn poll_status_label(code: PersistingEngineJobPollStatus) -> &'static str {
    match code {
        PERSISTING_ENGINE_POLL_OK => "OK",
        PERSISTING_ENGINE_POLL_ERR_NULL => "ERR_NULL",
        PERSISTING_ENGINE_POLL_ERR_NO_JOB => "ERR_NO_JOB",
        _ => "ERR_UNKNOWN",
    }
}

pub const fn take_status_label(code: PersistingEngineJobTakeStatus) -> &'static str {
    match code {
        PERSISTING_ENGINE_TAKE_OK => "OK",
        PERSISTING_ENGINE_TAKE_ERR_NULL => "ERR_NULL",
        PERSISTING_ENGINE_TAKE_ERR_NOT_READY => "ERR_NOT_READY",
        PERSISTING_ENGINE_TAKE_ERR_NO_JOB => "ERR_NO_JOB",
        PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL => "ERR_OUT_TOO_SMALL",
        _ => "ERR_UNKNOWN",
    }
}

/// 四个 C 入口的 Rust 视图（CLI `dlopen` 后填充）。
#[derive(Clone, Copy)]
pub struct PersistingEngineJobSyms {
    pub submit: PersistingEngineSubmitFn,
    pub poll: PersistingEngineJobPollFn,
    pub take_result: PersistingEngineJobTakeResultFn,
    pub release: PersistingEngineJobReleaseFn,
}

/// `job_take_result` 的 probe + fill（与旧 `invoke` 缓冲语义一致）。
///
/// # Safety
///
/// `take` must be a valid `persisting_engine_job_take_result` from the same loaded engine ABI,
/// and must remain valid for the duration of both calls. `job` must be a job id returned by
/// `submit` that is still poll-complete and not yet released.
pub unsafe fn job_take_result_utf8_with_buffer(
    take: PersistingEngineJobTakeResultFn,
    job: u64,
) -> Result<Vec<u8>> {
    let mut need: u64 = 0;
    let st = take(job, std::ptr::null_mut(), 0, &mut need);
    if st != PERSISTING_ENGINE_TAKE_OK {
        bail!(
            "persisting_engine_job_take_result (probe): {} ({})",
            take_status_label(st),
            st
        );
    }
    let mut buf = vec![0u8; need as usize];
    let mut written = need;
    let st2 = take(job, buf.as_mut_ptr(), need, &mut written);
    if st2 != PERSISTING_ENGINE_TAKE_OK {
        bail!(
            "persisting_engine_job_take_result (fill): {} ({})",
            take_status_label(st2),
            st2
        );
    }
    buf.truncate(written as usize);
    Ok(buf)
}

/// 同步封装：submit → 轮询至 `COMPLETE` → take_result → `release`（幂等）。
///
/// # Safety
/// `syms` 中函数指针须来自同一 `libpersisting_engine` 导出。
pub unsafe fn invoke_ron_utf8_via_jobs_sync(
    syms: PersistingEngineJobSyms,
    request_utf8: &[u8],
) -> Result<Vec<u8>> {
    let mut handle: u64 = 0;
    let st = (syms.submit)(
        request_utf8.as_ptr(),
        request_utf8.len() as u64,
        &mut handle,
    );
    if st != PERSISTING_ENGINE_SUBMIT_OK {
        bail!(
            "persisting_engine_submit: {} ({})",
            submit_status_label(st),
            st
        );
    }

    loop {
        let mut status = PersistingJobStatus::default();
        let p = (syms.poll)(handle, &mut status);
        if p != PERSISTING_ENGINE_POLL_OK {
            let _ = (syms.release)(handle);
            bail!(
                "persisting_engine_job_poll: {} ({})",
                poll_status_label(p),
                p
            );
        }
        if status.state == JOB_STATE_COMPLETE {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let raw = job_take_result_utf8_with_buffer(syms.take_result, handle)?;
    let _ = (syms.release)(handle);
    Ok(raw)
}

pub fn response_utf8_to_string(response: &[u8]) -> Result<String> {
    std::str::from_utf8(response)
        .map(str::to_owned)
        .context("engine response is not valid UTF-8")
}
