import pytest

from persisting.queue.tensor_serde import ZEROCOPY_PREFIX, decode_rows, encode_rows

try:
    from pulsing.core import ZeroCopyDescriptor

    ZEROCOPY_AVAILABLE = True
except ImportError:
    ZEROCOPY_AVAILABLE = False

pytestmark = pytest.mark.skipif(
    not ZEROCOPY_AVAILABLE, reason="pulsing.core.ZeroCopyDescriptor not available"
)


class _Blob:
    """Object implementing Pulsing zerocopy protocol."""

    def __init__(self, data: bytes):
        self.data = data

    def __zerocopy__(self, _ctx):
        return ZeroCopyDescriptor(
            buffers=[memoryview(self.data)],
            dtype="u8",
            shape=[len(self.data)],
            strides=[1],
            transport="inline",
            checksum=None,
            version=1,
        )


def test_encode_rows_auto_uses_zerocopy_prefix():
    rows, meta = encode_rows({"blob": [_Blob(b"abc")]}, partition_id="p0")
    assert meta.size == 1
    payload = rows[0]["blob"]
    assert isinstance(payload, (bytes, bytearray))
    assert bytes(payload).startswith(ZEROCOPY_PREFIX)


def test_decode_rows_returns_zerocopy_descriptor():
    rows, _ = encode_rows({"blob": [_Blob(b"xyz")]}, partition_id="p0")
    decoded = decode_rows(rows, fields=["blob"])
    item = decoded[0]["blob"] if isinstance(decoded, list) else decoded[0]["blob"]
    assert isinstance(item, ZeroCopyDescriptor)
    assert item.version == 1
    assert item.dtype == "u8"
    assert item.shape == [3]
    assert bytes(item.buffers[0]) == b"xyz"


def test_roundtrip_large_buffer():
    data = bytes(range(256)) * 1024  # 256 KB
    rows, _ = encode_rows({"tensor": [_Blob(data)]}, partition_id="p0")
    decoded = decode_rows(rows, fields=["tensor"])
    item = decoded[0]["tensor"] if isinstance(decoded, list) else decoded[0]["tensor"]
    assert isinstance(item, ZeroCopyDescriptor)
    assert bytes(item.buffers[0]) == data


def test_encode_rows_force_raises_without_descriptor():
    with pytest.raises(ValueError):
        encode_rows({"x": [object()]}, zerocopy_mode="force")


def test_encode_rows_off_falls_back_pickle():
    rows, _ = encode_rows({"x": [123]}, zerocopy_mode="off")
    payload = rows[0]["x"]
    assert isinstance(payload, (bytes, bytearray))
    assert not bytes(payload).startswith(ZEROCOPY_PREFIX)


def test_multiple_buffers():
    class _MultiBlob:
        def __init__(self, a: bytes, b: bytes):
            self.a = a
            self.b = b

        def __zerocopy__(self, _ctx):
            return ZeroCopyDescriptor(
                buffers=[self.a, self.b],
                dtype="u8",
                shape=[len(self.a) + len(self.b)],
            )

    rows, _ = encode_rows({"multi": [_MultiBlob(b"hello", b"world")]}, partition_id="p0")
    decoded = decode_rows(rows, fields=["multi"])
    item = decoded[0]["multi"] if isinstance(decoded, list) else decoded[0]["multi"]
    assert isinstance(item, ZeroCopyDescriptor)
    assert len(item.buffers) == 2
    assert bytes(item.buffers[0]) == b"hello"
    assert bytes(item.buffers[1]) == b"world"
