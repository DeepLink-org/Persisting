//! Single RPC entry used by bincode FFI, RON bridges, and in-process callers.

use std::future::Future;

use anyhow::Result;
use persisting_proto::{
    dispatch_bincode_with, handle_rpc_request_with, RequestBody, ResponseBody, RpcRequest,
    RpcResponse,
};

fn block_on<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(future)
}

fn dispatch_inner(body: RequestBody) -> Result<ResponseBody> {
    match body {
        RequestBody::SearchAdd(r) => Ok(ResponseBody::SearchAdd(block_on(
            crate::search::agent::add_document(r),
        )?)),
        RequestBody::SearchAddBatch(r) => Ok(ResponseBody::SearchAddBatch(block_on(
            crate::search::agent::add_documents_batch(r),
        )?)),
        RequestBody::SearchQuery(r) => Ok(ResponseBody::SearchQuery(block_on(
            crate::search::agent::query(r),
        )?)),
        RequestBody::SearchIndex(r) => Ok(ResponseBody::SearchIndex(block_on(
            crate::search::agent::create_index(r),
        )?)),
        RequestBody::SearchIndexList(r) => Ok(ResponseBody::SearchIndexList(block_on(
            crate::search::agent::list_indices(r),
        )?)),
        RequestBody::SearchIndexDelete(r) => Ok(ResponseBody::SearchIndexDelete(block_on(
            crate::search::agent::delete_index(r),
        )?)),
        RequestBody::SearchIndexRebuild(r) => Ok(ResponseBody::SearchIndexRebuild(block_on(
            crate::search::agent::rebuild_indices(r),
        )?)),
        RequestBody::SearchIndexReorder(r) => Ok(ResponseBody::SearchIndexReorder(block_on(
            crate::search::agent::reorder_ivf_layout(r),
        )?)),
        RequestBody::SearchImportLance(r) => Ok(ResponseBody::SearchImportLance(block_on(
            crate::search::agent::import_from_lance(r),
        )?)),
        RequestBody::TrajectoryAppend(r) => Ok(ResponseBody::TrajectoryAppend(block_on(
            crate::trajectory::append_async(r),
        )?)),
        RequestBody::TrajectoryReplay(r) => Ok(ResponseBody::TrajectoryReplay(block_on(
            crate::trajectory::replay_async(r),
        )?)),
        RequestBody::TrajectoryStats(r) => Ok(ResponseBody::TrajectoryStats(block_on(
            crate::trajectory::stats_async(r),
        )?)),
    }
}

pub fn handle_rpc_request(req: RpcRequest) -> RpcResponse {
    handle_rpc_request_with(req, dispatch_inner)
}

/// In-process call without `RpcRequest` envelope; errors surface as [`anyhow::Error`], not `ResponseBody::Error`.
pub fn invoke_request_body(body: RequestBody) -> Result<ResponseBody> {
    dispatch_inner(body)
}

pub fn dispatch_bytes(slice: &[u8]) -> Vec<u8> {
    dispatch_bincode_with(slice, dispatch_inner)
}
