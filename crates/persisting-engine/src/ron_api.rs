//! RON 信封 [`persisting_proto::RpcRequest`] / [`persisting_proto::RpcResponse`]，
//! 由 C ABI 异步 `submit`/`job_take_result` 或进程内直接调用；业务分发在 `dispatch` 内完成。

use anyhow::{Context, Result};
use persisting_proto::RpcRequest;
use serde::Serialize;

use crate::dispatch::handle_rpc_request;

#[derive(Serialize)]
struct AbiTransportError<'a> {
    error: &'a str,
}

/// RON 外壳 `(error: "...")`，与 CLI `WireError` 对齐。
pub fn transport_error_ron(message: &str) -> String {
    ron::to_string(&AbiTransportError { error: message }).unwrap_or_else(|_| "(error: \"ron\")".into())
}

fn ron_pretty() -> ron::ser::PrettyConfig {
    ron::ser::PrettyConfig::new()
        .depth_limit(64)
        .indentor("    ".to_string())
}

/// 解析完整 `RpcRequest` RON → 引擎处理 → 序列化 `RpcResponse` RON（pretty）。
pub fn invoke_ron(request_ron: &str) -> Result<String> {
    let req: RpcRequest =
        ron::de::from_str(request_ron).context("failed to deserialize RpcRequest RON")?;
    let resp = handle_rpc_request(req);
    Ok(ron::ser::to_string_pretty(&resp, ron_pretty())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_proto::{
        RequestBody, ResponseBody, RpcResponse, SearchAddRequest, PROTOCOL_VERSION,
    };

    #[test]
    fn invoke_ron_search_add_roundtrip_envelope() {
        let req = RpcRequest {
            version: PROTOCOL_VERSION,
            body: RequestBody::SearchAdd(SearchAddRequest {
                dataset: "ds".into(),
                id: Some("i1".into()),
                text: "hello".into(),
                metadata: None,
                embedding_dim: 8,
            }),
        };
        let ron_in = ron::ser::to_string_pretty(&req, ron_pretty()).unwrap();
        let ron_out = invoke_ron(&ron_in).unwrap();
        let resp: RpcResponse = ron::de::from_str(&ron_out).unwrap();
        assert_eq!(resp.version, PROTOCOL_VERSION);
        match resp.body {
            ResponseBody::SearchAdd(r) => {
                assert_eq!(r.dataset, "ds");
                assert_eq!(r.id, "i1");
            }
            _ => panic!("expected SearchAdd response"),
        }
    }
}
