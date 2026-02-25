"""主事件循环：预取与缺页统一调度（设计见 distributed_tiered_storage 6.2.1、6.5）。

已废弃：主事件循环必须在 Rust 侧实现（设计 6.2.2，避免 GIL 死锁）。请使用
persisting.core.TieredLoop（Rust 实现）。本模块仅保留作参考，不再从 store 导出。
"""

from __future__ import annotations

import asyncio
import os
import queue
import threading
from collections.abc import Callable
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .block import BlockId


class TieredEventLoop:
    """单线程事件循环：处理预取队列，预留 UFFD fd / 定时器接口。"""

    def __init__(self, fill_blocks: Callable[[list[BlockId]], None]):
        """
        :param fill_blocks: 调度逻辑核心：给定 BlockId 列表，从下层拉取并填入 L1（与缺页填页同路径）。
        """
        self._fill_blocks = fill_blocks
        self._reader, self._writer = os.pipe()
        self._prefetch_queue: queue.Queue[tuple[list[BlockId], Callable[[], None] | None]] = queue.Queue()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()

    def start(self) -> None:
        """在后台线程启动事件循环。"""
        if self._thread is not None and self._thread.is_alive():
            return

        def run() -> None:
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            self._loop = loop
            try:
                loop.add_reader(self._reader, self._on_wake)
                loop.run_forever()
            finally:
                loop.remove_reader(self._reader)
                loop.close()

        self._stop.clear()
        self._thread = threading.Thread(target=run, name="tiered-event-loop", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        """请求停止事件循环（写 pipe 唤醒后 loop.stop()）。"""
        self._stop.set()
        try:
            os.write(self._writer, b"\x00")
        except OSError:
            pass
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._loop.stop)

    def submit_prefetch(
        self,
        blocks: list[BlockId],
        on_done: Callable[[], None] | None = None,
    ) -> None:
        """将填块任务提交到调度器（与缺页触发的填块同路径）。线程安全。"""
        self._prefetch_queue.put((blocks, on_done))
        try:
            os.write(self._writer, b"\x00")
        except OSError:
            pass

    def _on_wake(self) -> None:
        """在事件循环线程中：读 pipe，消费一档预取任务并执行 fill_blocks。"""
        try:
            os.read(self._reader, 1)
        except OSError:
            pass
        if self._stop.is_set():
            if self._loop:
                self._loop.stop()
            return
        try:
            blocks, on_done = self._prefetch_queue.get_nowait()
        except queue.Empty:
            return
        self._fill_blocks(blocks)
        if on_done is not None:
            on_done()

    def register_fd(self, fd: int, callback: Callable[[], None]) -> None:
        """注册 fd：有可读事件时在循环中调用 callback。预留 UFFD 等。"""
        if self._loop is None:
            raise RuntimeError("事件循环未启动")
        self._loop.add_reader(fd, callback)

    def unregister_fd(self, fd: int) -> None:
        """移除 fd 监听。"""
        if self._loop is not None:
            self._loop.remove_reader(fd)

    def call_later(self, delay: float, callback: Callable[[], None]) -> asyncio.TimerHandle | None:
        """延迟执行。预留定时驱逐等。"""
        if self._loop is None:
            return None
        return self._loop.call_later(delay, callback)
