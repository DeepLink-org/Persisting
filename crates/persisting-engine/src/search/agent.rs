use anyhow::{Context, Result};
use lance::Dataset;
use lance::datatypes::Schema as LanceSchema;
use lance::deps::arrow_schema::DataType;
use lance::index::DatasetIndexExt;
use lance_index::is_system_index;
use lance_index::optimize::OptimizeOptions;
pub use persisting_proto::{
    SearchAddRequest, SearchAddResponse, SearchImportLanceRequest, SearchImportLanceResponse,
    SearchIndexDeleteRequest, SearchIndexDeleteResponse, SearchIndexListEntry,
    SearchIndexListRequest, SearchIndexListResponse, SearchIndexRebuildRequest,
    SearchIndexRebuildResponse, SearchIndexReorderRequest, SearchIndexReorderResponse,
    SearchIndexRequest, SearchIndexResponse, SearchQueryRequest, SearchQueryResponse,
};

pub fn embed_text(text: &str, dim: usize) -> Result<Vec<f32>> {
    if dim == 0 {
        anyhow::bail!("embedding_dim must be greater than 0");
    }

    let mut vector = vec![0.0_f32; dim];
    for token in text.split_whitespace() {
        let normalized = token
            .trim_matches(|ch: char| !ch.is_alphanumeric())
            .to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        let bucket = stable_hash(normalized.as_bytes()) as usize % dim;
        vector[bucket] += 1.0;
    }

    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }

    Ok(vector)
}

pub async fn add_document(request: SearchAddRequest) -> Result<SearchAddResponse> {
    super::lance::append_document(request).await
}

pub async fn query(request: SearchQueryRequest) -> Result<SearchQueryResponse> {
    validate_dataset_path(&request.dataset).await?;
    super::lance::vector_query(request).await
}

pub async fn list_indices(request: SearchIndexListRequest) -> Result<SearchIndexListResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    let ds = Dataset::open(&request.dataset)
        .await
        .with_context(|| format!("failed to open Lance dataset at {}", request.dataset))?;
    let arc = ds.load_indices().await.context("failed to load index metadata")?;
    let schema = ds.schema();
    let mut indices: Vec<SearchIndexListEntry> = Vec::new();
    for meta in arc.iter() {
        if is_system_index(meta) {
            continue;
        }
        let columns: Vec<String> = meta
            .fields
            .iter()
            .map(|fid| {
                schema
                    .field_path(*fid)
                    .unwrap_or_else(|_| format!("<field_id {}>", fid))
            })
            .collect();
        indices.push(SearchIndexListEntry {
            name: meta.name.clone(),
            uuid: meta.uuid.to_string(),
            dataset_version: meta.dataset_version,
            index_version: meta.index_version,
            columns,
        });
    }
    Ok(SearchIndexListResponse {
        dataset: request.dataset,
        indices,
        status: "ok".to_string(),
        note: "Non-system index segments from Dataset::load_indices (name may repeat across delta segments).".to_string(),
    })
}

pub async fn delete_index(request: SearchIndexDeleteRequest) -> Result<SearchIndexDeleteResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    if request.index_name.trim().is_empty() {
        anyhow::bail!("index_name must not be empty");
    }
    let mut ds = Dataset::open(&request.dataset)
        .await
        .with_context(|| format!("failed to open Lance dataset at {}", request.dataset))?;
    ds.drop_index(&request.index_name)
        .await
        .with_context(|| format!("failed to drop index '{}'", request.index_name))?;
    Ok(SearchIndexDeleteResponse {
        dataset: request.dataset,
        index_name: request.index_name,
        status: "ok".to_string(),
        note: "Index dropped via Lance DatasetIndexExt::drop_index.".to_string(),
    })
}

pub async fn rebuild_indices(request: SearchIndexRebuildRequest) -> Result<SearchIndexRebuildResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    let mut opts = if request.retrain {
        OptimizeOptions::retrain()
    } else {
        OptimizeOptions::default().num_indices_to_merge(request.merge_num_indices)
    };
    if let Some(ref name) = request.index_name {
        if name.trim().is_empty() {
            anyhow::bail!("index_name must not be empty when set");
        }
        opts = opts.index_names(vec![name.clone()]);
    }
    let mut ds = Dataset::open(&request.dataset)
        .await
        .with_context(|| format!("failed to open Lance dataset at {}", request.dataset))?;
    ds.optimize_indices(&opts)
        .await
        .with_context(|| "optimize_indices failed")?;
    let note = if request.retrain {
        "optimize_indices with retrain=true (v3 vector indices; see Lance OptimizeOptions::retrain)."
            .to_string()
    } else {
        format!(
            "optimize_indices merge path (retrain=false, num_indices_to_merge={:?}).",
            request.merge_num_indices
        )
    };
    Ok(SearchIndexRebuildResponse {
        dataset: request.dataset,
        index_name: request.index_name,
        retrain: request.retrain,
        status: "ok".to_string(),
        note,
    })
}

pub async fn create_index(request: SearchIndexRequest) -> Result<SearchIndexResponse> {
    validate_dataset_path(&request.dataset).await?;
    super::lance::build_vector_index(request).await
}

pub async fn reorder_ivf_layout(request: SearchIndexReorderRequest) -> Result<SearchIndexReorderResponse> {
    super::lance::reorder_ivf(request).await
}

pub(crate) fn ensure_utf8_column(schema: &LanceSchema, name: &str, role: &str) -> Result<()> {
    let field = schema.field(name).ok_or_else(|| {
        anyhow::anyhow!(
            "source Lance dataset has no column '{}' ({})",
            name,
            role
        )
    })?;
    match field.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 => Ok(()),
        _ => anyhow::bail!(
            "column '{}' ({}) must be Utf8 or LargeUtf8 for this import path",
            name,
            role
        ),
    }
}

/// Stream-copy an existing Lance dataset into a new target URI (schema preserved).
pub async fn import_from_lance(request: SearchImportLanceRequest) -> Result<SearchImportLanceResponse> {
    super::lance::import_lance_copy(request).await
}

pub async fn validate_dataset_path(dataset: &str) -> Result<()> {
    if dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    if std::path::Path::new(dataset).exists() {
        Dataset::open(dataset)
            .await
            .with_context(|| format!("failed to open Lance dataset at {}", dataset))?;
    }
    Ok(())
}

pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_is_deterministic_and_normalized() {
        let left = embed_text("agent search storage", 32).unwrap();
        let right = embed_text("agent search storage", 32).unwrap();
        assert_eq!(left, right);
        let norm = left.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }
}
