//! Landlock through raw syscalls. The `libc` crate has the syscall numbers
//! but not the structures, so the few that are needed are declared here
//! (from `include/uapi/linux/landlock.h`).

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Abstract unix sockets created outside the domain cannot be reached.
pub const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
/// Processes outside the domain cannot be signalled.
pub const SCOPE_SIGNAL: u64 = 1 << 1;
/// Connecting to a pathname unix socket created outside the domain.
pub const ACCESS_FS_RESOLVE_UNIX: u64 = 1 << 16;

/// ABI 6 (Linux 6.12) added the two scopes the sandbox relies on.
pub const MIN_ABI: u32 = 6;
/// ABI 9 (Linux 7.1) added `ACCESS_FS_RESOLVE_UNIX`.
pub const RESOLVE_UNIX_ABI: u32 = 9;

const CREATE_RULESET_VERSION: u32 = 1 << 0;
const RULE_PATH_BENEATH: libc::c_int = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// The Landlock ABI the running kernel offers. An error means Landlock is
/// unavailable (not built in, or not in the `lsm=` list).
pub fn abi() -> io::Result<u32> {
    // SAFETY: with a null attribute and the VERSION flag the kernel only
    // returns the ABI number; no memory is read or written.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as u32)
}

/// Put the calling thread, and everything it executes afterwards, in a new
/// Landlock domain:
/// - abstract unix sockets and signals are scoped to the domain;
/// - from ABI 9, pathname unix sockets created outside the domain can be
///   reached only beneath the directories in `unix_dirs`.
///
/// Refuses (and changes nothing) when the kernel's ABI is below `min_abi`.
/// Returns the ABI that was used. `no_new_privs` must already be set.
pub fn restrict_self(min_abi: u32, unix_dirs: &[&Path]) -> Result<u32, String> {
    let abi = abi().map_err(|e| format!("Landlock is not available: {e}"))?;
    if abi < min_abi.max(MIN_ABI) {
        return Err(format!(
            "Landlock ABI {abi} is older than the required {} (Linux 6.12 or newer, \
with landlock in the lsm= list)",
            min_abi.max(MIN_ABI)
        ));
    }
    let resolve_unix = abi >= RESOLVE_UNIX_ABI;
    let attr = RulesetAttr {
        handled_access_fs: if resolve_unix {
            ACCESS_FS_RESOLVE_UNIX
        } else {
            0
        },
        handled_access_net: 0,
        scoped: SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL,
    };
    // SAFETY: `attr` is a valid ruleset attribute of the size passed.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if rc < 0 {
        return Err(format!(
            "creating the Landlock ruleset: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: the syscall returned a new descriptor that nothing else owns.
    let ruleset = unsafe { OwnedFd::from_raw_fd(rc as i32) };
    if resolve_unix {
        for dir in unix_dirs {
            allow_unix_beneath(&ruleset, dir)?;
        }
    }
    // SAFETY: plain syscall on a descriptor we own.
    let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) };
    if rc < 0 {
        return Err(format!(
            "entering the Landlock domain: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(abi)
}

fn allow_unix_beneath(ruleset: &OwnedFd, dir: &Path) -> Result<(), String> {
    let c_dir = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", dir.display()))?;
    // SAFETY: `c_dir` is a valid C string; the result is checked.
    let fd = unsafe {
        libc::open(
            c_dir.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "opening {} for the Landlock socket rule: {}",
            dir.display(),
            io::Error::last_os_error()
        ));
    }
    // SAFETY: `open` returned a new descriptor that nothing else owns.
    let dir_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let rule = PathBeneathAttr {
        allowed_access: ACCESS_FS_RESOLVE_UNIX,
        parent_fd: dir_fd.as_raw_fd(),
    };
    // SAFETY: `rule` is a valid path-beneath attribute; both descriptors are open.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0u32,
        )
    };
    if rc < 0 {
        return Err(format!(
            "allowing sockets beneath {}: {}",
            dir.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::test_support::{host_has, in_child};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

    fn abi_at_least(min: u32) -> bool {
        abi().is_ok_and(|abi| abi >= min)
    }

    fn set_no_new_privs() -> bool {
        // SAFETY: plain prctl.
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0 }
    }

    fn errno_of(result: std::io::Result<UnixStream>) -> i32 {
        match result {
            Ok(_) => 0,
            Err(e) => e.raw_os_error().unwrap_or(-1),
        }
    }

    fn signal_errno(pid: i32) -> i32 {
        // SAFETY: signal 0 only checks permission and existence.
        if unsafe { libc::kill(pid, 0) } == 0 {
            0
        } else {
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        }
    }

    #[test]
    fn the_domain_cannot_reach_outside_abstract_sockets_or_signal_outside() {
        if !host_has(abi_at_least(MIN_ABI), "Landlock ABI 6") {
            return;
        }
        let name = format!("crew-landlock-test-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _listener = UnixListener::bind_addr(&addr).unwrap();
        let parent = std::process::id() as i32;
        // Positive controls, outside any domain.
        assert_eq!(errno_of(UnixStream::connect_addr(&addr)), 0);
        assert_eq!(signal_errno(parent), 0);
        let code = in_child(|| {
            if !set_no_new_privs() || restrict_self(MIN_ABI, &[]).is_err() {
                return 90;
            }
            if errno_of(UnixStream::connect_addr(&addr)) != libc::EPERM {
                return 1;
            }
            if signal_errno(parent) != libc::EPERM {
                return 2;
            }
            0
        });
        assert_eq!(code, 0, "check {code} failed inside the domain");
    }

    #[test]
    fn from_abi_9_outside_sockets_are_reached_only_beneath_allowed_directories() {
        if !host_has(abi_at_least(RESOLVE_UNIX_ABI), "Landlock ABI 9") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("allowed");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let _a = UnixListener::bind(allowed.join("s")).unwrap();
        let _b = UnixListener::bind(other.join("s")).unwrap();
        assert_eq!(errno_of(UnixStream::connect(other.join("s"))), 0);
        let code = in_child(|| {
            if !set_no_new_privs() || restrict_self(MIN_ABI, &[allowed.as_path()]).is_err() {
                return 90;
            }
            if errno_of(UnixStream::connect(allowed.join("s"))) != 0 {
                return 1;
            }
            if errno_of(UnixStream::connect(other.join("s"))) != libc::EACCES {
                return 2;
            }
            0
        });
        assert_eq!(code, 0, "check {code} failed inside the domain");
    }

    #[test]
    fn an_abi_below_the_minimum_is_refused_and_nothing_is_confined() {
        if !host_has(abi().is_ok(), "Landlock") {
            return;
        }
        let parent = std::process::id() as i32;
        let code = in_child(|| {
            if !set_no_new_privs() {
                return 90;
            }
            match restrict_self(999, &[]) {
                Err(e) if e.contains("older than the required 999") => {}
                _ => return 1,
            }
            if signal_errno(parent) != 0 {
                return 2;
            }
            0
        });
        assert_eq!(code, 0, "check {code} failed");
    }
}
