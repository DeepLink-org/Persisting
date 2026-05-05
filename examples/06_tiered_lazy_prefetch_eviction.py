"""
KV Cache on Tiered Memory

L1（host 内存）容量有限。驱逐 = 触发 + 选取：

  触发（谁说要驱逐）：
    被动 — prefetch/put 需要空间时自动触发
    主动 — 调度器调用 evict() 提前释放

  选取（驱逐谁）：
    pin/unpin 先划定范围：pinned block 不可选
    然后在 unpinned block 中按策略选牺牲者：
      "lru"           最久未访问的 block 先走（默认）
      "prefix-aware"  保留共享前缀，优先驱逐 session 尾部 block
      自定义          传入 (block_id, last_access, session, position) → score

main() 展示四个阶段：
  1. 写满 L1
  2. Decode — pin/unpin + 被动 LRU
  3. 调度器 evict() 主动释放
  4. 冷 session 复活
"""

from __future__ import annotations

import numpy as np

import persisting
from persisting.core import Dimension

# ── 地址空间 + 选取策略 ───────────────────────────────────

SESSION = Dimension("session", "str")
LAYER = Dimension("layer", "int")
HEAD = Dimension("head", "int")
TIME = Dimension("time", "int")

NUM_LAYERS = 32
NUM_HEADS = 8

kv = persisting.open(
    "kvcache/v1",
    dims=(SESSION, LAYER, HEAD, TIME),
    order_dim=TIME,
    partition_dims=(SESSION,),
    backend="tiered",
    shape=(1024, NUM_LAYERS, NUM_HEADS, 4096),
    dtype=np.float16,
    block_tokens=64,
    l1_max_blocks=2048,
    eviction="lru",
    # eviction="prefix-aware",
    # eviction=lambda blk: custom_score(blk),
)


# ── Prefill ────────────────────────────────────────────────

def prefill(sid: str, seq_len: int):
    for layer in range(NUM_LAYERS):
        for head in range(NUM_HEADS):
            data = model.compute_kv(sid, layer, head, seq_len)
            kv[sid, layer, head, 0:seq_len].put(data)


# ── Decode：被动驱逐 ──────────────────────────────────────
#
# prefetch 需要空间 → 按选取策略挑 unpinned block 驱逐
# pin 保护 → unpin 后回到候选池

def decode_step(sid: str, pos: int, stream):
    for layer in range(NUM_LAYERS):
        if layer + 1 < NUM_LAYERS:
            kv[sid, layer + 1, :, 0:pos].prefetch()

        ref = kv[sid, layer, :, 0:pos]
        with persisting.pin(ref, after=stream):
            attention(ref.tensor(), stream)

        kv[sid, layer, :, pos : pos + 1].put(model.single_token_kv(layer))


# ── Batch 调度：主动驱逐 ──────────────────────────────────
#
# 调度器调用 evict() → 不走选取策略，直接把指定 session 推出 L1。
# 这比被动 LRU 更快腾出空间（LRU 可能选错——刚被抢占的 session
# 在 LRU 里还是"热"的，但调度器知道它不会再用了）。

def serve_batch(batch: list[tuple[str, int]], preempted: list[str], stream):
    for sid in preempted:
        kv[sid, :, :, :].evict()

    refs = [kv[sid, 0, :, 0:pos] for sid, pos in batch]
    for ref in refs:
        ref.prefetch()
    for ref in refs:
        ref.wait()

    for sid, pos in batch:
        decode_step(sid, pos, stream)


# ── Main ──────────────────────────────────────────────────

def main():
    stream = cuda.Stream()

    # 1. 写满 L1
    for i in range(100):
        prefill(f"req-{i:03d}", seq_len=128)

    # 2. Decode + 调度器驱逐
    while scheduler.has_active():
        batch, preempted = scheduler.schedule()
        serve_batch(batch, preempted, stream)

    # 3. 冷 session 复活
    prefill("req-000", seq_len=64)
    decode_step("req-000", 64, stream)
