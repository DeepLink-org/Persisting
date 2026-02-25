//! 块级文件 I/O — 供分层存储 L3/缺页填块使用。
//! 设计文档：docs/src/design/distributed_tiered_storage.md

use pyo3::exceptions::PyIOError;
use pyo3::prelude::*;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

/// 从文件指定偏移量读取恰好 `len` 字节。
/// 用于 Block 粒度读：L3 文件 / 缺页时从文件拉取一块。
#[pyfunction]
pub fn block_read(path: &str, offset: u64, len: usize) -> PyResult<Vec<u8>> {
    let mut f = File::open(path).map_err(|e| PyIOError::new_err(e.to_string()))?;
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| PyIOError::new_err(e.to_string()))?;
    let mut buf = vec![0u8; len];
    let n = f.read(&mut buf).map_err(|e| PyIOError::new_err(e.to_string()))?;
    if n != len {
        buf.truncate(n);
    }
    Ok(buf)
}

/// 向文件指定偏移量写入 `data`。
/// 用于 Block 粒度写：写回 L3、或预取时落盘。
#[pyfunction]
pub fn block_write(path: &str, offset: u64, data: &[u8]) -> PyResult<()> {
    let mut f = File::options()
        .write(true)
        .create(true)
        .open(path)
        .map_err(|e| PyIOError::new_err(e.to_string()))?;
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| PyIOError::new_err(e.to_string()))?;
    f.write_all(data).map_err(|e| PyIOError::new_err(e.to_string()))?;
    f.sync_all().map_err(|e| PyIOError::new_err(e.to_string()))?;
    Ok(())
}
