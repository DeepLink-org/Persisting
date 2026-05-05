//! Lance-backed search: append rows, IVF-PQ index, IVF physical reorder (engine-local remap path),
//! vector KNN, and Lance→Lance import.
//!
//! IVF/PQ build defaults mirror Lance `lance-tools` style knobs where applicable:
//! balance_factor `0.0`, optional postprocess ratio `2.5` when balancing postprocess is on.

use std::sync::Arc;

use anyhow::{Context, Result};
use lance::Dataset;
use lance::Error as LanceError;
use lance::dataset::scanner::QueryFilter;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Float64Array, LargeStringArray, RecordBatch,
    StringArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use lance::deps::datafusion::physical_plan::SendableRecordBatchStream;
use lance::index::vector::VectorIndexParams;
use lance::index::DatasetIndexExt;
use lance_index::vector::ivf::IvfBuildParams;
use lance_index::scalar::inverted::InvertedIndexParams;
use lance_index::scalar::FullTextSearchQuery;
use lance_index::vector::pq::PQBuildParams;
use lance_index::IndexType;
use lance_linalg::distance::DistanceType;
use persisting_proto::{
    SearchAddRequest, SearchAddResponse, SearchImportLanceRequest, SearchImportLanceResponse,
    SearchIndexReorderRequest, SearchIndexReorderResponse, SearchIndexRequest, SearchIndexResponse,
    SearchQueryRequest, SearchQueryResponse,
};

use super::agent::embed_text;

/// Logical name for the IVF-PQ vector index created by `search index build`.
pub const PERSISTING_VECTOR_INDEX_NAME: &str = "persisting_ivf_pq";

/// Logical name for the inverted (FTS) index on `SearchIndexRequest::text_column`.
pub const PERSISTING_FTS_INDEX_NAME: &str = "persisting_fts";

/// Query path uses fixed column `embedding` (matches `SearchAdd` / CLI defaults).
const QUERY_VECTOR_COLUMN: &str = "embedding";

const DEFAULT_IVF_POSTPROCESS_MAX_CLUSTER_RATIO: f32 = 2.5;

async fn dataset_exists(uri: &str) -> Result<bool> {
    match Dataset::open(uri).await {
        Ok(_) => Ok(true),
        Err(e) if matches!(e, LanceError::DatasetNotFound { .. }) => Ok(false),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

fn parse_metric(s: &str) -> Result<DistanceType> {
    match s.trim().to_lowercase().as_str() {
        "l2" | "euclidean" => Ok(DistanceType::L2),
        "cosine" => Ok(DistanceType::Cosine),
        "dot" | "ip" | "innerproduct" => Ok(DistanceType::Dot),
        other => anyhow::bail!("unsupported distance metric: {other} (use l2, cosine, or dot)"),
    }
}

fn default_ivf_partitions(row_count: usize) -> usize {
    if row_count == 0 {
        return 1;
    }
    let k = (row_count as f64).sqrt().ceil() as usize;
    k.max(1).min(row_count.max(1))
}

fn build_ivf_params(req: &SearchIndexRequest, row_count: usize) -> IvfBuildParams {
    let num_partitions = req
        .num_partitions
        .or_else(|| {
            req.ivf_target_partition_size
                .map(|t| (row_count.div_ceil(t)).max(1))
        })
        .unwrap_or_else(|| default_ivf_partitions(row_count));

    let mut ivf = IvfBuildParams::new(num_partitions)
        .with_balance_factor(req.ivf_balance_factor.unwrap_or(0.0));
    if req.ivf_balance_postprocess.unwrap_or(false) {
        ivf = ivf.with_postprocess_max_cluster_ratio(
            req.ivf_postprocess_max_cluster_ratio
                .unwrap_or(DEFAULT_IVF_POSTPROCESS_MAX_CLUSTER_RATIO),
        );
    }
    IvfBuildParams {
        max_iters: req.ivf_max_iters.unwrap_or(50),
        sample_rate: req.ivf_sample_rate.unwrap_or(256),
        target_partition_size: req.ivf_target_partition_size,
        shuffle_partition_batches: req.ivf_shuffle_partition_batches.unwrap_or(1024 * 10),
        shuffle_partition_concurrency: req.ivf_shuffle_partition_concurrency.unwrap_or(2),
        ..ivf
    }
}

fn build_pq_params(req: &SearchIndexRequest) -> PQBuildParams {
    PQBuildParams {
        num_sub_vectors: req.pq_num_sub_vectors.unwrap_or(16),
        num_bits: req.pq_num_bits.unwrap_or(8) as usize,
        max_iters: req.pq_max_iters.unwrap_or(50),
        kmeans_redos: req.pq_kmeans_redos.unwrap_or(1),
        sample_rate: req.pq_sample_rate.unwrap_or(256),
        ..Default::default()
    }
}

/// Minimum rows recommended for stable PQ training (see `PQBuildParams::sample_size` in lance-index).
fn min_rows_for_pq(pq: &PQBuildParams) -> usize {
    pq.sample_rate * 2_usize.pow(pq.num_bits as u32)
}

fn record_batch_for_document(
    id: &str,
    text: &str,
    embedding: &[f32],
) -> Result<RecordBatch> {
    let dim = embedding.len() as i32;
    let values = Float32Array::from_iter_values(embedding.iter().copied());
    let list = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        dim,
        Arc::new(values),
        None,
    )
    .context("build FixedSizeListArray for embedding")?;

    let schema = Arc::new(ArrowSchema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim,
            ),
            false,
        ),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![id.to_string()])) as ArrayRef,
            Arc::new(StringArray::from(vec![text.to_string()])) as ArrayRef,
            Arc::new(list) as ArrayRef,
        ],
    )
    .context("build RecordBatch")
}

