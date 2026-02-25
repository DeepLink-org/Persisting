"""TensorDict serialization helpers for queue backends.

Uses Pulsing's ZeroCopyDescriptor for efficient tensor storage when available.
Falls back to pickle for non-zerocopy objects.
"""

from __future__ import annotations

import pickle
import struct
from typing import Any

from .metadata import BatchMeta, FieldMeta, SampleMeta

ZEROCOPY_PREFIX = b"__pulsing_zc_v2__:"

try:
    from pulsing.core import ZeroCopyDescriptor

    ZEROCOPY_AVAILABLE = True
except ImportError:
    ZeroCopyDescriptor = None  # type: ignore[assignment,misc]
    ZEROCOPY_AVAILABLE = False

try:
    from tensordict import TensorDict

    TENSORDICT_AVAILABLE = True
except Exception:
    TensorDict = None  # type: ignore[assignment]
    TENSORDICT_AVAILABLE = False


def _is_tensordict(data: Any) -> bool:
    return TENSORDICT_AVAILABLE and isinstance(data, TensorDict)


def _batch_size_of(data: Any) -> int:
    if _is_tensordict(data):
        batch_size = getattr(data, "batch_size", [])
        if batch_size and int(batch_size[0]) > 0:
            return int(batch_size[0])
        return 1
    if isinstance(data, list):
        return len(data)
    return 1


def _single_from_tensordict(data: Any, idx: int) -> dict[str, Any]:
    row: dict[str, Any] = {}
    if _is_tensordict(data):
        for key, value in data.items():
            try:
                row[key] = value[idx]
            except Exception:
                row[key] = value
    return row


def _single_from_mapping(data: Any, idx: int) -> dict[str, Any]:
    if isinstance(data, list):
        value = data[idx]
        if not isinstance(value, dict):
            raise TypeError("list input must contain dict elements")
        return value
    if not isinstance(data, dict):
        raise TypeError("input data must be dict/list[dict]/TensorDict")
    row: dict[str, Any] = {}
    for key, value in data.items():
        if isinstance(value, list):
            row[key] = value[idx]
        else:
            try:
                row[key] = value[idx]
            except Exception:
                row[key] = value
    return row


def _extract_rows(data: Any) -> list[dict[str, Any]]:
    n = _batch_size_of(data)
    if _is_tensordict(data):
        return [_single_from_tensordict(data, i) for i in range(n)]
    return [_single_from_mapping(data, i) for i in range(n)]


def _field_meta_from_value(name: str, value: Any) -> FieldMeta:
    return FieldMeta(
        name=name,
        dtype=getattr(value, "dtype", None),
        shape=getattr(value, "shape", None),
        production_status="ready",
    )


def _try_zerocopy_descriptor(value: Any) -> "ZeroCopyDescriptor | None":
    """Extract a ZeroCopyDescriptor from an object via __zerocopy__ protocol."""
    if not ZEROCOPY_AVAILABLE:
        return None
    zc = getattr(value, "__zerocopy__", None)
    if zc is None or not callable(zc):
        return None
    try:
        descriptor = zc(None)
    except Exception:
        return None
    if not isinstance(descriptor, ZeroCopyDescriptor):
        return None
    return descriptor


def _pack_descriptor(desc: "ZeroCopyDescriptor") -> bytes:
    """Pack a ZeroCopyDescriptor into binary: PREFIX + [header_len LE u32] + header + raw_data.

    This mirrors Pulsing's wire format for single-message zerocopy payloads,
    prefixed so backends can detect zerocopy fields in stored bytes.
    """
    header = {
        "version": desc.version,
        "buffer_count": len(desc.buffers),
        "buffer_lengths": [len(bytes(b)) for b in desc.buffers],
        "dtype": desc.dtype,
        "shape": desc.shape,
        "strides": desc.strides,
        "transport": desc.transport,
        "checksum": desc.checksum,
    }
    header_bytes = pickle.dumps(header, protocol=pickle.HIGHEST_PROTOCOL)
    header_len = len(header_bytes)

    total_data = sum(header["buffer_lengths"])
    out = bytearray(len(ZEROCOPY_PREFIX) + 4 + header_len + total_data)
    offset = 0

    out[offset : offset + len(ZEROCOPY_PREFIX)] = ZEROCOPY_PREFIX
    offset += len(ZEROCOPY_PREFIX)

    struct.pack_into("<I", out, offset, header_len)
    offset += 4

    out[offset : offset + header_len] = header_bytes
    offset += header_len

    for buf_obj in desc.buffers:
        raw = bytes(buf_obj)
        out[offset : offset + len(raw)] = raw
        offset += len(raw)

    return bytes(out)


