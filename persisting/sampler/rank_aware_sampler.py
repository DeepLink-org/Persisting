"""Rank-aware sampler for distributed training."""

from __future__ import annotations

from typing import Any

from .base import BaseSampler


class RankAwareSampler(BaseSampler):
    """Keep same sampled indices for ranks within a data-parallel group."""

    def sample(
        self,
        ready_indexes: list[int],
        batch_size: int,
        dp_rank: int,
        batch_index: int,
        task_name: str,
        partition_id: str,
        *args: Any,
        **kwargs: Any,
    ) -> tuple[list[int], list[int]]:
        if dp_rank < 0:
            raise ValueError("dp_rank must be >= 0")

        task_states = self._states.setdefault(partition_id, {}).setdefault(task_name, {})
        rank_states = task_states.setdefault(dp_rank, {})
        if batch_index in rank_states:
            sampled = rank_states[batch_index]
            return sampled, sampled

        sampled = ready_indexes[:batch_size]
        if len(sampled) < batch_size:
            return [], []
        rank_states[batch_index] = sampled
        return sampled, sampled
