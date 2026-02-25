//! 虚拟地址预留与按块填/读 — 供 BlockMappedBacking、UFFD 缺页填块使用。
//! 设计文档：docs/src/design/distributed_tiered_storage.md

use libc::{c_void, mprotect, mmap, munmap, MAP_ANONYMOUS, MAP_FAILED, MAP_PRIVATE, PROT_NONE, PROT_READ, PROT_WRITE};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::ptr::{self, NonNull};

#[cfg(not(target_os = "windows"))]
const PAGE_SIZE: usize = 4096;

fn page_align_down(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

fn page_align_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// 预留一段 PROT_NONE 的虚拟地址空间，不分配物理页。
/// 后续可用 region_copy_in 填块、region_copy_out 读块；UFFD 可在此区间注册缺页。
#[pyclass]
pub struct MmapRegion {
    ptr: ptr::NonNull<c_void>,
    len: usize,
}

impl MmapRegion {
    fn ptr_addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }
}

impl Drop for MmapRegion {
    fn drop(&mut self) {
        let ptr = self.ptr.as_ptr();
        if !ptr.is_null() && self.len > 0 {
            unsafe {
                munmap(ptr, self.len);
            }
        }
    }
}

// 仅由 Python GIL 下使用；指针不跨线程共享
unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

#[pymethods]
impl MmapRegion {
    #[new]
    fn new(length: usize) -> PyResult<Self> {
        if length == 0 {
            return Err(PyValueError::new_err("mmap_reserve length must be > 0"));
        }
        let ptr = unsafe {
            mmap(
                ptr::null_mut(),
                length,
                PROT_NONE,
                MAP_ANONYMOUS | MAP_PRIVATE,
                -1,
                0,
            )
        };
        if ptr == MAP_FAILED {
            let e = std::io::Error::last_os_error();
            return Err(PyValueError::new_err(format!("mmap_reserve failed: {}", e)));
        }
        let ptr = NonNull::new(ptr).ok_or_else(|| PyValueError::new_err("mmap returned null"))?;
        Ok(Self { ptr, len: length })
    }

    /// 在区间内 offset 处写入 data；会临时 mprotect 该页为 RW。
    fn copy_in(&self, offset: usize, data: &[u8]) -> PyResult<()> {
        let end = offset.checked_add(data.len()).ok_or_else(|| PyValueError::new_err("offset + data.len overflow"))?;
        if end > self.len {
            return Err(PyValueError::new_err("offset + data.len exceeds region length"));
        }
        let base = self.ptr_addr();
        let page_start = page_align_down(base + offset);
        let page_end = page_align_up(base + end);
        let mprot_len = page_end - page_start;
        unsafe {
            if mprotect(page_start as *mut c_void, mprot_len, PROT_READ | PROT_WRITE) != 0 {
                let e = std::io::Error::last_os_error();
                return Err(PyValueError::new_err(format!("mprotect failed: {}", e)));
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (base + offset) as *mut u8, data.len());
        }
        Ok(())
    }

    /// 从区间内 offset 处读取 length 字节。
    fn copy_out(&self, offset: usize, length: usize) -> PyResult<Vec<u8>> {
        let end = offset.checked_add(length).ok_or_else(|| PyValueError::new_err("offset + length overflow"))?;
        if end > self.len {
            return Err(PyValueError::new_err("offset + length exceeds region length"));
        }
        let base = self.ptr_addr();
        let page_start = page_align_down(base + offset);
        let page_end = page_align_up(base + end);
        let mprot_len = page_end - page_start;
        unsafe {
            if mprotect(page_start as *mut c_void, mprot_len, PROT_READ) != 0 {
                let e = std::io::Error::last_os_error();
                return Err(PyValueError::new_err(format!("mprotect read failed: {}", e)));
            }
            let mut out = vec![0u8; length];
            ptr::copy_nonoverlapping((base + offset) as *const u8, out.as_mut_ptr(), length);
            Ok(out)
        }
    }

    #[getter]
    fn length(&self) -> usize {
        self.len
    }

    /// 区间基址（用于 UFFD 注册等）。仅当区间为当前进程有效 mmap 时有效。
    #[getter]
    fn base_address(&self) -> usize {
        self.ptr_addr()
    }
}

/// 预留虚拟地址区间（不分配物理页），返回 MmapRegion。
#[pyfunction]
pub fn mmap_reserve(length: usize) -> PyResult<Py<MmapRegion>> {
    let region = MmapRegion::new(length)?;
    Python::with_gil(|py| Py::new(py, region))
}