def _unpack_descriptor(payload: bytes) -> "ZeroCopyDescriptor | dict[str, Any]":
    """Unpack binary payload back to a ZeroCopyDescriptor (or dict fallback)."""
    data = payload[len(ZEROCOPY_PREFIX) :]
    if len(data) < 4:
        raise ValueError("Zerocopy payload too short")

    header_len = struct.unpack_from("<I", data, 0)[0]
    if len(data) < 4 + header_len:
        raise ValueError("Zerocopy payload truncated")

    header = pickle.loads(data[4 : 4 + header_len])

    offset = 4 + header_len
    raw_buffers: list[bytes] = []
    for buf_len in header["buffer_lengths"]:
        raw_buffers.append(data[offset : offset + buf_len])
        offset += buf_len

    if ZEROCOPY_AVAILABLE:
        return ZeroCopyDescriptor(
            buffers=raw_buffers,
            dtype=header.get("dtype"),
            shape=header.get("shape"),
            strides=header.get("strides"),
            transport=header.get("transport"),
            checksum=header.get("checksum"),
            version=header.get("version", 1),
        )

    # Fallback: return dict representation
    return {
        "version": header.get("version", 1),
        "buffers": raw_buffers,
        "dtype": header.get("dtype"),
        "shape": header.get("shape"),
        "strides": header.get("strides"),
        "transport": header.get("transport"),
        "checksum": header.get("checksum"),
    }


def encode_rows(
    data: Any,
    *,
    partition_id: str = "default",
    start_global_index: int = 0,
    custom_meta: dict[int, dict[str, Any]] | None = None,
    zerocopy_mode: str = "auto",
) -> tuple[list[dict[str, Any]], BatchMeta]:
    """Encode TensorDict/dict records to backend rows and return BatchMeta."""
    rows = _extract_rows(data)
    output: list[dict[str, Any]] = []
    samples: list[SampleMeta] = []
    custom_meta = custom_meta or {}

    for i, row in enumerate(rows):
        global_index = start_global_index + i
        tensor_fields: list[str] = []
        meta_fields: dict[str, dict[str, Any]] = {}
        encoded_row: dict[str, Any] = {}

        for key, value in row.items():
            tensor_fields.append(key)
            zc_desc = _try_zerocopy_descriptor(value) if zerocopy_mode != "off" else None
            if zc_desc is not None:
                encoded_row[key] = _pack_descriptor(zc_desc)
            else:
                if zerocopy_mode == "force":
                    raise ValueError(
                        f"zerocopy_mode=force but field '{key}' does not provide __zerocopy__"
                    )
                encoded_row[key] = pickle.dumps(value, protocol=pickle.HIGHEST_PROTOCOL)
            meta_fields[key] = _field_meta_from_value(key, value).to_dict()

        encoded_row["_tensor_fields"] = tensor_fields
        encoded_row["_meta_fields"] = meta_fields
        encoded_row["_partition_id"] = partition_id
        encoded_row["_global_index"] = global_index
        output.append(encoded_row)

        sample_fields = {
            name: FieldMeta(
                name=field["name"],
                dtype=field["dtype"],
                shape=field["shape"],
                production_status=field["production_status"],
            )
            for name, field in meta_fields.items()
        }
        samples.append(
            SampleMeta(
                partition_id=partition_id,
                global_index=global_index,
                fields=sample_fields,
            )
        )

    return output, BatchMeta(samples=samples, custom_meta=custom_meta)


def decode_rows(rows: list[dict[str, Any]], fields: list[str] | None = None) -> Any:
    """Decode backend rows to TensorDict if available, otherwise list[dict]."""
    decoded: list[dict[str, Any]] = []
    for row in rows:
        names = fields or row.get("_tensor_fields", [])
        item: dict[str, Any] = {}
        for name in names:
            if name not in row:
                continue
            value = row[name]
            if isinstance(value, (bytes, bytearray)):
                raw_bytes = bytes(value)
                if raw_bytes.startswith(ZEROCOPY_PREFIX):
                    item[name] = _unpack_descriptor(raw_bytes)
                else:
                    item[name] = pickle.loads(raw_bytes)
            else:
                item[name] = value
        decoded.append(item)

    if TENSORDICT_AVAILABLE and decoded:
        try:
            return TensorDict(decoded, batch_size=[len(decoded)])  # type: ignore[operator]
        except Exception:
            return decoded
    return decoded
