use anyhow::{Context, Result};
use axum::http::{HeaderMap, HeaderName, HeaderValue};

pub(crate) fn headers_to_vec(map: &HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
        })
        .collect()
}

pub(crate) fn headers_to_header_map(pairs: &[(String, String)]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid header name: {name}"))?;
        let value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid header value for {name}: {value}"))?;
        map.append(name, value);
    }
    Ok(map)
}
