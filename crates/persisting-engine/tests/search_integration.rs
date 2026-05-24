//! Search 端到端集成测试：写入 → 建索引（IVF-PQ + FTS）→ list / query。
//!
//! ## 多步骤测试组织方式（Rust 生态）
//!
//! - **本仓库做法**：`tests/*.rs` 独立 crate + **`tempfile::TempDir`** + 小型 **`Harness` 结构体**
//!   封装场景步骤（建数据集、建索引、查询）。简单、无额外 DSL，CI 友好。
//! - **[`rstest`](https://crates.io/crates/rstest)**：`#[fixture]` 链式依赖，适合同一套 Lance 目录下多组参数矩阵。
//! - **[`cargo-nextest`](https://nexte.st)**：并行、超时、按名称筛选与更好的失败输出（测试**运行器**，不改变写法）。
//! - **`serial_test`**：若将来共享全局资源且不能并行，再给个别测试加 `#[serial]`。
//!
//! PQ 最小行数在引擎中与 `sample_rate * 2^num_bits` 对齐；此处用小维度、`pq_num_bits=4`、`pq_sample_rate=4`
//!（最少 **64** 行）以缩短索引构建时间。

use persisting_engine::{
    search_add, search_index, search_index_delete, search_index_list, search_query,
};
use persisting_proto::{
    SearchAddRequest, SearchIndexDeleteRequest, SearchIndexListRequest, SearchIndexRequest,
    SearchQueryRequest,
};
use tempfile::TempDir;

const EMBEDDING_DIM: usize = 32;

/// PQ 最小样本需求：`pq_sample_rate * 2^pq_num_bits` → `4 * 16 = 64`。
const MIN_ROWS_PQ: usize = 66;

fn tiny_index_request(dataset: String) -> SearchIndexRequest {
    SearchIndexRequest {
        dataset,
        vector_column: "embedding".into(),
        text_column: "text".into(),
        metric: "cosine".into(),
        num_partitions: Some(2),
        ivf_max_iters: Some(12),
        ivf_balance_factor: None,
        ivf_balance_postprocess: None,
        ivf_postprocess_max_cluster_ratio: None,
        ivf_sample_rate: None,
        ivf_target_partition_size: None,
        ivf_shuffle_partition_batches: None,
        ivf_shuffle_partition_concurrency: None,
        pq_num_sub_vectors: Some(8),
        pq_num_bits: Some(4),
        pq_max_iters: Some(12),
        pq_kmeans_redos: Some(1),
        pq_sample_rate: Some(4),
    }
}

fn seed_dataset(dataset: &str, rows: usize) {
    for i in 0..rows {
        search_add(SearchAddRequest {
            dataset: dataset.to_string(),
            id: Some(format!("doc-{i:04}")),
            text: format!(
                "integration test document {i} alpha beta gamma keyword {}",
                i % 7
            ),
            metadata: None,
            embedding_dim: EMBEDDING_DIM,
        })
        .unwrap_or_else(|e| panic!("search_add row {i}: {e:#}"));
    }
}

#[test]
fn search_end_to_end_write_index_query_vector_and_fts() {
    let dir = TempDir::new().unwrap();
    let dataset = dir.path().join("agent_ds").to_string_lossy().into_owned();

    seed_dataset(&dataset, MIN_ROWS_PQ);

    let idx_resp = search_index(tiny_index_request(dataset.clone()))
        .unwrap_or_else(|e| panic!("search_index: {e:#}"));
    assert_eq!(idx_resp.status, "ok");

    let list = search_index_list(SearchIndexListRequest {
        dataset: dataset.clone(),
    })
    .unwrap();
    assert_eq!(list.status, "ok");
    let names: Vec<&str> = list.indices.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names
            .iter()
            .any(|n| *n == persisting_engine::PERSISTING_VECTOR_INDEX_NAME),
        "expected IVF-PQ index name in {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|n| *n == persisting_engine::PERSISTING_FTS_INDEX_NAME),
        "expected FTS index name in {:?}",
        names
    );

    let q_vec = search_query(SearchQueryRequest {
        dataset: dataset.clone(),
        query: "integration test alpha".into(),
        mode: "vector".into(),
        k: 5,
        embedding_dim: EMBEDDING_DIM,
        text_column: "text".into(),
        filter: None,
        nprobes: Some(2),
        minimum_nprobes: None,
        maximum_nprobes: None,
        adaptive_nprobes_margin: None,
    })
    .unwrap();
    assert_eq!(q_vec.status, "ok");
    assert!(
        !q_vec.results.is_empty(),
        "vector query should return rows: {:?}",
        q_vec.note
    );

    let q_fts = search_query(SearchQueryRequest {
        dataset: dataset.clone(),
        query: "integration".into(),
        mode: "fts".into(),
        k: 8,
        embedding_dim: EMBEDDING_DIM,
        text_column: "text".into(),
        filter: None,
        nprobes: None,
        minimum_nprobes: None,
        maximum_nprobes: None,
        adaptive_nprobes_margin: None,
    })
    .unwrap();
    assert_eq!(q_fts.status, "ok");
    assert!(
        !q_fts.results.is_empty(),
        "fts query should return rows: {:?}",
        q_fts.note
    );

    let q_hybrid = search_query(SearchQueryRequest {
        dataset: dataset.clone(),
        query: "keyword".into(),
        mode: "hybrid".into(),
        k: 5,
        embedding_dim: EMBEDDING_DIM,
        text_column: "text".into(),
        filter: None,
        nprobes: Some(2),
        minimum_nprobes: None,
        maximum_nprobes: None,
        adaptive_nprobes_margin: None,
    })
    .unwrap();
    assert_eq!(q_hybrid.status, "ok");
    assert!(
        !q_hybrid.results.is_empty(),
        "hybrid query should return rows: {:?}",
        q_hybrid.note
    );
}

#[test]
fn search_index_delete_removes_named_index() {
    let dir = TempDir::new().unwrap();
    let dataset = dir.path().join("del_ds").to_string_lossy().into_owned();
    seed_dataset(&dataset, MIN_ROWS_PQ);
    search_index(tiny_index_request(dataset.clone())).unwrap();

    let del = search_index_delete(SearchIndexDeleteRequest {
        dataset: dataset.clone(),
        index_name: persisting_engine::PERSISTING_FTS_INDEX_NAME.into(),
    })
    .unwrap();
    assert_eq!(del.status, "ok");

    let list = search_index_list(SearchIndexListRequest {
        dataset: dataset.clone(),
    })
    .unwrap();
    let names: Vec<&str> = list.indices.iter().map(|e| e.name.as_str()).collect();
    assert!(
        !names
            .iter()
            .any(|n| *n == persisting_engine::PERSISTING_FTS_INDEX_NAME),
        "FTS index should be dropped: {:?}",
        names
    );
}