fn embedding_dim_for_column(ds: &Dataset, col: &str) -> Result<i32> {
    let f = ds
        .schema()
        .field(col)
        .ok_or_else(|| anyhow::anyhow!("dataset has no column '{col}'"))?;
    match f.data_type() {
        DataType::FixedSizeList(inner, size) if inner.data_type() == &DataType::Float32 => Ok(size),
        other => anyhow::bail!(
            "column '{col}' must be FixedSizeList<Float32> (f32 embeddings), got {:?}",
            other
        ),
    }
}

/// Append one document row (id, text, embedding) or create a new dataset at `request.dataset`.
pub async fn append_document(request: SearchAddRequest) -> Result<SearchAddResponse> {
    if request.text.trim().is_empty() {
        anyhow::bail!("text must not be empty");
    }
    let embedding = embed_text(&request.text, request.embedding_dim)?;
    let id = request.id.unwrap_or_else(|| {
        format!(
            "doc-{:016x}",
            super::agent::stable_hash(
                format!("{}:{}", request.dataset, request.text).as_bytes(),
            )
        )
    });
    let batch = record_batch_for_document(&id, &request.text, &embedding)?;

    if dataset_exists(&request.dataset).await? {
        let ds = Arc::new(
            Dataset::open(&request.dataset)
                .await
                .with_context(|| format!("open dataset {}", request.dataset))?,
        );
        let dim = embedding_dim_for_column(&ds, "embedding")?;
        if dim as usize != request.embedding_dim {
            anyhow::bail!(
                "dataset embedding dim {} does not match request.embedding_dim {}",
                dim,
                request.embedding_dim
            );
        }
        InsertBuilder::new(ds.clone())
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute(vec![batch])
            .await
            .context("append row to Lance dataset")?;
    } else {
        InsertBuilder::new(request.dataset.as_str())
            .execute(vec![batch])
            .await
            .context("create Lance dataset with first row")?;
    }

    Ok(SearchAddResponse {
        dataset: request.dataset,
        id,
        embedding_dim: request.embedding_dim,
        embedding_preview: embedding.into_iter().take(8).collect(),
        status: "ok".to_string(),
        note: "Row written to Lance (columns: id, text, embedding).".to_string(),
    })
}

pub async fn build_vector_index(request: SearchIndexRequest) -> Result<SearchIndexResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    let mut ds = Dataset::open(&request.dataset)
        .await
        .with_context(|| format!("open dataset {}", request.dataset))?;

    let _ = embedding_dim_for_column(&ds, &request.vector_column)
        .with_context(|| format!("inspect vector column '{}'", request.vector_column))?;

    let row_count = ds
        .count_rows(None)
        .await
        .context("count_rows")?;

    let pq = build_pq_params(&request);
    let min_rows = min_rows_for_pq(&pq);
    if row_count < min_rows {
        anyhow::bail!(
            "not enough rows for PQ training: have {}, need at least {} (PQ sample_rate * 2^num_bits); add more rows or lower num_bits / sample_rate",
            row_count,
            min_rows
        );
    }

    let metric = parse_metric(&request.metric)?;
    let ivf = build_ivf_params(&request, row_count);
    let params = VectorIndexParams::with_ivf_pq_params(metric, ivf, pq);

    ds.create_index(
        &[request.vector_column.as_str()],
        IndexType::Vector,
        Some(PERSISTING_VECTOR_INDEX_NAME.to_string()),
        &params,
        true,
    )
    .await
    .context("create_index IVF-PQ")?;

    super::agent::ensure_utf8_column(ds.schema(), &request.text_column, "full-text index")?;
    ds.create_index(
        &[request.text_column.as_str()],
        IndexType::Inverted,
        Some(PERSISTING_FTS_INDEX_NAME.to_string()),
        &InvertedIndexParams::default(),
        true,
    )
    .await
    .context("create_index FTS (Inverted) on text column")?;

    Ok(SearchIndexResponse {
        dataset: request.dataset,
        status: "ok".to_string(),
        note: format!(
            "IVF-PQ index '{}' on '{}' (metric {:?}); FTS inverted index '{}' on '{}'.",
            PERSISTING_VECTOR_INDEX_NAME,
            request.vector_column,
            metric,
            PERSISTING_FTS_INDEX_NAME,
            request.text_column
        ),
    })
}

