"""Base sampler interface — aligned with TransferQueue sampler semantics.

Samplers control which indices are selected for consumption and which are
marked consumed (will not be returned again by the same consumer).
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any


class BaseSampler(ABC):
    """Base class for samplers that control how data is consumed from a queue.

    A sampler selects which indices to read from the set of available
    (ready) indices and which to mark as consumed. Aligned with
    TransferQueue's BaseSampler interface.

    Available samplers:
    - SequentialSampler: consume in order, no replacement.
    """

    def __init__(self) -> None:
        self._states: dict[Any, Any] = {}

    @abstractmethod
    def sample(
        self,
        ready_indexes: list[int],
        batch_size: int,
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        """Sample a batch of indices from the ready indices.

        Args:
            ready_indexes: Indices available for consumption (e.g. not yet consumed).
            batch_size: Number of samples to select.
            *args: Extra positional args for specific samplers.
            **kwargs: Extra keyword args (e.g. sampling_config).

        Returns:
            (sampled_indexes, consumed_indexes): indices to read and to mark consumed.
            consumed_indexes are typically the same as sampled_indexes for one-shot read.
        """
        raise NotImplementedError

    def __call__(
        self,
        ready_indexes: list[int],
        batch_size: int,
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        return self.sample(ready_indexes, batch_size, *args, **kwargs)

    def clear_cache(self, partition_id: str) -> None:
        """Clear cached state for the given partition."""
        if partition_id in self._states:
            self._states.pop(partition_id)
