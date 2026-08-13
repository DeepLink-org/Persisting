use std::path::Path;

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use include_dir::{include_dir, Dir};

static EMBEDDED_ASSETS: Dir<'_> = include_dir!("$OUT_DIR/pchronicle-web-assets");

fn assets_root() -> Option<String> {
    std::env::var("PERSISTING_CHRONICLE_ASSETS_ROOT")
        .ok()
        .filter(|root| Path::new(root).join("index.html").is_file())
}

fn normalize(path: &str) -> String {
    let mut value = path.trim_start_matches('/').to_string();
    while value.starts_with("./") {
        value = value[2..].to_string();
    }
    value
}

/// Normalize an asset request path and reject path traversal before any join
/// against the assets root. The env-override mode joins the key onto an
/// arbitrary local directory, so a `..` segment would otherwise read files
/// outside it (e.g. `GET /../../etc/passwd`).
fn safe_key(path: &str) -> Option<String> {
    let key = normalize(path);
    if key.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(key)
}

fn read(path: &str) -> Option<Bytes> {
    let key = safe_key(path)?;
    if let Some(root) = assets_root() {
        return std::fs::read(Path::new(&root).join(key))
            .ok()
            .map(Bytes::from);
    }
    EMBEDDED_ASSETS
        .get_file(key)
        .map(|file| Bytes::copy_from_slice(file.contents()))
}

fn contains(path: &str) -> bool {
    let Some(key) = safe_key(path) else {
        return false;
    };
    if let Some(root) = assets_root() {
        return Path::new(&root).join(key).is_file();
    }
    EMBEDDED_ASSETS.get_file(key).is_some()
}

fn accepts_brotli(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.split(';').next().unwrap_or(part).trim() == "br")
        })
}

fn content_type(path: &str) -> &'static str {
    let logical = path.strip_suffix(".br").unwrap_or(path);
    if logical.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if logical.ends_with(".js") {
        "application/javascript"
    } else if logical.ends_with(".css") {
        "text/css"
    } else if logical.ends_with(".wasm") {
        "application/wasm"
    } else if logical.ends_with(".svg") {
        "image/svg+xml"
    } else if logical.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

fn is_hashed(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains("-dxh"))
}

fn is_static_path(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| {
        matches!(
            name.rsplit_once('.').map(|(_, ext)| ext),
            Some("css" | "js" | "wasm" | "svg" | "png" | "jpg" | "ico" | "json" | "br")
        )
    })
}

fn response_for(path: &str, headers: &HeaderMap) -> Option<Response> {
    let key = safe_key(path)?;
    let (body, encoding) = if accepts_brotli(headers) {
        let compressed = format!("{key}.br");
        match read(&compressed) {
            Some(bytes) if !bytes.is_empty() => (bytes, Some("br")),
            _ => (read(&key)?, None),
        }
    } else {
        (read(&key)?, None)
    };
    let cache = if is_hashed(&key) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(&key))
        .header(header::CACHE_CONTROL, cache);
    if let Some(encoding) = encoding {
        builder = builder.header(header::CONTENT_ENCODING, encoding);
    }
    Some(builder.body(Body::from(body)).unwrap_or_default())
}

pub async fn index(headers: HeaderMap) -> Response {
    response_for("index.html", &headers).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub async fn fallback(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if contains(path) {
        return response_for(path, &headers)
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    }
    if is_static_path(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    index(headers).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_index_is_embedded() {
        assert!(contains("index.html"));
        assert!(read("index.html").is_some_and(|body| !body.is_empty()));
    }

    #[test]
    fn traversal_segments_are_rejected_before_any_read() {
        // `..` segments must never reach the assets root join; otherwise the
        // env-override mode serves arbitrary local files (e.g. /etc/passwd).
        assert!(!contains("/../../etc/passwd"));
        assert!(!contains("../index.html"));
        assert!(!contains("assets/../index.html"));
        assert!(read("/../../etc/passwd").is_none());
        assert!(read("/../secret").is_none());
        // Leading slashes and `./` prefixes are still normalized normally.
        assert!(contains("/index.html"));
        assert!(read("/./index.html").is_some());
        assert!(read("./index.html").is_some());
    }

    #[test]
    fn fallback_serves_index_for_traversal_paths() {
        let headers = HeaderMap::new();
        let response =
            futures::executor::block_on(fallback(Uri::from_static("/../../etc/passwd"), headers));
        let status = response.status();
        let bytes = futures::executor::block_on(async {
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .map(|body| body.to_vec())
                .unwrap_or_default()
        });
        // Either 404 or the SPA index — never raw file contents.
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::OK);
        assert!(!bytes.starts_with(b"root:"));
        assert!(!bytes
            .windows(b"nobody".len())
            .any(|window| window == b"nobody"));
    }

    #[test]
    fn mime_types_cover_dioxus_assets() {
        assert_eq!(content_type("app.js"), "application/javascript");
        assert_eq!(content_type("app_bg.wasm.br"), "application/wasm");
        assert_eq!(content_type("app.css"), "text/css");
    }
}