pub async fn reorder_ivf(request: SearchIndexReorderRequest) -> Result<SearchIndexReorderResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    if request.pivot_index.trim().is_empty() {
        anyhow::bail!("pivot_index must not be empty");
    }

    let params = super::ivf_physical_reorder::IvfPhysicalReorderParams {
        source: request.dataset.clone(),
        target: request.target.clone(),
        in_place: request.in_place,
        pivot_index: request.pivot_index.clone(),
        batch_size: 8192,
    };

    let mut log_buf = Vec::new();
    super::ivf_physical_reorder::run_physical_reorder(&params, &mut log_buf)
        .await
        .map_err(|e| anyhow::anyhow!("{:#}", e))?;

    let log = String::from_utf8_lossy(&log_buf).into_owned();
    Ok(SearchIndexReorderResponse {
        dataset: request.dataset.clone(),
        pivot_index: request.pivot_index.clone(),
        target: request.target.clone(),
        in_place: request.in_place,
        status: "ok".to_string(),
        note: "IVF physical reorder (engine remap path; indices remapped, PQ not retrained)."
            .to_string(),
        log,
    })
}

fn cell_to_json(col: &ArrayRef, row: usize) -> serde_json::Value {
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return serde_json::Value::String(a.value(row).to_string());
    }
    if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
        return serde_json::Value::String(a.value(row).to_string());
    }
    if let Some(a) = col.as_any().downcast_ref::<Float32Array>() {
        return serde_json::Number::from_f64(f64::from(a.value(row)))
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
    }
    if let Some(a) = col.as_any().downcast_ref::<Float64Array>() {
        return serde_json::Number::from_f64(a.value(row))
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null);
    }
    if let Some(a) = col.as_any().downcast_ref::<FixedSizeListArray>() {
        let inner = a.value(row);
        if let Some(f) = inner.as_any().downcast_ref::<Float32Array>() {
            let vec: Vec<f32> = (0..f.len()).map(|j| f.value(j)).collect();
            return serde_json::to_value(vec).unwrap_or(serde_json::Value::Null);
        }
    }
    serde_json::Value::String(format!("{:?}", col.data_type()))
}

fn row_to_json(batch: &RecordBatch, row: usize) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (i, field) in batch.schema().fields().iter().enumerate() {
        let col = batch.column(i);
        map.insert(field.name().clone(), cell_to_json(col, row));
    }
    serde_json::Value::Object(map)
}

fn apply_nprobes(scan: &mut lance::dataset::scanner::Scanner, request: &SearchQueryRequest) {
    if let Some(n) = request.nprobes {
        scan.nprobes(n);
    }
    if let Some(n) = request.minimum_nprobes {
        scan.minimum_nprobes(n);
    }
    if let Some(n) = request.maximum_nprobes {
        scan.maximum_nprobes(n);
    }
    if let Some(m) = request.adaptive_nprobes_margin {
        scan.adaptive_nprobes(m, None);
    }
}

fn apply_sql_filter(
    scan: &mut lance::dataset::scanner::Scanner,
    filter: Option<&str>,
) -> Result<()> {
    if let Some(f) = filter {
        let t = f.trim();
        if !t.is_empty() {
            scan.filter(t).context("scan.filter SQL")?;
        }
    }
    Ok(())
}

