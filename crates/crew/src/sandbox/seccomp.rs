//! A seccomp filter against terminal input injection: `ioctl(TIOCSTI)`
//! pushes characters into a terminal's input queue and `ioctl(TIOCLINUX)`
//! can paste the console selection. Flatpak refuses the same two.
//!
//! The kernel takes the ioctl request as a 32-bit value, so only the low
//! half of the argument is compared: a request with junk in the high bits
//! is still the same request (CVE-2019-10063).

use std::io;

/// The x32 ABI shares x86_64's audit arch and sets this bit in the syscall
/// number. Nothing crew runs uses it, so every x32 call is refused. On
/// aarch64 no valid syscall number has the bit, so the check is inert.
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// (audit arch, ioctl syscall number) of the native ABI and the 32-bit
/// compat ABI: AUDIT_ARCH_X86_64 and AUDIT_ARCH_I386, or AUDIT_ARCH_AARCH64
/// and AUDIT_ARCH_ARM.
#[cfg(target_arch = "x86_64")]
const ARCHES: [(u32, u32); 2] = [(0xC000_003E, 16), (0x4000_0003, 54)];
#[cfg(target_arch = "aarch64")]
const ARCHES: [(u32, u32); 2] = [(0xC000_00B7, 29), (0x4000_0028, 54)];

// Offsets into `struct seccomp_data`. The low half of `args[1]` is at 24 on
// little-endian machines, which both supported architectures are.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG1_LOW: u32 = 24;

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

/// The filter program. Jump offsets count from the next instruction.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn program() -> Vec<libc::sock_filter> {
    use libc::{BPF_ABS, BPF_JEQ, BPF_JGE, BPF_JMP, BPF_K, BPF_LD, BPF_RET, BPF_W};
    let [(native_arch, native_ioctl), (compat_arch, compat_ioctl)] = ARCHES;
    let load = BPF_LD | BPF_W | BPF_ABS;
    let jeq = BPF_JMP | BPF_JEQ | BPF_K;
    let ret = BPF_RET | BPF_K;
    vec![
        /* 0 */ stmt(load, OFFSET_ARCH),
        /* 1 */ jump(jeq, native_arch, 0, 3), // -> 2, else 5
        /* 2 */ stmt(load, OFFSET_NR),
        /* 3 */ jump(BPF_JMP | BPF_JGE | BPF_K, X32_SYSCALL_BIT, 9, 0), // -> 13, else 4
        /* 4 */ jump(jeq, native_ioctl, 3, 6), // -> 8, else 11
        /* 5 */ jump(jeq, compat_arch, 0, 8), // -> 6, else 14
        /* 6 */ stmt(load, OFFSET_NR),
        /* 7 */ jump(jeq, compat_ioctl, 0, 3), // -> 8, else 11
        /* 8 */ stmt(load, OFFSET_ARG1_LOW),
        /* 9 */ jump(jeq, libc::TIOCSTI as u32, 2, 0), // -> 12, else 10
        /* 10 */ jump(jeq, libc::TIOCLINUX as u32, 1, 0), // -> 12, else 11
        /* 11 */ stmt(ret, libc::SECCOMP_RET_ALLOW),
        /* 12 */ stmt(ret, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        /* 13 */ stmt(ret, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        /* 14 */ stmt(ret, libc::SECCOMP_RET_KILL_PROCESS),
    ]
}

/// Install the filter on the calling thread. `no_new_privs` must be set.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install() -> io::Result<()> {
    let filter = program();
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };
    // SAFETY: `prog` points at `filter`, which outlives the call; the kernel
    // copies the program.
    let rc = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn install() -> io::Result<()> {
    Err(io::Error::other(
        "the terminal-injection filter supports x86_64 and aarch64 only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::test_support::in_child;
    use std::os::fd::AsRawFd;

    fn ioctl_errno(fd: i32, request: u64) -> i32 {
        let mut byte = b'x';
        // SAFETY: raw ioctl with a pointer to a live byte; `request` is
        // passed in full so the high half reaches the filter.
        let rc = unsafe { libc::syscall(libc::SYS_ioctl, fd, request, &mut byte as *mut u8) };
        if rc == 0 {
            0
        } else {
            io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        }
    }

    #[test]
    fn terminal_injection_ioctls_are_refused_and_others_pass() {
        let null = std::fs::File::open("/dev/null").unwrap();
        let fd = null.as_raw_fd();
        // Positive control: without the filter /dev/null is simply no tty.
        assert_eq!(ioctl_errno(fd, libc::TIOCSTI), libc::ENOTTY);
        let code = in_child(|| {
            // SAFETY: plain prctl.
            if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
                return 90;
            }
            if install().is_err() {
                return 91;
            }
            if ioctl_errno(fd, libc::TIOCSTI) != libc::EPERM {
                return 1;
            }
            if ioctl_errno(fd, libc::TIOCLINUX) != libc::EPERM {
                return 2;
            }
            if ioctl_errno(fd, (1u64 << 32) | libc::TIOCSTI) != libc::EPERM {
                return 3;
            }
            if ioctl_errno(fd, libc::TIOCGWINSZ) != libc::ENOTTY {
                return 4;
            }
            0
        });
        assert_eq!(code, 0, "check {code} failed in the filtered child");
    }
}
