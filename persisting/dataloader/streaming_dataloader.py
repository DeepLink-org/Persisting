"""DataLoader wrapper for StreamingDataset."""

from __future__ import annotations

from typing import Any

try:
    from torch.utils.data import DataLoader
except Exception:
    DataLoader = None  # type: ignore[assignment]


class StreamingDataLoader:
    """Light wrapper to keep API close to TransferQueue."""

    def __init__(self, dataset: Any, **kwargs: Any):
        if DataLoader is None:
            raise ImportError("torch is required for StreamingDataLoader")
        self._loader = DataLoader(dataset, **kwargs)

    def __iter__(self):
        return iter(self._loader)
