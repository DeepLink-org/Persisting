//! Shared API types, **RON C ABI** version ([`RON_ABI_VERSION`](messages::RON_ABI_VERSION)), and optional **bincode RPC** framing ([`PROTOCOL_VERSION`](messages::PROTOCOL_VERSION)).
//!
//! C 宿主经 **`persisting_engine_submit`** 提交 UTF-8 RON `RpcRequest`，用 **`job_poll`** 查进度，用 **`job_take_result`** 取 `RpcResponse` RON；约定见 [`invoke_abi`]。
//! Bincode 编解码见 [`encode_rpc_request`] 等；[`rpc_dispatch`] 模块实现与业务无关的信封转发（[`handle_rpc_request_with`](rpc_dispatch::handle_rpc_request_with)、[`dispatch_bincode_with`](rpc_dispatch::dispatch_bincode_with)），供 `persisting-engine` 等接入。

mod messages;
pub mod rpc_dispatch;
pub mod invoke_abi;

pub use messages::*;
pub use rpc_dispatch::{
    application_error_response, dispatch_bincode_with, handle_rpc_request_with,
    malformed_request_response, version_mismatch_response,
};
pub use invoke_abi::{
    invoke_ron_utf8_via_jobs_sync, job_take_result_utf8_with_buffer, poll_status_label,
    response_utf8_to_string, submit_status_label, take_status_label, PersistingEngineJobPollFn,
    PersistingEngineJobPollStatus, PersistingEngineJobReleaseFn, PersistingEngineJobReleaseStatus,
    PersistingEngineJobSyms, PersistingEngineJobTakeResultFn, PersistingEngineJobTakeStatus,
    PersistingEngineSubmitFn, PersistingEngineSubmitStatus, PersistingJobStatus,
    PERSISTING_ENGINE_POLL_ERR_NO_JOB, PERSISTING_ENGINE_POLL_ERR_NULL, PERSISTING_ENGINE_POLL_OK,
    PERSISTING_ENGINE_RELEASE_ERR_POISONED, PERSISTING_ENGINE_RELEASE_OK,
    PERSISTING_ENGINE_SUBMIT_ERR_NULL, PERSISTING_ENGINE_SUBMIT_ERR_THREAD,
    PERSISTING_ENGINE_SUBMIT_ERR_UTF8, PERSISTING_ENGINE_SUBMIT_OK, PERSISTING_ENGINE_TAKE_ERR_NO_JOB,
    PERSISTING_ENGINE_TAKE_ERR_NOT_READY, PERSISTING_ENGINE_TAKE_ERR_NULL,
    PERSISTING_ENGINE_TAKE_ERR_OUT_TOO_SMALL, PERSISTING_ENGINE_TAKE_OK, JOB_STATE_COMPLETE,
    JOB_STATE_PENDING, JOB_STATE_RUNNING,
};

use anyhow::{Context, Result};

fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard()
}

pub fn encode_rpc_request(req: &RpcRequest) -> Result<Vec<u8>> {
    bincode::serde::encode_to_vec(req, bincode_config()).context("encode RpcRequest")
}

pub fn decode_rpc_request(bytes: &[u8]) -> Result<RpcRequest> {
    let (req, _len): (RpcRequest, usize) =
        bincode::serde::decode_from_slice(bytes, bincode_config()).context("decode RpcRequest")?;
    Ok(req)
}

pub fn encode_rpc_response(resp: &RpcResponse) -> Result<Vec<u8>> {
    bincode::serde::encode_to_vec(resp, bincode_config()).context("encode RpcResponse")
}

pub fn decode_rpc_response(bytes: &[u8]) -> Result<RpcResponse> {
    let (resp, _len): (RpcResponse, usize) =
        bincode::serde::decode_from_slice(bytes, bincode_config()).context("decode RpcResponse")?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_request_roundtrips() {
        let req = RpcRequest {
            version: PROTOCOL_VERSION,
            body: RequestBody::SearchAdd(SearchAddRequest {
                dataset: "ds".into(),
                id: Some("id1".into()),
                text: "hello".into(),
                metadata: None,
                embedding_dim: 16,
            }),
        };
        let bytes = encode_rpc_request(&req).unwrap();
        let got = decode_rpc_request(&bytes).unwrap();
        assert_eq!(got.version, req.version);
        match (&got.body, &req.body) {
            (RequestBody::SearchAdd(a), RequestBody::SearchAdd(b)) => {
                assert_eq!(a.dataset, b.dataset);
                assert_eq!(a.text, b.text);
            }
            _ => panic!("variant mismatch"),
        }
    }
}
