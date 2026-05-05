//! 极窄 C ABI：请求只经 **`persisting_engine_submit`**；**不**在同调用返回业务体。
//! 通过 **`persisting_engine_job_poll`** 查询 `PersistingJobStatus`（含进度）；经 **`persisting_engine_job_take_result`**
//! 按缓冲取走 UTF-8 RON `RpcResponse`；**`persisting_engine_job_release`** 丢弃任务。契约与常量在 **`persisting_proto::invoke_abi`**。

use std::ffi::c_char;

use persisting_proto::PersistingJobStatus;

use crate::jobs::SubmitError;

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

#[no_mangle]
pub extern "C" fn persisting_engine_version() -> *const c_char {
    VERSION.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn persisting_engine_protocol_version() -> u32 {
    persisting_proto::PROTOCOL_VERSION
}

#[no_mangle]
pub extern "C" fn persisting_engine_ron_abi_version() -> u32 {
    persisting_proto::RON_ABI_VERSION
}

/// 提交 UTF-8 RON `RpcRequest`；成功时写入 **非零** `*handle_out`，任务在后台执行。
#[no_mangle]
pub unsafe extern "C" fn persisting_engine_submit(
    request: *const u8,
    request_len: u64,
    handle_out: *mut u64,
) -> i32 {
    use persisting_proto::{
        PERSISTING_ENGINE_SUBMIT_ERR_NULL, PERSISTING_ENGINE_SUBMIT_ERR_THREAD,
        PERSISTING_ENGINE_SUBMIT_ERR_UTF8, PERSISTING_ENGINE_SUBMIT_OK,
    };

    if handle_out.is_null() {
        return PERSISTING_ENGINE_SUBMIT_ERR_NULL;
    }
    let req = if request_len == 0 {
        &[][..]
    } else if request.is_null() {
        return PERSISTING_ENGINE_SUBMIT_ERR_NULL;
    } else {
        std::slice::from_raw_parts(request, request_len as usize)
    };

    match crate::jobs::submit_job(req) {
        Ok(id) => {
            *handle_out = id;
            PERSISTING_ENGINE_SUBMIT_OK
        }
        Err(SubmitError::NotUtf8) => PERSISTING_ENGINE_SUBMIT_ERR_UTF8,
        Err(SubmitError::Spawn) => PERSISTING_ENGINE_SUBMIT_ERR_THREAD,
    }
}

/// 查询任务状态与 `progress_percent`（0–100，粗粒度）。
#[no_mangle]
pub unsafe extern "C" fn persisting_engine_job_poll(
    job: u64,
    status_out: *mut PersistingJobStatus,
) -> i32 {
    use persisting_proto::{
        PERSISTING_ENGINE_POLL_ERR_NULL, PERSISTING_ENGINE_POLL_ERR_NO_JOB,
        PERSISTING_ENGINE_POLL_OK,
    };

    if status_out.is_null() {
        return PERSISTING_ENGINE_POLL_ERR_NULL;
    }
    let mut st = PersistingJobStatus::default();
    match crate::jobs::poll_job(job, &mut st) {
        Ok(()) => {
            *status_out = st;
            PERSISTING_ENGINE_POLL_OK
        }
        Err(()) => PERSISTING_ENGINE_POLL_ERR_NO_JOB,
    }
}

/// 取走 UTF-8 RON `RpcResponse`：probe / fill 语义见 **`persisting_proto::invoke_abi`**；fill 成功后 job 从表中移除。
#[no_mangle]
pub unsafe extern "C" fn persisting_engine_job_take_result(
    job: u64,
    response_out: *mut u8,
    response_capacity: u64,
    response_len_out: *mut u64,
) -> i32 {
    crate::jobs::take_job_result(job, response_out, response_capacity, response_len_out)
}

/// 取消或清理任务（未 `take_result` 也可调用）；无此 handle 时仍返回 OK。
#[no_mangle]
pub unsafe extern "C" fn persisting_engine_job_release(job: u64) -> i32 {
    crate::jobs::release_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_proto::{
        RequestBody, RpcRequest, RpcResponse, SearchAddRequest, PROTOCOL_VERSION,
    };

    fn sample_request_ron() -> String {
        let req = RpcRequest {
            version: PROTOCOL_VERSION,
            body: RequestBody::SearchAdd(SearchAddRequest {
                dataset: "ds".into(),
                id: None,
                text: "hi".into(),
                metadata: None,
                embedding_dim: 4,
            }),
        };
        ron::ser::to_string_pretty(
            &req,
            ron::ser::PrettyConfig::new().indentor("    ".to_string()),
        )
        .unwrap()
    }

    #[test]
    fn submit_poll_take_smoke() {
        let ron_in = sample_request_ron();
        let mut handle: u64 = 0;
        let st = unsafe {
            persisting_engine_submit(
                ron_in.as_ptr(),
                ron_in.len() as u64,
                &mut handle,
            )
        };
        assert_eq!(st, persisting_proto::PERSISTING_ENGINE_SUBMIT_OK);
        assert!(handle > 0);

        loop {
            let mut status = PersistingJobStatus::default();
            let p = unsafe { persisting_engine_job_poll(handle, &mut status) };
            assert_eq!(p, persisting_proto::PERSISTING_ENGINE_POLL_OK);
            if status.state == persisting_proto::JOB_STATE_COMPLETE {
                break;
            }
            assert!(status.progress_percent >= 0 && status.progress_percent <= 100);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let mut need: u64 = 0;
        let t1 = unsafe {
            persisting_engine_job_take_result(handle, std::ptr::null_mut(), 0, &mut need)
        };
        assert_eq!(t1, persisting_proto::PERSISTING_ENGINE_TAKE_OK);
        let mut buf = vec![0u8; need as usize];
        let mut w = need;
        let t2 = unsafe {
            persisting_engine_job_take_result(handle, buf.as_mut_ptr(), need, &mut w)
        };
        assert_eq!(t2, persisting_proto::PERSISTING_ENGINE_TAKE_OK);
        let text = std::str::from_utf8(&buf).unwrap();
        let _: RpcResponse = ron::de::from_str(text).unwrap();
        let r = unsafe { persisting_engine_job_release(handle) };
        assert_eq!(r, persisting_proto::PERSISTING_ENGINE_RELEASE_OK);
    }

    #[test]
    fn take_out_too_small_then_retry() {
        let ron_in = sample_request_ron();
        let mut handle: u64 = 0;
        unsafe {
            persisting_engine_submit(
                ron_in.as_ptr(),
                ron_in.len() as u64,
                &mut handle,
            )
        };
        loop {
            let mut status = PersistingJobStatus::default();
            unsafe { persisting_engine_job_poll(handle, &mut status) };
            if status.state == persisting_proto::JOB_STATE_COMPLETE {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut need: u64 = 0;
        unsafe { persisting_engine_job_take_result(handle, std::ptr::null_mut(), 0, &mut need) };
        let mut tiny = [0u8; 2];
        let mut out_len = need;
        let st = unsafe {
            persisting_engine_job_take_result(
                handle,
                tiny.as_mut_ptr(),
                tiny.len() as u64,
                &mut out_len,
            )
        };
        assert_eq!(st, persisting_proto::PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL);
        assert_eq!(out_len, need);
        let mut buf = vec![0u8; need as usize];
        let st2 = unsafe {
            persisting_engine_job_take_result(handle, buf.as_mut_ptr(), need, &mut out_len)
        };
        assert_eq!(st2, persisting_proto::PERSISTING_ENGINE_TAKE_OK);
        let _ = unsafe { persisting_engine_job_release(handle) };
    }
}
