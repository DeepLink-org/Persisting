"""缺页处理逻辑测试：start_uffd_handler 契约与端到端「从文件填页」。

- Linux: userfaultfd + UFFDIO_COPY
- macOS: Mach exception handler（标准模式，非 SIGSEGV 降级）
"""

import ctypes
import os
import sys

import pytest

try:
    from persisting.core import mmap_reserve, start_uffd_handler
except ImportError as e:
    pytest.skip(f"persisting._core 未安装: {e}", allow_module_level=True)

PAGE_SIZE = 4096


def _fault_handler_available():
    """start_uffd_handler 在 Linux/macOS 上可用。"""
    return start_uffd_handler is not None


def _is_linux():
    return sys.platform == "linux"


@pytest.mark.skipif(not _fault_handler_available(), reason="start_uffd_handler 仅 Linux/macOS 可用")
class TestStartUffdHandlerContract:
    """start_uffd_handler API 契约：返回值、关闭 fd、参数边界。"""

    def test_returns_fd_nonnegative(self, tmp_path):
        """调用返回非负整数 fd。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        path.write_bytes(b"x" * (PAGE_SIZE * 2))
        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        assert isinstance(fd, int) and fd >= 0
        os.close(fd)

    def test_close_fd_does_not_raise(self, tmp_path):
        """关闭返回的 fd 不抛错，handler 可正常退出。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        path.write_bytes(b"x" * (PAGE_SIZE * 2))
        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        os.close(fd)
        # 无异常即通过；handler 线程会在下次 recv 超时/read 后退出

    def test_block_size_page_aligned(self, tmp_path):
        """block_size 至少为一页（4096）；更小可能由实现拒绝或行为未定义，此处用合法值。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        path.write_bytes(b"y" * (PAGE_SIZE * 2))
        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        assert fd >= 0
        os.close(fd)


@pytest.mark.skipif(
    mmap_reserve is None or not _fault_handler_available(),
    reason="需 Unix 且 start_uffd_handler 可用",
)
class TestPageFaultFillsFromFile:
    """端到端：预留区间 + L3 文件 + 启动缺页 handler，访问触发缺页后读到文件内容。"""

    @pytest.mark.skipif(
        not _is_linux(),
        reason="e2e 缺页读仅在 Linux (UFFD) 下稳定验证；macOS Mach 路径可单独调试",
    )
    def test_first_page_fill_from_file(self, tmp_path):
        """访问第一页触发缺页，读到的内容与 L3 文件第一页一致。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        first_page = b"A" * PAGE_SIZE
        second_page = b"B" * PAGE_SIZE
        path.write_bytes(first_page + second_page)

        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        try:
            # 从基址读 5 字节，触发缺页，handler 从文件填页
            buf = ctypes.string_at(ctypes.c_void_p(region.base_address), 5)
            assert buf == b"AAAAA"
        finally:
            os.close(fd)

    @pytest.mark.skipif(
        not _is_linux(),
        reason="e2e 缺页读仅在 Linux (UFFD) 下稳定验证",
    )
    def test_second_page_fill_from_file(self, tmp_path):
        """访问第二页触发缺页，读到的内容与 L3 文件第二页一致。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        first_page = b"A" * PAGE_SIZE
        second_page = b"B" * PAGE_SIZE
        path.write_bytes(first_page + second_page)

        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        try:
            buf = ctypes.string_at(
                ctypes.c_void_p(region.base_address + PAGE_SIZE),
                5,
            )
            assert buf == b"BBBBB"
        finally:
            os.close(fd)

    @pytest.mark.skipif(
        not _is_linux(),
        reason="e2e 缺页读仅在 Linux (UFFD) 下稳定验证",
    )
    def test_both_pages_fill_consistent(self, tmp_path):
        """连续访问两页，两次读到的内容均与文件一致。"""
        region = mmap_reserve(PAGE_SIZE * 2)
        path = tmp_path / "l3.bin"
        path.write_bytes(b"0" * PAGE_SIZE + b"1" * PAGE_SIZE)

        fd = start_uffd_handler(
            region.base_address,
            region.length,
            PAGE_SIZE,
            str(path),
        )
        try:
            p0 = ctypes.string_at(ctypes.c_void_p(region.base_address), 3)
            p1 = ctypes.string_at(
                ctypes.c_void_p(region.base_address + PAGE_SIZE),
                3,
            )
            assert p0 == b"000"
            assert p1 == b"111"
        finally:
            os.close(fd)
