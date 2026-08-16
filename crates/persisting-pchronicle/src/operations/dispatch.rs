//! In-process dispatch for pChronicle request values.

#[cfg(feature = "search")]
use std::future::Future;
#[cfg(feature = "search")]
use std::sync::OnceLock;

use crate::{RequestBody, ResponseBody};
use anyhow::Result;

#[cfg(feature = "search")]
fn block_on<F, T>(future: F) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("pchronicle-blocking")
            .build()
            .expect("build shared pChronicle runtime")
    });
    runtime.block_on(future)
}

fn dispatch_inner(body: RequestBody) -> Result<ResponseBody> {
    match body {
        #[cfg(feature = "search")]
        RequestBody::SearchAdd(r) => Ok(ResponseBody::SearchAdd(block_on(
            crate::search::agent::add_document(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchAddBatch(r) => Ok(ResponseBody::SearchAddBatch(block_on(
            crate::search::agent::add_documents_batch(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchQuery(r) => Ok(ResponseBody::SearchQuery(block_on(
            crate::search::agent::query(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchIndex(r) => Ok(ResponseBody::SearchIndex(block_on(
            crate::search::agent::create_index(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchIndexList(r) => Ok(ResponseBody::SearchIndexList(block_on(
            crate::search::agent::list_indices(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchIndexDelete(r) => Ok(ResponseBody::SearchIndexDelete(block_on(
            crate::search::agent::delete_index(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchIndexRebuild(r) => Ok(ResponseBody::SearchIndexRebuild(block_on(
            crate::search::agent::rebuild_indices(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchIndexReorder(r) => Ok(ResponseBody::SearchIndexReorder(block_on(
            crate::search::agent::reorder_ivf_layout(r),
        )?)),
        #[cfg(feature = "search")]
        RequestBody::SearchImportLance(r) => Ok(ResponseBody::SearchImportLance(block_on(
            crate::search::agent::import_from_lance(r),
        )?)),
        #[cfg(not(feature = "search"))]
        RequestBody::SearchAdd(_)
        | RequestBody::SearchAddBatch(_)
        | RequestBody::SearchQuery(_)
        | RequestBody::SearchIndex(_)
        | RequestBody::SearchIndexList(_)
        | RequestBody::SearchIndexDelete(_)
        | RequestBody::SearchIndexRebuild(_)
        | RequestBody::SearchIndexReorder(_)
        | RequestBody::SearchImportLance(_) => {
            anyhow::bail!("pChronicle Search requires the `search` feature")
        }
    }
}

/// Execute one pChronicle operation in process.
pub fn invoke_request_body(body: RequestBody) -> Result<ResponseBody> {
    dispatch_inner(body)
}
