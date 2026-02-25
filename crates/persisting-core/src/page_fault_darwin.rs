//! macOS 缺页处理：Mach Exception Handler（标准模式，非 SIGSEGV 降级）。
//! 设计见 distributed_tiered_storage 6.2：exception port + handler 线程收消息、填页、回复 KERN_SUCCESS。

#![cfg(target_os = "macos")]

use mach2::exc::{__Reply__exception_raise_t, __Request__exception_raise_t};
use mach2::exception_types::{
    exception_behavior_t, EXC_BAD_ACCESS, EXC_MASK_BAD_ACCESS, EXCEPTION_DEFAULT, MACH_EXCEPTION_CODES,
};
use mach2::kern_return::KERN_SUCCESS;
use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
use mach2::message::{
    mach_msg, mach_msg_header_t, mach_msg_size_t, MACH_MSG_TYPE_MOVE_SEND_ONCE, MACH_RCV_MSG,
    MACH_RCV_TIMEOUT, MACH_SEND_MSG, MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_RCV_LARGE,
    MACH_RCV_TIMED_OUT, MACH_MSGH_BITS,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE};
use mach2::task::task_set_exception_ports;
use mach2::traps::mach_task_self;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const PAGE_SIZE: usize = 4096;

/// 从 L3 文件按 block 读取一页（与 uffd 逻辑一致）。
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

/// 填页：mprotect(RW) + memcpy，与设计 6.2 一致。
fn fill_page(page_start: usize, page_data: &[u8; PAGE_SIZE]) -> std::io::Result<()> {
    let r = unsafe {
        libc::mprotect(
            page_start as *mut libc::c_void,
            PAGE_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            page_data.as_ptr(),
            page_start as *mut u8,
            PAGE_SIZE,
        );
    }
    Ok(())
}

/// 行为：EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES，对应 exception_raise 消息，fault 地址在 code[1]。
const BEHAVIOR: exception_behavior_t = (EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES) as exception_behavior_t;

/// MIG 约定：reply msgh_id = request msgh_id + 100。
const EXC_REPLY_OFFSET: i32 = 100;

