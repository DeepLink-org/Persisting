"""KV-style interface on top of Queue APIs."""

from __future__ import annotations

from typing import Any

from .metadata import BatchMeta, FieldMeta, SampleMeta
from .queue import Queue


class KVInterface:
    """TransferQueue-like KV APIs backed by Persisting Queue."""

    def __init__(self, queue: Queue):
        self.queue = queue
        self._key_to_index: dict[str, dict[str, int]] = {}
        self._key_tags: dict[str, dict[str, dict[str, Any]]] = {}
        self._key_fields: dict[str, dict[str, list[str]]] = {}

    async def kv_put(
        self,
        key: str,
        data: Any,
        partition_id: str,
        tag: dict[str, Any] | None = None,
    ) -> None:
        meta = await self.queue.put(data=data, partition_id=partition_id)
        if meta is None or meta.size != 1:
            raise ValueError("kv_put expects single-sample tensor input")
        index = meta.global_indexes[0]
        self._key_to_index.setdefault(partition_id, {})[key] = index
        self._key_fields.setdefault(partition_id, {})[key] = meta.field_names
        self._key_tags.setdefault(partition_id, {})[key] = tag or {}

        if self.queue._distributed:
            bucket = await self.queue._resolve_partition_bucket(partition_id)
            await bucket.kv_register(key, index)

    async def kv_batch_put(
        self,
        keys: list[str],
        data: Any,
        partition_id: str,
        tags: list[dict[str, Any]] | None = None,
    ) -> None:
        if len(set(keys)) != len(keys):
            raise ValueError("duplicate keys are not allowed")

        meta = await self.queue.put(data=data, partition_id=partition_id)
        if meta is None or meta.size != len(keys):
            raise ValueError("kv_batch_put expects tensor batch size equal to number of keys")

        tags = tags or [{} for _ in keys]
        if len(tags) != len(keys):
            raise ValueError("tags size must match keys size")

        for idx, key in enumerate(keys):
            global_index = meta.global_indexes[idx]
            self._key_to_index.setdefault(partition_id, {})[key] = global_index
            self._key_fields.setdefault(partition_id, {})[key] = meta.field_names
            self._key_tags.setdefault(partition_id, {})[key] = tags[idx]

        if self.queue._distributed:
            bucket = await self.queue._resolve_partition_bucket(partition_id)
            for idx, key in enumerate(keys):
                await bucket.kv_register(key, meta.global_indexes[idx])

    async def kv_batch_get(
        self,
        keys: list[str],
        partition_id: str,
        fields: list[str] | None = None,
    ) -> Any:
        indexes = await self._resolve_keys(keys, partition_id)
        if not indexes:
            return []

        sample_fields = fields or self._infer_fields(keys, partition_id)
        samples = [
            SampleMeta(
                partition_id=partition_id,
                global_index=index,
                fields={
                    name: FieldMeta(
                        name=name,
                        dtype=None,
                        shape=None,
                        production_status="ready",
                    )
                    for name in sample_fields
                },
            )
            for index in indexes
        ]
        return await self.queue.get_data(BatchMeta(samples=samples), partition_id=partition_id)

    async def kv_list(self, partition_id: str) -> list[tuple[str, dict[str, Any]]]:
        mapping = self._key_tags.get(partition_id, {})
        return [(key, tag) for key, tag in mapping.items()]

    async def kv_clear(self, keys: list[str], partition_id: str) -> None:
        index_mapping = self._key_to_index.get(partition_id, {})
        indexes = [index_mapping[key] for key in keys if key in index_mapping]
        await self.queue.clear(indexes, partition_id=partition_id)
        for key in keys:
            self._key_to_index.get(partition_id, {}).pop(key, None)
            self._key_tags.get(partition_id, {}).pop(key, None)
            self._key_fields.get(partition_id, {}).pop(key, None)

    async def _resolve_keys(self, keys: list[str], partition_id: str) -> list[int]:
        if self.queue._distributed:
            bucket = await self.queue._resolve_partition_bucket(partition_id)
            result = await bucket.kv_resolve(keys)
            return [index for index in result.get("indexes", []) if index >= 0]
        index_mapping = self._key_to_index.get(partition_id, {})
        return [index_mapping[key] for key in keys if key in index_mapping]

    def _infer_fields(self, keys: list[str], partition_id: str) -> list[str]:
        for key in keys:
            fields = self._key_fields.get(partition_id, {}).get(key)
            if fields:
                return fields
        return []
