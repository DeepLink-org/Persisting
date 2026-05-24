//! userfaultfd 缺页处理（仅 Linux）：注册区间、读缺页事件、UFFDIO_COPY 填页。
//! 设计见 distributed_tiered_storage 6.2、6.5.1。与 TieredLoop 的填页逻辑一致，可后续并入同一事件循环。

#![cfg(target_os = "linux")]

use libc::{c_void, ioctl, read, syscall, SYS_userfaultfd, O_CLOEXEC};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const PAGE_SIZE: usize = 4096;

// Linux userfaultfd.h 常量（与内核一致）
const UFFD_EVENT_PAGEFAULT: u8 = 12;
const UFFDIO_API: u64 = 0xC018AA00_u64;
const UFFDIO_REGISTER: u64 = 0xC020AA01_u64;
const UFFDIO_COPY: u64 = 0xC038AA03_u64;
const UFFDIO_REGISTER_MODE_MISSING: u64 = 1;

#[repr(C)]
struct UffdioApi {
    api: u64, // 0xAA
    features: u64,
    ioctl_bits: u64,
}

#[repr(C)]
struct UffdioRange {
    start: u64,
    len: u64,
}

#[repr(C)]
struct UffdioRegister {
    range: UffdioRange,
    mode: u64,
    ioctls: u64,
}

#[repr(C)]
struct UffdioCopy {
    dst: u64,
    src: u64,
    len: u64,
    mode: u64,
    copy: i64,
}

/// read(uffd_fd) 返回的消息；仅解析 PAGEFAULT 所需字段。
#[repr(C)]
struct UffdMsg {
    event: u8,
    _pad1: [u8; 3],
    _reserved1: u32,
    _reserved2: u64,
    pagefault_flags: u64,
    pagefault_address: u64,
}

/// 创建 userfaultfd 并启用 API。
fn uffd_create() -> std::io::Result<i32> {
    let fd = unsafe { syscall(SYS_userfaultfd, O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = fd as i32;
    let api = UffdioApi {
        api: 0xAA,
        features: 0,
        ioctl_bits: 0,
    };
    if unsafe { ioctl(fd, UFFDIO_API as i32, &api) } < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e);
    }
    Ok(fd)
}

/// 向 UFFD 注册 [start, start+len) 区间，缺页时由内核投递到 fd。
fn uffd_register(fd: i32, start: u64, len: u64) -> std::io::Result<()> {
    let reg = UffdioRegister {
        range: UffdioRange { start, len },
        mode: UFFDIO_REGISTER_MODE_MISSING,
        ioctls: 0,
    };
    if unsafe { ioctl(fd, UFFDIO_REGISTER as i32, &reg) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 用 UFFDIO_COPY 将 src 的 len 字节填入 dst（页对齐），并唤醒触缺页线程。
fn uffd_copy(fd: i32, dst: u64, src: *const u8, len: usize) -> std::io::Result<()> {
    let mut copy = UffdioCopy {
        dst,
        src: src as u64,
        len: len as u64,
        mode: 0,
        copy: 0,
    };
    if unsafe { ioctl(fd, UFFDIO_COPY as i32, &mut copy) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 从 L3 文件按 block 读取一页数据，用于缺页填页。
/// path: L3 文件路径；block_id: 块号；block_size: 块大小（≥ PAGE_SIZE）；page_offset_in_block: 该页在块内偏移（0 或 页对齐）。
fn fetch_page_from_block(
    path: &str,
    block_id: u64,
    block_size: usize,
    page_offset_in_block: usize,
) -> std::io::Result<[u8; PAGE_SIZE]> {
    let offset = block_id as u64 * block_size as u64 + page_offset_in_block as u64;
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut page = [0u8; PAGE_SIZE];
    f.read_exact(&mut page)?;
    Ok(page)
}

/// 缺页处理循环：从 uffd_fd 读事件，对 PAGEFAULT 从 L3 拉一页并用 UFFDIO_COPY 填页。
/// 调用前调用方需已对 [base, base+len) 做 UFFDIO_REGISTER。
/// 在独立线程中运行，不持 GIL；与设计 6.2.2 一致。
pub fn run_uffd_handler(
    uffd_fd: i32,
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
) -> std::io::Result<()> {
    let base = base as u64;
    let mut msg: UffdMsg = unsafe { std::mem::zeroed() };
    let msg_size = std::mem::size_of::<UffdMsg>();
    let mut page_buf = [0u8; PAGE_SIZE];

    loop {
        let n = unsafe { read(uffd_fd, &mut msg as *mut _ as *mut c_void, msg_size) };
        if n <= 0 {
            break;
        }
        if msg.event != UFFD_EVENT_PAGEFAULT {
            continue;
        }
        let fault_addr = msg.pagefault_address;
        if fault_addr < base || fault_addr >= base + len as u64 {
            continue;
        }
        let page_start = fault_addr & !(PAGE_SIZE as u64 - 1);
        let offset_in_region = page_start - base;
        let block_id = offset_in_region / block_size as u64;
        let page_offset_in_block = (offset_in_region % block_size as u64) as usize;

        match fetch_page_from_block(&path, block_id, block_size, page_offset_in_block) {
            Ok(page) => {
                page_buf = page;
                if uffd_copy(uffd_fd, page_start, page_buf.as_ptr(), PAGE_SIZE).is_err() {
                    // 填页失败可记录并跳过，触缺页线程将再次 fault
                }
            }
            Err(_) => {
                // L3 读失败：可记录、重试或零页；此处简化不填，线程会再次 fault
            }
        }
    }
    Ok(())
}

/// 创建 userfaultfd，注册 [base, base+len)，并在后台线程启动缺页处理循环。
/// 返回 (uffd_fd, join_handle)。调用方需保证 path、base、len、block_size 在 handler 运行期间有效。
pub fn uffd_register_and_spawn_handler(
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
) -> std::io::Result<(i32, std::thread::JoinHandle<std::io::Result<()>>)> {
    let fd = uffd_create()?;
    uffd_register(fd, base as u64, len as u64)?;
    let fd_for_thread = fd;
    let handle = std::thread::Builder::new()
        .name("uffd-handler".into())
        .spawn(move || run_uffd_handler(fd_for_thread, base, len, block_size, path))?;
    Ok((fd, handle))
}

// ---------------------------------------------------------------------------
// Python 暴露（仅 Linux）
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
use pyo3::prelude::*;

/// 为 [base, base+len) 注册 userfaultfd 并启动缺页处理线程；L3 数据从 path 按 block 读取。
/// 返回 uffd fd（关闭该 fd 可停止 handler 线程）。block_size 须 ≥ 页大小（4096）。
#[cfg(target_os = "linux")]
#[pyfunction]
pub fn start_uffd_handler(
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
) -> PyResult<i32> {
    let (fd, handle) = uffd_register_and_spawn_handler(base, len, block_size, path)
        .map_err(|e| pyo3::exceptions::PyOSError::new_err(e.to_string()))?;
    // 后台线程脱离，关闭 fd 时 read 返回线程退出
    drop(handle);
    Ok(fd)
}
