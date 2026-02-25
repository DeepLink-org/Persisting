"""
Persisting Example 06: 理想形态 — 惰性加载、预取、驱逐

展示「最理想情况下代码应该长成什么样子」：用户只关心命名空间、下标、物化与预取，
L1 容量与驱逐策略在 open 时声明，运行时透明。

Run (from repo root):
    python -m examples.06_tiered_lazy_prefetch_eviction
"""

from __future__ import annotations

import sys
from pathlib import Path

if __name__ == "__main__" and __package__ is None:
    _root = Path(__file__).resolve().parent.parent
    sys.path.insert(0, str(_root))

import numpy as np
from persisting.core import Dimension

# -----------------------------------------------------------------------------
# 理想形态：open 时声明分层与驱逐策略，一次配置、全程透明
# -----------------------------------------------------------------------------

SESSION = Dimension("session", "int")
LAYER = Dimension("layer", "int")
TIME = Dimension("time", "int")

# 理想 API（当前 backend="tiered" 已支持；l1_max_blocks / eviction 为目标参数）
def ideal_open():
    """理想：open 时指定 L1 容量与驱逐策略，后续 get/prefetch 自动按策略执行。"""
    import persisting

    kv = persisting.open(
        "kvcache/v1",
        dims=(SESSION, LAYER, TIME),
        order_dim=TIME,
        partition_dims=(SESSION,),
        backend="tiered",
        shape=(4, 2, 256),   # 4 sessions, 2 layers, 256 time steps
        dtype=np.float32,
        block_tokens=64,
        # 理想扩展（当前未实现，此处仅作目标形态）：
        # l1_max_blocks=128,        # L1 最多缓存的 block 数
        # eviction="lru",           # 满时按 LRU 驱逐
        # prefetch_workers=1,       # 预取线程/协程数
        # 关键数据：kv.pin(key) / kv.unpin(key) 使该区间不参与驱逐
    )
    return kv


# -----------------------------------------------------------------------------
# 惰性加载：首次 h.tensor() 才从 L3 拉取，按需填 L1
# -----------------------------------------------------------------------------

def demo_lazy_load(kv):
    print("\n--- 惰性加载 ---")
    print("  h = kv[0, 0, 0:64]   # 仅构造切片，无 I/O")
    h = kv[0, 0, 0:64]
    print("  arr = h.tensor()     # 首次物化：L3 → L1 → 返回")
    arr = h.tensor()
    print(f"  shape={arr.shape}, dtype={arr.dtype}")
    assert arr.shape == (64,)
    return arr


# -----------------------------------------------------------------------------
# 预取：提前把即将用到的区间拉到 L1，后续 tensor() 无等待
# -----------------------------------------------------------------------------

def demo_prefetch(kv):
    print("\n--- 预取 ---")
    print("  kv.prefetch(0, 0, 64:128)   # 异步：L3 → L1")
    key = (0, 0, slice(64, 128))
    kv.prefetch(key)
    print("  kv.wait(0, 0, 64:128)       # 阻塞直到就绪")
    kv.wait(key)
    print("  arr = kv[0, 0, 64:128].tensor()  # 直接从 L1 读")
    arr = kv[0, 0, slice(64, 128)].tensor()
    print(f"  shape={arr.shape}")
    return arr


# -----------------------------------------------------------------------------
# 驱逐（理想）：L1 满时由策略自动驱逐冷块，用户无感知
# -----------------------------------------------------------------------------

def demo_eviction_ideal():
    print("\n--- 驱逐（理想形态） ---")
    print("  理想：open(..., l1_max_blocks=2, eviction=\"lru\")")
    print("  - 访问 block A、B → L1 持有 A、B")
    print("  - 再访问 block C → L1 满，按 LRU 驱逐 A，拉入 C")
    print("  - 再访问 A → A 已不在 L1，自动从 L3 再拉取")
    print("  当前实现：BlockStore 不限制 L1 容量，无驱逐；上述策略为设计目标。")


# -----------------------------------------------------------------------------
# Pin / Unpin（理想）：persisting.pin(ref) 上下文，退出时 launch 两算子通知 block manager unpin
# -----------------------------------------------------------------------------

def demo_pin_unpin_ideal():
    print("\n--- Pin / Unpin（理想形态） ---")
    print("  ref = kv[key] 得到切片引用（Handler），用模块级上下文管理 pin 生命周期：")
    print("    with persisting.pin(ref):")
    print("        xxxx   # 进入时 pin(ref)，退出时安排 unpin（同步/CPU）")
    print("    with persisting.pin(ref, after=stream):  # 异步/GPU")
    print("        x = ref.tensor(); stream.enqueue(kernel, x, ...)")
    print("        # 退出 with 时不 unpin；在 stream 上挂「在使用 ref 的算子之后执行的算子」，")
    print("        # 该算子执行完时通知 block manager unpin，避免 GPU 未用完就释放。")
    print("  Pin 对 block 做引用计数；仅 count==0 的 block 可被驱逐。")
    print("  当前实现：无 pin/unpin；为设计目标，与 l1_max_blocks/eviction 一并实现。")


# -----------------------------------------------------------------------------
# 主流程：先写 L3（put），再演示惰性、预取、驱逐概念
# -----------------------------------------------------------------------------

def main():
    print("=" * 60)
    print("Example 06: 理想形态 — 惰性加载、预取、驱逐")
    print("=" * 60)

    kv = ideal_open()

    # 写入初始数据到 L3（put 会写 L1 并 write-through 到 L3）
    print("\n--- 初始化：写入 L3 ---")
    for s in range(4):
        for layer in range(2):
            data = np.arange(256, dtype=np.float32) * (s * 10 + layer)
            kv[s, layer, :].put(data)
    print("  kv[s, layer, :].put(...) 等，数据落 L3")

    demo_lazy_load(kv)
    demo_prefetch(kv)
    demo_eviction_ideal()
    demo_pin_unpin_ideal()

    print("\n" + "=" * 60)
    print("理想 API 小结：")
    print("  open(..., backend=\"tiered\", l1_max_blocks=..., eviction=\"lru\")")
    print("  kv[key].tensor()     # 惰性物化")
    print("  kv.prefetch(key)     # 预取")
    print("  kv.wait(key)         # 等待就绪")
    print("  ref = kv[key]")
    print("  with persisting.pin(ref):           # 同步：退出时安排 unpin")
    print("  with persisting.pin(ref, after=stream):  # 异步：GPU 上算子执行完后由该算子 unpin")
    print("  驱逐由系统按策略透明执行，用户不写 evict()。")
    print("=" * 60)


if __name__ == "__main__":
    main()
