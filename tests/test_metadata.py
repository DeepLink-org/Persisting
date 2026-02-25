from persisting.queue.metadata import BatchMeta, FieldMeta, SampleMeta


def test_batch_meta_basic_properties():
    samples = [
        SampleMeta(
            partition_id="p0",
            global_index=0,
            fields={"x": FieldMeta(name="x", dtype="float32", shape=(2,), production_status="ready")},
        ),
        SampleMeta(
            partition_id="p0",
            global_index=1,
            fields={"x": FieldMeta(name="x", dtype="float32", shape=(2,), production_status="ready")},
        ),
    ]
    meta = BatchMeta(samples=samples, custom_meta={0: {"k": "v"}})
    assert meta.size == 2
    assert meta.global_indexes == [0, 1]
    assert meta.field_names == ["x"]
    assert meta.custom_meta[0]["k"] == "v"


def test_batch_meta_roundtrip():
    meta = BatchMeta(
        samples=[
            SampleMeta(
                partition_id="p1",
                global_index=3,
                fields={"y": FieldMeta(name="y", dtype="int64", shape=(1,), production_status="ready")},
            )
        ]
    )
    payload = meta.to_dict()
    restored = BatchMeta.from_dict(payload)
    assert restored.size == 1
    assert restored.global_indexes == [3]
    assert restored.field_names == ["y"]
