//! Typed in-process API for pChronicle callers: no RON string at the Rust boundary.

#[cfg(feature = "search")]
use crate::{RequestBody, ResponseBody};
#[cfg(feature = "search")]
use crate::{
    SearchAddBatchRequest, SearchAddBatchResponse, SearchAddRequest, SearchAddResponse,
    SearchImportLanceRequest, SearchImportLanceResponse, SearchIndexDeleteRequest,
    SearchIndexDeleteResponse, SearchIndexListRequest, SearchIndexListResponse,
    SearchIndexRebuildRequest, SearchIndexRebuildResponse, SearchIndexReorderRequest,
    SearchIndexReorderResponse, SearchIndexRequest, SearchIndexResponse, SearchQueryRequest,
    SearchQueryResponse,
};
#[cfg(feature = "search")]
use anyhow::{anyhow, Result};

#[cfg(feature = "search")]
use super::dispatch::invoke_request_body;

#[cfg(feature = "search")]
fn map_body<T, F>(body: ResponseBody, f: F) -> Result<T>
where
    F: FnOnce(ResponseBody) -> Option<T>,
{
    f(body).ok_or_else(|| anyhow!("unexpected pChronicle response variant"))
}

#[cfg(feature = "search")]
pub fn search_add(req: SearchAddRequest) -> Result<SearchAddResponse> {
    map_body(invoke_request_body(RequestBody::SearchAdd(req))?, |b| {
        if let ResponseBody::SearchAdd(r) = b {
            Some(r)
        } else {
            None
        }
    })
}

#[cfg(feature = "search")]
pub fn search_add_batch(req: SearchAddBatchRequest) -> Result<SearchAddBatchResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchAddBatch(req))?,
        |b| {
            if let ResponseBody::SearchAddBatch(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}

#[cfg(feature = "search")]
pub fn search_query(req: SearchQueryRequest) -> Result<SearchQueryResponse> {
    map_body(invoke_request_body(RequestBody::SearchQuery(req))?, |b| {
        if let ResponseBody::SearchQuery(r) = b {
            Some(r)
        } else {
            None
        }
    })
}

#[cfg(feature = "search")]
pub fn search_index(req: SearchIndexRequest) -> Result<SearchIndexResponse> {
    map_body(invoke_request_body(RequestBody::SearchIndex(req))?, |b| {
        if let ResponseBody::SearchIndex(r) = b {
            Some(r)
        } else {
            None
        }
    })
}

#[cfg(feature = "search")]
pub fn search_index_list(req: SearchIndexListRequest) -> Result<SearchIndexListResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchIndexList(req))?,
        |b| {
            if let ResponseBody::SearchIndexList(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}

#[cfg(feature = "search")]
pub fn search_index_delete(req: SearchIndexDeleteRequest) -> Result<SearchIndexDeleteResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchIndexDelete(req))?,
        |b| {
            if let ResponseBody::SearchIndexDelete(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}

#[cfg(feature = "search")]
pub fn search_index_rebuild(req: SearchIndexRebuildRequest) -> Result<SearchIndexRebuildResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchIndexRebuild(req))?,
        |b| {
            if let ResponseBody::SearchIndexRebuild(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}

#[cfg(feature = "search")]
pub fn search_index_reorder(req: SearchIndexReorderRequest) -> Result<SearchIndexReorderResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchIndexReorder(req))?,
        |b| {
            if let ResponseBody::SearchIndexReorder(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}

#[cfg(feature = "search")]
pub fn search_import_lance(req: SearchImportLanceRequest) -> Result<SearchImportLanceResponse> {
    map_body(
        invoke_request_body(RequestBody::SearchImportLance(req))?,
        |b| {
            if let ResponseBody::SearchImportLance(r) = b {
                Some(r)
            } else {
                None
            }
        },
    )
}
