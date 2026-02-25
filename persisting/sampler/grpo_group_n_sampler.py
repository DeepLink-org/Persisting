"""Group sampler for GRPO-style grouped samples."""

from __future__ import annotations

from typing import Any

from .base import BaseSampler


class GRPOGroupNSampler(BaseSampler):
    """Sample complete consecutive groups with fixed group size."""

    def __init__(self, n_samples_per_prompt: int = 1):
        super().__init__()
        if n_samples_per_prompt <= 0:
            raise ValueError("n_samples_per_prompt must be positive")
        self.n_samples_per_prompt = n_samples_per_prompt

    def sample(
        self,
        ready_indexes: list[int],
        batch_size: int,
        task_name: str = "",
        partition_id: str = "",
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        if batch_size % self.n_samples_per_prompt != 0:
            raise ValueError("batch_size must be multiple of n_samples_per_prompt")

        states = self._states.setdefault(partition_id, {}).setdefault(task_name, {})
        dp_rank = kwargs.get("dp_rank")
        batch_index = kwargs.get("batch_index")
        if dp_rank is not None and batch_index is not None:
            if dp_rank in states and batch_index in states[dp_rank]:
                return states[dp_rank][batch_index]

        required_groups = batch_size // self.n_samples_per_prompt
        sorted_indexes = sorted(ready_indexes)
        selected: list[int] = []
        found = 0
        i = 0
        while i <= len(sorted_indexes) - self.n_samples_per_prompt and found < required_groups:
            group = sorted_indexes[i : i + self.n_samples_per_prompt]
            if all(group[j + 1] - group[j] == 1 for j in range(len(group) - 1)):
                selected.extend(group)
                found += 1
                i += self.n_samples_per_prompt
            else:
                i += 1

        if found < required_groups:
            return [], []

        sampled = selected
        consumed = sampled.copy()

        if dp_rank is not None and batch_index is not None:
            states.setdefault(dp_rank, {})[batch_index] = (sampled, consumed)

        return sampled, consumed
