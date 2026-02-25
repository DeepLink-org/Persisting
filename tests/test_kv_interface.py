import pytest

from persisting.queue import KVInterface, Queue

tensordict = pytest.importorskip("tensordict")
torch = pytest.importorskip("torch")
TensorDict = tensordict.TensorDict


@pytest.mark.asyncio
async def test_kv_put_get_list_clear(tmp_path):
    queue = Queue("kvq", storage_path=str(tmp_path))
    kv = KVInterface(queue)

    td = TensorDict({"x": torch.arange(2).unsqueeze(0)}, batch_size=[1])
    await kv.kv_put("k1", td, partition_id="p0", tag={"stage": "train"})

    listed = await kv.kv_list("p0")
    assert listed[0][0] == "k1"

    data = await kv.kv_batch_get(["k1"], partition_id="p0", fields=["x"])
    assert len(data) == 1 or data.batch_size[0] == 1

    await kv.kv_clear(["k1"], partition_id="p0")
    empty = await kv.kv_batch_get(["k1"], partition_id="p0", fields=["x"])
    assert empty == []

    queue.close()
