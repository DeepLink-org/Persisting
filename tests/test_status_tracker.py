from persisting.queue.status_tracker import StatusTracker


def test_status_tracker_ready_and_consumption():
    tracker = StatusTracker(partition_id="default")
    tracker.mark_produced([0, 1, 2], ["a", "b"])
    ready = tracker.scan_ready(["a"], task_name="t1")
    assert ready == [0, 1, 2]

    tracker.mark_consumed("t1", [0, 2])
    ready2 = tracker.scan_ready(["a"], task_name="t1")
    assert ready2 == [1]

    tracker.reset_consumption("t1")
    ready3 = tracker.scan_ready(["a"], task_name="t1")
    assert ready3 == [0, 1, 2]
