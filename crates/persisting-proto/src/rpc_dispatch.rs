//! Bincode RPC envelope: decode → [`RequestBody`] → encode [`RpcResponse`].
//!
//! 业务层（如 `persisting-engine`）只实现 **`RequestBody` → `Result<ResponseBody>`** 的匹配转发；
//! 协议版本号、**`ResponseBody::Error`** 码与 **malformed** 路径在此统一处理。

use anyhow::Result;

use crate::{
    decode_rpc_request, encode_rpc_response, error_codes, RequestBody, ResponseBody, RpcRequest,
    RpcResponse, PROTOCOL_VERSION,
};

fn response_ok(body: ResponseBody) -> RpcResponse {
    RpcResponse {
        version: PROTOCOL_VERSION,
        body,
    }
}

/// `PROTOCOL_VERSION` 与请求不一致时的标准错误响应。
pub fn version_mismatch_response(request_version: u32) -> RpcResponse {
    RpcResponse {
        version: PROTOCOL_VERSION,
        body: ResponseBody::Error {
            code: error_codes::VERSION_MISMATCH,
            message: format!(
                "protocol version mismatch: expected {}, got {}",
                PROTOCOL_VERSION, request_version
            ),
        },
    }
}

/// 请求体无法解码为 [`RpcRequest`] 时的标准错误响应。
pub fn malformed_request_response(message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        version: PROTOCOL_VERSION,
        body: ResponseBody::Error {
            code: error_codes::MALFORMED_REQUEST,
            message: message.into(),
        },
    }
}

/// 业务错误（`anyhow`）映射为 **`APPLICATION_ERROR`**。
pub fn application_error_response(message: impl Into<String>) -> RpcResponse {
    RpcResponse {
        version: PROTOCOL_VERSION,
        body: ResponseBody::Error {
            code: error_codes::APPLICATION_ERROR,
            message: message.into(),
        },
    }
}

/// 校验 `RpcRequest.version`，再调用 **`dispatch`** 得到业务体；错误统一为 [`ResponseBody::Error`]。
pub fn handle_rpc_request_with<F>(req: RpcRequest, dispatch: F) -> RpcResponse
where
    F: FnOnce(RequestBody) -> Result<ResponseBody>,
{
    if req.version != PROTOCOL_VERSION {
        return version_mismatch_response(req.version);
    }
    match dispatch(req.body) {
        Ok(body) => response_ok(body),
        Err(e) => application_error_response(e.to_string()),
    }
}

/// 解码 bincode **`RpcRequest`** → 转发 → 编码 **`RpcResponse`**（含 malformed 分支）。
pub fn dispatch_bincode_with<F>(input: &[u8], dispatch: F) -> Vec<u8>
where
    F: FnOnce(RequestBody) -> Result<ResponseBody>,
{
    match decode_rpc_request(input) {
        Ok(req) => encode_rpc_response(&handle_rpc_request_with(req, dispatch)).unwrap_or_default(),
        Err(e) => {
            encode_rpc_response(&malformed_request_response(e.to_string())).unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchAddRequest;

    #[test]
    fn version_mismatch_short_circuit() {
        let req = RpcRequest {
            version: PROTOCOL_VERSION.wrapping_add(1),
            body: RequestBody::SearchAdd(SearchAddRequest {
                dataset: "x".into(),
                id: None,
                text: "y".into(),
                metadata: None,
                embedding_dim: 1,
            }),
        };
        let resp = handle_rpc_request_with(req, |_| {
            panic!("dispatch must not run when version mismatches");
        });
        assert_eq!(resp.version, PROTOCOL_VERSION);
        match resp.body {
            ResponseBody::Error { code, .. } => {
                assert_eq!(code, error_codes::VERSION_MISMATCH);
            }
            _ => panic!("expected error body"),
        }
    }

    #[test]
    fn dispatch_ok_roundtrip() {
        let req = RpcRequest {
            version: PROTOCOL_VERSION,
            body: RequestBody::SearchAdd(SearchAddRequest {
                dataset: "ds".into(),
                id: None,
                text: "hello".into(),
                metadata: None,
                embedding_dim: 8,
            }),
        };
        let resp = handle_rpc_request_with(req, |body| {
            let RequestBody::SearchAdd(r) = body else {
                anyhow::bail!("expected SearchAdd");
            };
            Ok(ResponseBody::SearchAdd(crate::SearchAddResponse {
                dataset: r.dataset,
                id: "generated".into(),
                embedding_dim: r.embedding_dim,
                embedding_preview: vec![0.0_f32; 2],
                status: "ok".into(),
                note: "test".into(),
            }))
        });
        assert!(matches!(resp.body, ResponseBody::SearchAdd(_)));
    }
}
