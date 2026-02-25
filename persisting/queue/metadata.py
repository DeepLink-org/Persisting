"""Metadata models aligned with TransferQueue semantics."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class FieldMeta:
    """Metadata for a single field."""

    name: str
    dtype: Any = None
    shape: Any = None
    production_status: str = "not_produced"

    @property
    def is_ready(self) -> bool:
        return self.production_status == "ready"

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "dtype": str(self.dtype) if self.dtype is not None else None,
            "shape": tuple(self.shape) if self.shape is not None else None,
            "production_status": self.production_status,
        }


@dataclass
class SampleMeta:
    """Metadata for one sample."""

    partition_id: str
    global_index: int
    fields: dict[str, FieldMeta]

    @property
    def field_names(self) -> list[str]:
        return list(self.fields.keys())

    @property
    def is_ready(self) -> bool:
        return all(field.is_ready for field in self.fields.values())

    def select_fields(self, names: list[str]) -> "SampleMeta":
        return SampleMeta(
            partition_id=self.partition_id,
            global_index=self.global_index,
            fields={name: self.fields[name] for name in names if name in self.fields},
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "partition_id": self.partition_id,
            "global_index": self.global_index,
            "fields": {name: meta.to_dict() for name, meta in self.fields.items()},
        }


@dataclass
class BatchMeta:
    """Metadata for one sampled batch."""

    samples: list[SampleMeta]
    custom_meta: dict[int, dict[str, Any]] = field(default_factory=dict)
    extra_info: dict[str, Any] = field(default_factory=dict)

    @property
    def size(self) -> int:
        return len(self.samples)

    @property
    def global_indexes(self) -> list[int]:
        return [sample.global_index for sample in self.samples]

    @property
    def field_names(self) -> list[str]:
        if not self.samples:
            return []
        return self.samples[0].field_names

    def select_fields(self, names: list[str]) -> "BatchMeta":
        return BatchMeta(
            samples=[sample.select_fields(names) for sample in self.samples],
            custom_meta={idx: meta for idx, meta in self.custom_meta.items() if idx in self.global_indexes},
            extra_info=dict(self.extra_info),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "samples": [sample.to_dict() for sample in self.samples],
            "custom_meta": self.custom_meta,
            "extra_info": self.extra_info,
        }

    @classmethod
    def empty(cls) -> "BatchMeta":
        return cls(samples=[])

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "BatchMeta":
        samples: list[SampleMeta] = []
        for sample in data.get("samples", []):
            field_metas = {
                name: FieldMeta(
                    name=value.get("name", name),
                    dtype=value.get("dtype"),
                    shape=value.get("shape"),
                    production_status=value.get("production_status", "not_produced"),
                )
                for name, value in sample.get("fields", {}).items()
            }
            samples.append(
                SampleMeta(
                    partition_id=sample.get("partition_id", "default"),
                    global_index=int(sample.get("global_index", 0)),
                    fields=field_metas,
                )
            )
        return cls(
            samples=samples,
            custom_meta=data.get("custom_meta", {}),
            extra_info=data.get("extra_info", {}),
        )
