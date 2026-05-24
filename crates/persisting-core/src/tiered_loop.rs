//! 分层存储主事件循环（Rust 侧）：预取队列 + 循环线程，避免 Python GIL 死锁。
//! 设计见 distributed_tiered_storage 6.2.2、6.5；事件管理与派发见 6.5.1（类型、优先级、派发表、完成通知、背压）。

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use std::sync::{mpsc, Mutex};
use std::thread;

/// 单条预取任务：(partition_key, block_id)，与 Python BlockId 对应。
pub type BlockRef = (Vec<i64>, i64);

/// Rust 侧主事件循环：单线程消费预取队列，填页逻辑（fill_blocks）全在 Rust，不持 GIL。
#[pyclass]
pub struct TieredLoop {
    tx: Mutex<Option<mpsc::Sender<Vec<BlockRef>>>>,
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

#[pymethods]
impl TieredLoop {
    #[new]
    fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            thread_handle: Mutex::new(None),
        }
    }

    /// 启动循环线程：当前 fill_blocks 为占位（无操作），后续接入 block_read + copy_in。
    fn start(&self) -> PyResult<()> {
        if self.tx.lock().unwrap().is_some() {
            return Ok(());
        }
        let (tx, rx) = mpsc::channel::<Vec<BlockRef>>();
        let handle = thread::Builder::new()
            .name("tiered-loop".into())
            .spawn(move || {
                while let Ok(blocks) = rx.recv() {
                    // 占位：后续在此执行选层、block_read、copy_in，全在 Rust 内
                    let _ = blocks;
                }
            })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        *self.tx.lock().unwrap() = Some(tx);
        *self.thread_handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// 提交预取任务：blocks 为 [(partition_key_tuple, block_id), ...]，与 Python BlockId 一致。
    fn submit_prefetch(&self, blocks: &Bound<'_, PyAny>) -> PyResult<()> {
        let tx = self.tx.lock().unwrap();
        let tx = tx
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("TieredLoop 未 start"))?;
        let parsed = parse_block_list(blocks)?;
        tx.send(parsed)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(())
    }

    /// 停止循环：释放 Sender 并 join 线程。
    fn stop(&self) -> PyResult<()> {
        *self.tx.lock().unwrap() = None;
        if let Some(h) = self.thread_handle.lock().unwrap().take() {
            h.join().map_err(|_| {
                pyo3::exceptions::PyRuntimeError::new_err("tiered loop thread join failed")
            })?;
        }
        Ok(())
    }
}

fn parse_block_list(blocks: &Bound<'_, PyAny>) -> PyResult<Vec<BlockRef>> {
    let list = blocks.downcast::<pyo3::types::PyList>()?;
    let mut out = Vec::with_capacity(list.len());
    for i in 0..list.len() {
        let item = list.get_item(i)?;
        let (pk, bid) = parse_single_block(&item)?;
        out.push((pk, bid));
    }
    Ok(out)
}

fn parse_single_block(item: &Bound<'_, PyAny>) -> PyResult<BlockRef> {
    let tuple = item.downcast::<pyo3::types::PyTuple>()?;
    if tuple.len() != 2 {
        return Err(PyTypeError::new_err(
            "each block must be (partition_key, block_id)",
        ));
    }
    let pk_seq = tuple.get_item(0)?;
    let len: usize = pk_seq.len()?;
    let mut key = Vec::with_capacity(len);
    for i in 0..len {
        let v = pk_seq.get_item(i)?;
        let v: i64 = if let Ok(n) = v.extract::<i64>() {
            n
        } else if let Ok(s) = v.extract::<String>() {
            // TODO: BlockRef 当前使用 Vec<i64>，无法忠实表示字符串 partition key。
            // 改为支持 CoordValue（Int/Str/Bytes）后再启用字符串 key。
            return Err(PyTypeError::new_err(format!(
                "string partition keys not yet supported (got {s:?}); use integer keys"
            )));
        } else {
            return Err(PyTypeError::new_err(
                "partition_key elements must be int or str",
            ));
        };
        key.push(v);
    }
    let bid: i64 = tuple.get_item(1)?.extract()?;
    Ok((key, bid))
}
