"""Sampled read helper — one-batch consumption using a sampler and backend with get_by_indices."""

from __future__ import annotations

from typing import Any, Protocol, runtime_checkable

from persisting.sampler.base import BaseSampler
from persisting.queue.metadata import BatchMeta


@runtime_checkable
class SampledBackend(Protocol):
    """Backend that supports index-based read for sampler use."""

    def total_count(self) -> int: ...
    async def get_by_indices(self, indices: list[int]) -> list[dict[str, Any]]: ...
    async def get_meta(
        self,
        fields: list[str],
        batch_size: int,
        task_name: str,
        sampler: BaseSampler | None = None,
        **sampling_kwargs: Any,
    ) -> BatchMeta: ...
    async def get_data(self, batch_meta: BatchMeta, fields: list[str] | None = None) -> Any: ...


async def get_sampled_batch(
    backend: SampledBackend,
    sampler: BaseSampler,
    batch_size: int,
    consumed_offset: int,
    partition_id: str = "default",
    sampling_config: dict[str, Any] | None = None,
) -> tuple[list[dict[str, Any]], int]:
    """Read one batch in sampler order and return new consumed offset.

    Args:
        backend: Backend with total_count() and get_by_indices(indices).
        sampler: Sampler implementing sample(ready_indexes, batch_size, ...).
        batch_size: Number of records to request.
        consumed_offset: First index not yet consumed (0 = start).
        partition_id: Passed to sampler for state/cache (e.g. clear_cache).
        sampling_config: Optional kwargs for sampler.sample().

    Returns:
        (records, new_consumed_offset). records may be fewer than batch_size
        if fewer are available.
    """
    if hasattr(backend, "get_meta") and hasattr(backend, "get_data"):
        kwargs = dict(sampling_config or {})
        kwargs.setdefault("partition_id", partition_id)
        batch_meta = await backend.get_meta(
            fields=kwargs.get("fields", []),
            batch_size=batch_size,
            task_name=kwargs.get("task_name", "default"),
            sampler=sampler,
            **kwargs,
        )
        if batch_meta.size == 0:
            return [], consumed_offset
        data = await backend.get_data(batch_meta, fields=batch_meta.field_names)
        if isinstance(data, list):
            return data, consumed_offset + batch_meta.size
        return [dict(item) for item in data], consumed_offset + batch_meta.size

    total = backend.total_count()
    if consumed_offset >= total:
        return [], consumed_offset
    ready = list(range(consumed_offset, total))
    kwargs = dict(sampling_config or {})
    sampled, consumed = sampler.sample(ready, batch_size, **kwargs)
    if not sampled:
        return [], consumed_offset
    records = await backend.get_by_indices(sampled)
    return records, consumed_offset + len(consumed)