/// 缺页处理循环：mach_msg 收异常 → 取 fault_addr = code[1] → fetch_page → fill_page → 回复 KERN_SUCCESS。
/// shutdown_read_fd：pipe 读端；当读端关闭或可读时退出循环。
pub fn run_mach_handler(
    exc_port: mach2::port::mach_port_t,
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
    shutdown_read_fd: std::os::unix::io::RawFd,
) -> std::io::Result<()> {
    let base_u = base as u64;
    let len_u = len as u64;
    let mut req_buf = [0u8; 256];
    let req_ptr = req_buf.as_mut_ptr() as *mut mach_msg_header_t;
    let recv_size = req_buf.len() as mach_msg_size_t;
    let timeout_ms = 200u32;

    loop {
        let ret = unsafe {
            mach_msg(
                req_ptr,
                MACH_RCV_MSG | MACH_RCV_LARGE | MACH_RCV_TIMEOUT,
                0,
                recv_size,
                exc_port,
                timeout_ms,
                MACH_PORT_NULL,
            )
        };
        if ret != MACH_MSG_SUCCESS {
            if ret == MACH_RCV_TIMED_OUT {
                let mut b = [0u8; 1];
                let n = unsafe { libc::read(shutdown_read_fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
                if n <= 0 {
                    break;
                }
            }
            continue;
        }

        let req: &__Request__exception_raise_t = unsafe { &*(req_buf.as_ptr() as *const __Request__exception_raise_t) };
        if req.exception != EXC_BAD_ACCESS as i32 {
            continue;
        }
        if req.codeCnt < 2 {
            continue;
        }
        // MACH_EXCEPTION_CODES 下内核发送两个 64 位值；mach2 的 code 为 [i32;2]，需按 64 位解析。
        let code_64: &[u64; 2] = unsafe { &*(req.code.as_ptr() as *const [u64; 2]) };
        let fault_addr = code_64[1];
        if fault_addr < base_u || fault_addr >= base_u + len_u {
            continue;
        }
        let page_start = fault_addr & !(PAGE_SIZE as u64 - 1);
        let offset_in_region = page_start - base_u;
        let block_id = offset_in_region / block_size as u64;
        let page_offset_in_block = (offset_in_region % block_size as u64) as usize;

        let page_start_usize = page_start as usize;
        match fetch_page_from_block(&path, block_id, block_size, page_offset_in_block) {
            Ok(page) => {
                if fill_page(page_start_usize, &page).is_err() {
                    // 填页失败，不回复或回复失败，让内核继续链
                }
            }
            Err(_) => {}
        }

        let reply_id = req.Head.msgh_id.wrapping_add(EXC_REPLY_OFFSET);
        let mut reply = __Reply__exception_raise_t {
            Head: mach2::message::mach_msg_header_t {
                msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0),
                msgh_size: std::mem::size_of::<__Reply__exception_raise_t>() as mach_msg_size_t,
                msgh_remote_port: req.Head.msgh_remote_port,
                msgh_local_port: MACH_PORT_NULL,
                msgh_voucher_port: 0,
                msgh_id: reply_id,
            },
            NDR: req.NDR,
            RetCode: KERN_SUCCESS,
        };
        let send_ret = unsafe {
            mach_msg(
                &mut reply.Head as *mut _ as *mut mach_msg_header_t,
                MACH_SEND_MSG,
                reply.Head.msgh_size,
                0,
                MACH_PORT_NULL,
                MACH_MSG_TIMEOUT_NONE,
                MACH_PORT_NULL,
            )
        };
        if send_ret != MACH_MSG_SUCCESS {
            // 无法回复，异常会继续传递
        }
    }
    Ok(())
}

/// 创建 exception port，注册 EXC_MASK_BAD_ACCESS，并启动 handler 线程。
/// 返回 (shutdown_write_fd, join_handle)。关闭 shutdown_write_fd 后 handler 会在下次超时后退出。
pub fn mach_register_and_spawn_handler(
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
) -> std::io::Result<(std::os::unix::io::RawFd, std::thread::JoinHandle<std::io::Result<()>>)> {
    let mut exc_port: mach2::port::mach_port_t = 0;
    let kr = unsafe {
        mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut exc_port)
    };
    if kr != KERN_SUCCESS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("mach_port_allocate: {:?}", kr),
        ));
    }
    let kr = unsafe {
        mach_port_insert_right(
            mach_task_self(),
            exc_port,
            exc_port,
            mach2::message::MACH_MSG_TYPE_MAKE_SEND,
        )
    };
    if kr != KERN_SUCCESS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("mach_port_insert_right: {:?}", kr),
        ));
    }
    let kr = unsafe {
        task_set_exception_ports(
            mach_task_self(),
            EXC_MASK_BAD_ACCESS,
            exc_port,
            BEHAVIOR,
            0,
        )
    };
    if kr != KERN_SUCCESS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("task_set_exception_ports: {:?}", kr),
        ));
    }

    let mut pipe_fds: [libc::c_int; 2] = [0, 0];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);
    // 子线程只持 read_fd；write_fd 返回给调用方，关闭即让 read 端收到 EOF，handler 退出。
    let handle = std::thread::Builder::new()
        .name("mach-fault-handler".into())
        .spawn(move || {
            run_mach_handler(exc_port, base, len, block_size, path, read_fd)
        })?;
    Ok((write_fd, handle))
}

// ---------------------------------------------------------------------------
// Python 暴露（与 Linux start_uffd_handler 同语义：返回 fd，关闭即停止 handler）
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
use pyo3::prelude::*;

/// macOS：启动 Mach 异常缺页 handler；返回 shutdown fd（关闭该 fd 可停止 handler）。
#[cfg(target_os = "macos")]
#[pyfunction]
pub fn start_uffd_handler(
    base: usize,
    len: usize,
    block_size: usize,
    path: String,
) -> PyResult<i32> {
    let (write_fd, _handle) = mach_register_and_spawn_handler(base, len, block_size, path)
        .map_err(|e| pyo3::exceptions::PyOSError::new_err(e.to_string()))?;
    Ok(write_fd as i32)
}
