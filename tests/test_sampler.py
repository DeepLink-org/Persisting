from persisting.sampler import GRPOGroupNSampler, RankAwareSampler, SequentialSampler


def test_sequential_sampler():
    sampler = SequentialSampler()
    sampled, consumed = sampler.sample([0, 1, 2, 3], 2)
    assert sampled == [0, 1]
    assert consumed == [0, 1]


def test_rank_aware_sampler_cache():
    sampler = RankAwareSampler()
    s1, c1 = sampler.sample(
        ready_indexes=[0, 1, 2, 3],
        batch_size=2,
        dp_rank=0,
        batch_index=0,
        task_name="task",
        partition_id="p",
    )
    s2, c2 = sampler.sample(
        ready_indexes=[100, 101],
        batch_size=2,
        dp_rank=0,
        batch_index=0,
        task_name="task",
        partition_id="p",
    )
    assert s1 == c1 == [0, 1]
    assert s2 == c2 == [0, 1]


def test_grpo_group_n_sampler():
    sampler = GRPOGroupNSampler(n_samples_per_prompt=2)
    sampled, consumed = sampler.sample(
        ready_indexes=[0, 1, 3, 4, 6, 7],
        batch_size=4,
        task_name="task",
        partition_id="p",
    )
    assert sampled == [0, 1, 3, 4]
    assert consumed == sampled
