"""Sequential sampler — consume in order, aligned with TQ SequentialSampler."""

from __future__ import annotations

from typing import Any

from persisting.sampler.base import BaseSampler


class SequentialSampler(BaseSampler):
    """Sequential sampling without replacement.

    Selects the first batch_size elements from ready_indexes. Both
    sampled and consumed are the same set. Aligned with TransferQueue
    SequentialSampler.
    """

    def __init__(self) -> None:
        super().__init__()

    def sample(
        self,
        ready_indexes: list[int],
        batch_size: int,
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        sampled = ready_indexes[:batch_size]
        consumed = sampled
        return sampled, consumed
