// SPDX-License-Identifier: GPL-2.0-or-later
//
// Safe wrappers for process-related syscalls.
// All unsafe libc FFI calls are contained here.

use log::error;

/// Daemonize the process: fork, setsid, redirect stdio to /dev/null.
/// Parent exits with status 0, child continues.
pub fn daemonize() {
    // SAFETY: fork/setsid/dup2/close are standard POSIX process operations.
    // Called before any threads are spawned, so fork is safe.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            error!("fork failed");
            std::process::exit(1);
        }
        if pid > 0 {
            // Parent exits
            std::process::exit(0);
        }

        // Child: new session
        if libc::setsid() < 0 {
            error!("setsid failed");
            std::process::exit(1);
        }

        // Redirect stdio to /dev/null
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            if libc::dup2(devnull, 0) < 0
                || libc::dup2(devnull, 1) < 0
                || libc::dup2(devnull, 2) < 0
            {
                error!("dup2 failed");
                std::process::exit(1);
            }
            if devnull > 2 {
                libc::close(devnull);
            }
        }

        let _ = libc::chdir(c"/".as_ptr());
    }
}

pub fn geteuid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}
