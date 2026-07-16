use crate::config::LanceStorageConfig;
use lance::Dataset;
use lance::Error as LanceError;
use lance::dataset::builder::DatasetBuilder;
use lance::io::{ObjectStoreParams, StorageOptionsAccessor};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn is_s3_db_uri(db_uri: &str) -> bool {
    db_uri.starts_with("s3://")
}

pub fn lance_dataset_uri(db_uri: &str, table_name: &str) -> String {
    let base = db_uri.trim_end_matches('/');
    format!("{base}/{table_name}.lance")
}

pub fn local_dataset_path(db_uri: &str, table_name: &str) -> PathBuf {
    PathBuf::from(lance_dataset_uri(db_uri, table_name))
}

pub fn build_object_store_params(cfg: &LanceStorageConfig) -> Option<ObjectStoreParams> {
    if !is_s3_db_uri(&cfg.db_uri) {
        return None;
    }

    let opts = cfg.storage_options();
    if opts.is_empty() {
        return Some(ObjectStoreParams::default());
    }

    Some(ObjectStoreParams {
        storage_options_accessor: Some(Arc::new(StorageOptionsAccessor::with_static_options(opts))),
        ..Default::default()
    })
}

pub fn write_params_with_store(
    store_params: Option<ObjectStoreParams>,
) -> lance::dataset::WriteParams {
    lance::dataset::WriteParams {
        store_params,
        ..Default::default()
    }
}

pub async fn open_dataset(
    uri: &str,
    store_params: &Option<ObjectStoreParams>,
) -> lance::Result<Option<Dataset>> {
    if !is_s3_db_uri(uri) {
        let path = Path::new(uri);
        if !path.exists() {
            return Ok(None);
        }
        return Dataset::open(uri).await.map(Some);
    }

    let mut builder = DatasetBuilder::from_uri(uri);
    if let Some(params) = store_params
        && let Some(opts) = params.storage_options()
    {
        builder = builder.with_storage_options(opts.clone());
    }

    match builder.load().await {
        Ok(dataset) => Ok(Some(dataset)),
        Err(err) if is_dataset_missing(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn is_dataset_missing(err: &LanceError) -> bool {
    matches!(
        err,
        LanceError::DatasetNotFound { .. } | LanceError::NotFound { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LanceS3Config;

    #[test]
    fn lance_dataset_uri_joins_table_name() {
        assert_eq!(
            lance_dataset_uri("s3://bucket/prefix", "session_steps"),
            "s3://bucket/prefix/session_steps.lance"
        );
        assert_eq!(
            lance_dataset_uri("../../var/lance/local", "session_steps"),
            "../../var/lance/local/session_steps.lance"
        );
    }

    #[test]
    fn storage_options_include_region_and_endpoint() {
        let cfg = LanceStorageConfig {
            db_uri: "s3://bucket/prefix".to_string(),
            s3: Some(LanceS3Config {
                region: "cn-north-1".to_string(),
                endpoint: Some("https://minio.local".to_string()),
                allow_http: Some(true),
            }),
            ..LanceStorageConfig::default()
        };
        let opts = cfg.storage_options();
        assert_eq!(
            opts.get("aws_region").map(String::as_str),
            Some("cn-north-1")
        );
        assert_eq!(
            opts.get("aws_endpoint").map(String::as_str),
            Some("https://minio.local")
        );
        assert_eq!(opts.get("allow_http").map(String::as_str), Some("true"));
        assert!(build_object_store_params(&cfg).is_some());
    }

    #[test]
    fn storage_options_set_allow_http_for_http_endpoint() {
        let cfg = LanceStorageConfig {
            db_uri: "s3://bucket/prefix".to_string(),
            s3: Some(LanceS3Config {
                region: "cn-north-1".to_string(),
                endpoint: Some("http://ssd1.h.pjlab.org.cn:8060".to_string()),
                allow_http: Some(true),
            }),
            ..LanceStorageConfig::default()
        };
        let opts = cfg.storage_options();
        assert_eq!(opts.get("allow_http").map(String::as_str), Some("true"));
    }
}
