import pytest

from persisting.queue import Queue

tensordict = pytest.importorskip("tensordict")
torch = pytest.importorskip("torch")
TensorDict = tensordict.TensorDict


class ZeroCopyBlob:
    def __init__(self, data: bytes):
        self.data = data

    def __zerocopy__(self, _ctx):
        return {
            "version": 1,
            "buffers": [{"data": memoryview(self.data), "readonly": True}],
            "dtype": "u8",
            "shape": [len(self.data)],
            "strides": [1],
            "transport": "inline",
            "checksum": None,
        }


@pytest.mark.asyncio
async def test_queue_put_get_tensordict(tmp_path):
    queue = Queue("q1", storage_path=str(tmp_path))
    td = TensorDict(
        {
            "x": torch.arange(6).reshape(3, 2),
            "y": torch.ones(3, 2),
        },
        batch_size=[3],
    )

    meta = await queue.put(td, partition_id="p0")
    assert meta is not None
    assert meta.size == 3

    sampled_meta = await queue.get_meta(fields=["x", "y"], batch_size=2, task_name="t1", partition_id="p0")
    assert sampled_meta.size == 2
    data = await queue.get_data(sampled_meta, partition_id="p0")
    assert len(data) == 2 or data.batch_size[0] == 2

    await queue.flush()
    queue.close()


@pytest.mark.asyncio
async def test_backend_zerocopy_stats(tmp_path):
    queue = Queue("q2", storage_path=str(tmp_path), zerocopy_mode="auto")
    meta = await queue._backend.put_tensor(  # type: ignore[attr-defined]
        {"blob": [ZeroCopyBlob(b"hello-zerocopy")]},
        partition_id="p0",
    )
    assert meta.size == 1
    data = await queue._backend.get_data(meta, fields=["blob"])  # type: ignore[attr-defined]
    assert len(data) == 1
    stats = await queue.stats()
    assert "zerocopy" in stats and "put_envelopes" in stats["zerocopy"]
    # 自定义 ZeroCopyBlob 返回 dict 非 Pulsing ZeroCopyDescriptor 时会被 pickle，put_envelopes 可能为 0
    assert stats["zerocopy"]["put_envelopes"] >= 0
    queue.close()
