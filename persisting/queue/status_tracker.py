"""Per-bucket production and consumption status tracker."""

from __future__ import annotations

from typing import Any

from .metadata import BatchMeta, FieldMeta, SampleMeta


class StatusTracker:
    """Track production and per-task consumption status for one bucket."""

    def __init__(self, partition_id: str):
        self.partition_id = partition_id
        self.production_status: dict[int, dict[str, str]] = {}
        self.field_meta: dict[int, dict[str, tuple[Any, Any]]] = {}
        self.consumption_status: dict[str, set[int]] = {}
        self.key_to_index: dict[str, int] = {}

    def mark_produced(
        self,
        global_indexes: list[int],
        fields: list[str],
        dtypes: dict[str, Any] | None = None,
        shapes: dict[str, Any] | None = None,
    ) -> None:
        dtypes = dtypes or {}
        shapes = shapes or {}
        for idx in global_indexes:
            self.production_status[idx] = {field: "ready" for field in fields}
            self.field_meta[idx] = {field: (dtypes.get(field), shapes.get(field)) for field in fields}

    def scan_ready(self, fields: list[str], task_name: str) -> list[int]:
        consumed = self.consumption_status.setdefault(task_name, set())
        ready: list[int] = []
        for idx in sorted(self.production_status):
            if idx in consumed:
                continue
            status = self.production_status[idx]
            if all(status.get(field) == "ready" for field in fields):
                ready.append(idx)
        return ready

    def make_batch_meta(self, global_indexes: list[int], fields: list[str]) -> BatchMeta:
        samples: list[SampleMeta] = []
        for idx in global_indexes:
            sample_fields: dict[str, FieldMeta] = {}
            for name in fields:
                dtype, shape = self.field_meta.get(idx, {}).get(name, (None, None))
                sample_fields[name] = FieldMeta(
                    name=name,
                    dtype=dtype,
                    shape=shape,
                    production_status="ready",
                )
            samples.append(
                SampleMeta(
                    partition_id=self.partition_id,
                    global_index=idx,
                    fields=sample_fields,
                )
            )
        return BatchMeta(samples=samples)

    def mark_consumed(self, task_name: str, global_indexes: list[int]) -> None:
        self.consumption_status.setdefault(task_name, set()).update(global_indexes)

    def reset_consumption(self, task_name: str) -> None:
        self.consumption_status.pop(task_name, None)

    def register_key(self, key: str, global_index: int) -> None:
        self.key_to_index[key] = global_index

    def get_key(self, key: str) -> int | None:
        return self.key_to_index.get(key)
