"""Rust 层块级文件 I/O：block_read / block_write。供分层存储 L3、缺页填块使用。"""

import os
import tempfile

import pytest

try:
    from persisting.core import block_read, block_write, mmap_reserve, MmapRegion
except ImportError as e:
    pytest.skip(f"persisting._core 未安装: {e}", allow_module_level=True)


class TestBlockReadWrite:
    """block_read / block_write 单测：对齐块读写、短读、多块。"""

    def test_write_then_read_roundtrip(self):
        with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
            path = f.name
        try:
            data = b"x" * 256
            block_write(path, 0, data)
            out = block_read(path, 0, 256)
            assert out == data
        finally:
            os.unlink(path)

    def test_read_short_file_returns_available(self):
        """文件不足 len 时返回实际读到的长度（Rust 当前实现截断 buf）。"""
        with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
            path = f.name
            f.write(b"ab")
        try:
            out = block_read(path, 0, 10)
            assert out == b"ab"
        finally:
            os.unlink(path)

    def test_write_at_offset(self):
        with tempfile.NamedTemporaryFile(suffix=".bin", delete=False) as f:
            path = f.name
        try:
            block_write(path, 100, b"block")
            out = block_read(path, 100, 5)
            assert out == b"block"
        finally:
            os.unlink(path)

    def test_create_file_on_write(self):
        """block_write 使用 create(true)，写入新路径会创建文件。"""
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "new.bin")
            assert not os.path.exists(path)
            block_write(path, 0, b"hello")
            assert os.path.exists(path)
            assert block_read(path, 0, 5) == b"hello"
        # 目录删除后 path 不再存在，无需 unlink


@pytest.mark.skipif(mmap_reserve is None, reason="mmap_reserve 仅 Unix 可用")
class TestMmapRegion:
    """Rust 层 mmap 预留与 copy_in/copy_out。"""

    def test_mmap_region_base_address(self):
        """MmapRegion.base_address 返回基址（供 UFFD 注册等）。"""
        region = mmap_reserve(4096)
        base = region.base_address
        assert isinstance(base, int) and base != 0
        assert region.length == 4096

    def test_mmap_reserve_then_copy_in_out(self):
        region = mmap_reserve(8192)
        assert region.length == 8192
        region.copy_in(0, b"hello")
        out = region.copy_out(0, 5)
        assert out == b"hello"

    def test_mmap_reserve_copy_in_at_offset(self):
        region = mmap_reserve(8192)
        region.copy_in(100, b"block")
        assert region.copy_out(100, 5) == b"block"

    def test_mmap_reserve_zero_length_raises(self):
        import persisting._core as _core
        with pytest.raises(ValueError, match="length must be > 0"):
            _core.MmapRegion(0)

    def test_mmap_region_copy_out_bounds_raises(self):
        region = mmap_reserve(4096)
        with pytest.raises(ValueError, match="exceeds"):
            region.copy_out(4090, 10)
