"""Python wrappers for Persisting Agent Search core APIs (PyO3 → Rust engine, no JSON wire)."""

from __future__ import annotations

from typing import Any, Iterable

from persisting import _core

# Bincode `engine_dispatch` wire version only (optional advanced use).
ENGINE_PROTOCOL_VERSION: int = _core.engine_protocol_version()


def embed_text(text: str, embedding_dim: int = 384) -> list[float]:
    return list(_core.search_embed_text(text, embedding_dim))


def add_document(
    dataset: str,
    text: str,
    *,
    id: str | None = None,
    metadata: dict[str, Any] | None = None,
    embedding_dim: int = 384,
) -> dict[str, Any]:
    return _core.search_add(dataset, text, id=id, metadata=metadata, embedding_dim=embedding_dim)


def add_documents_batch(
    dataset: str,
    documents: Iterable[dict[str, Any]],
    *,
    embedding_dim: int = 384,
    chunk_size: int = 256,
) -> dict[str, Any]:
    """批量写入（引擎 ``SearchAddBatch``，按 ``chunk_size`` 切块）。每行 dict 须含 ``text``，可选 ``id``、``metadata``。

    返回聚合结果：``added`` 为各批成功写入条数之和；``embedding_preview`` 来自首批。
    """
    rows = list(documents)
    return _core.search_add_batch(
        dataset, rows, embedding_dim=embedding_dim, chunk_size=chunk_size
    )


def query(
    dataset: str,
    query: str,
    *,
    mode: str = "hybrid",
    k: int = 10,
    embedding_dim: int = 384,
    text_column: str = "text",
    filter: str | None = None,
    nprobes: int | None = None,
    minimum_nprobes: int | None = None,
    maximum_nprobes: int | None = None,
    adaptive_nprobes_margin: float | None = None,
) -> dict[str, Any]:
    return _core.search_query(
        dataset,
        query,
        mode,
        k,
        embedding_dim,
        text_column,
        filter,
        nprobes,
        minimum_nprobes,
        maximum_nprobes,
        adaptive_nprobes_margin,
    )


def list_indices(dataset: str) -> dict[str, Any]:
    """Return non-system Lance index segments for the dataset (see `SearchIndexListResponse`)."""
    return _core.search_index_list(dataset)


def delete_index(dataset: str, index_name: str) -> dict[str, Any]:
    return _core.search_index_delete(dataset, index_name)


def rebuild_indices(
    dataset: str,
    *,
    index_name: str | None = None,
    retrain: bool = True,
    merge_num_indices: int | None = None,
) -> dict[str, Any]:
    """Call Lance `optimize_indices` (`SearchIndexRebuildRequest`)."""
    return _core.search_index_rebuild(
        dataset, index_name, retrain, merge_num_indices
    )


def create_index(
    dataset: str,
    *,
    vector_column: str = "embedding",
    text_column: str = "text",
    metric: str = "cosine",
    num_partitions: int | None = None,
    ivf_max_iters: int | None = None,
    ivf_balance_factor: float | None = None,
    ivf_balance_postprocess: bool | None = None,
    ivf_postprocess_max_cluster_ratio: float | None = None,
    ivf_sample_rate: int | None = None,
    ivf_target_partition_size: int | None = None,
    ivf_shuffle_partition_batches: int | None = None,
    ivf_shuffle_partition_concurrency: int | None = None,
    pq_num_sub_vectors: int | None = None,
    pq_num_bits: int | None = None,
    pq_max_iters: int | None = None,
    pq_kmeans_redos: int | None = None,
    pq_sample_rate: int | None = None,
) -> dict[str, Any]:
    """IVF/PQ fields match `persisting-proto::SearchIndexRequest` (see Lance `IvfBuildParams` / `PQBuildParams`)."""
    return _core.search_index(
        dataset,
        vector_column,
        text_column,
        metric,
        num_partitions,
        ivf_max_iters,
        ivf_balance_factor,
        ivf_balance_postprocess,
        ivf_postprocess_max_cluster_ratio,
        ivf_sample_rate,
        ivf_target_partition_size,
        ivf_shuffle_partition_batches,
        ivf_shuffle_partition_concurrency,
        pq_num_sub_vectors,
        pq_num_bits,
        pq_max_iters,
        pq_kmeans_redos,
        pq_sample_rate,
    )


def import_from_lance(
    target_dataset: str,
    source_lance: str,
    *,
    source_text_column: str = "text",
    source_id_column: str | None = None,
    embedding_dim: int = 384,
    limit: int | None = None,
) -> dict[str, Any]:
    return _core.search_import_lance(
        target_dataset,
        source_lance,
        source_text_column,
        source_id_column,
        embedding_dim,
        limit,
    )


def reorder_ivf(
    dataset: str,
    pivot_index: str,
    *,
    target: str | None = None,
    in_place: bool = False,
) -> dict[str, Any]:
    """IVF 物理重排（`lance-tools::reorder`）；须指定 `target` 或 `in_place=True` 之一。"""
    return _core.search_index_reorder(dataset, pivot_index, target, in_place)


__all__ = [
    "ENGINE_PROTOCOL_VERSION",
    "add_document",
    "add_documents_batch",
    "create_index",
    "delete_index",
    "embed_text",
    "import_from_lance",
    "list_indices",
    "query",
    "rebuild_indices",
    "reorder_ivf",
]