pub async fn vector_query(request: SearchQueryRequest) -> Result<SearchQueryResponse> {
    if request.dataset.trim().is_empty() {
        anyhow::bail!("dataset path must not be empty");
    }
    let mode = request.mode.to_lowercase();
    let text_col = request.text_column.trim();
    if text_col.is_empty() {
        anyhow::bail!("text_column must not be empty");
    }

    let ds = Dataset::open(&request.dataset)
        .await
        .with_context(|| format!("open dataset {}", request.dataset))?;

    match mode.as_str() {
        "fts" | "hybrid" => {
            super::agent::ensure_utf8_column(ds.schema(), text_col, "full-text search")?;
        }
        "vector" => {}
        other => anyhow::bail!("unknown query mode '{other}' (use vector, fts, or hybrid)"),
    }

    let embedding_full = embed_text(&request.query, request.embedding_dim)?;
    let embedding_preview: Vec<f32> = embedding_full.iter().copied().take(8).collect();

    let mut scan = ds.scan();

    let note = match mode.as_str() {
        "vector" => {
            let dim = embedding_dim_for_column(&ds, QUERY_VECTOR_COLUMN)? as usize;
            if dim != request.embedding_dim {
                anyhow::bail!(
                    "dataset embedding dim {} does not match request.embedding_dim {}",
                    dim,
                    request.embedding_dim
                );
            }
            let q = Float32Array::from_iter_values(embedding_full.iter().copied());
            scan.nearest(QUERY_VECTOR_COLUMN, &q, request.k)
                .context("nearest scan")?;
            apply_nprobes(&mut scan, &request);
            apply_sql_filter(&mut scan, request.filter.as_deref())?;
            format!("Vector kNN on '{QUERY_VECTOR_COLUMN}' (Lance scan.nearest).")
        }
        "fts" => {
            let fts_q = FullTextSearchQuery::new(request.query.clone())
                .with_column(text_col.to_string())
                .map_err(|e| anyhow::anyhow!("{:#}", e))?
                .limit(Some(request.k as i64));
            scan.full_text_search(fts_q).context("full_text_search")?;
            apply_sql_filter(&mut scan, request.filter.as_deref())?;
            format!("BM25 FTS on column '{text_col}' (requires inverted index, e.g. from search index build).")
        }
        "hybrid" => {
            let dim = embedding_dim_for_column(&ds, QUERY_VECTOR_COLUMN)? as usize;
            if dim != request.embedding_dim {
                anyhow::bail!(
                    "dataset embedding dim {} does not match request.embedding_dim {}",
                    dim,
                    request.embedding_dim
                );
            }
            let q = Float32Array::from_iter_values(embedding_full.iter().copied());
            scan.nearest(QUERY_VECTOR_COLUMN, &q, request.k)
                .context("nearest scan")?;
            scan.prefilter(true);
            apply_sql_filter(&mut scan, request.filter.as_deref())?;
            let fts_q = FullTextSearchQuery::new(request.query.clone())
                .with_column(text_col.to_string())
                .map_err(|e| anyhow::anyhow!("{:#}", e))?;
            scan.filter_query(QueryFilter::Fts(fts_q))
                .context("filter_query FTS prefilter for vector search")?;
            apply_nprobes(&mut scan, &request);
            format!(
                "Hybrid: vector kNN on '{QUERY_VECTOR_COLUMN}' with FTS prefilter on '{text_col}' (Lance nearest + QueryFilter::Fts, prefilter=true)."
            )
        }
        other => anyhow::bail!("unknown query mode '{other}' (use vector, fts, or hybrid)"),
    };

    let batch = scan
        .try_into_batch()
        .await
        .context("execute search query")?;

    let mut results = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        results.push(row_to_json(&batch, i));
    }

    Ok(SearchQueryResponse {
        dataset: request.dataset,
        mode: request.mode,
        k: request.k,
        query_embedding_preview: embedding_preview,
        results,
        status: "ok".to_string(),
        note,
    })
}

pub async fn import_lance_copy(
    request: SearchImportLanceRequest,
) -> Result<SearchImportLanceResponse> {
    if request.target_dataset.trim().is_empty() {
        anyhow::bail!("target_dataset must not be empty");
    }
    if request.source_lance.trim().is_empty() {
        anyhow::bail!("source_lance must not be empty");
    }
    if dataset_exists(&request.target_dataset).await? {
        anyhow::bail!(
            "target dataset path already exists: {}; choose a non-existing directory",
            request.target_dataset
        );
    }

    let src = Dataset::open(&request.source_lance)
        .await
        .with_context(|| format!("open source {}", request.source_lance))?;
    let schema = src.schema();
    super::agent::ensure_utf8_column(schema, &request.source_text_column, "text")?;
    if let Some(ref idcol) = request.source_id_column {
        super::agent::ensure_utf8_column(schema, idcol, "id")?;
    }
    let total = src
        .count_rows(None)
        .await
        .context("count source rows")?;
    let field_names: Vec<String> = schema.fields.iter().map(|f| f.name.clone()).collect();

    let mut scan = src.scan();
    if let Some(lim) = request.limit {
        scan.limit(Some(lim as i64), None).context("scan limit")?;
    }
    let stream = scan
        .try_into_stream()
        .await
        .context("source scan stream")?;
    let stream: SendableRecordBatchStream = stream.into();

    InsertBuilder::new(request.target_dataset.as_str())
        .execute_stream(stream)
        .await
        .context("write import stream to target")?;

    Ok(SearchImportLanceResponse {
        target_dataset: request.target_dataset,
        source_lance: request.source_lance,
        source_row_count: total,
        source_field_names: field_names,
        embedding_dim: request.embedding_dim,
        limit: request.limit,
        status: "ok".to_string(),
        note: "Copied rows from source Lance dataset to target URI via scan + InsertBuilder."
            .to_string(),
    })
}
