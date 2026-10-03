//! Agent sandboxing (B3): every process that runs for an agent runs inside
//! bubblewrap, behind a small shim.
//!
//! Inside the sandbox, `crew sandbox-exec` ([`shim`]) closes inherited
//! descriptors, sets `no_new_privs`, installs a seccomp filter against
//! terminal input injection and a Landlock domain, and only then execs the
//! agent's program. Any failure in a crew stage exits with
//! [`EXIT_SETUP_FAILED`] before the program runs; nothing falls back to
//! running unsandboxed.

pub mod landlock;
pub mod pin;
pub mod probe;
pub mod profile;
pub mod run;
pub mod scan;
pub mod seccomp;
pub mod shim;
pub mod spec;
pub mod validate;

/// Exit status of the crew sandbox stages when the sandbox could not be set
/// up. The requested program never ran.
pub const EXIT_SETUP_FAILED: i32 = 125;

#[cfg(test)]
pub(crate) mod test_support {
    /// Run `body` in a forked child and return its exit status. Confinement
    /// is per process and irreversible, so it is tried in a child. The child
    /// only makes syscalls (glibc keeps malloc usable after fork) and must
    /// not touch the environment: another thread may hold Rust's env lock.
    pub fn in_child(body: impl FnOnce() -> i32) -> i32 {
        // SAFETY: the child runs `body` and `_exit`s without unwinding.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(99);
            // SAFETY: leaves the child without running destructors.
            unsafe { libc::_exit(code) };
        }
        let mut status = 0;
        // SAFETY: waits for the child just forked.
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(libc::WIFEXITED(status), "child died: {status:#x}");
        libc::WEXITSTATUS(status)
    }

    /// Whether a test that needs `what` from the host can run. With
    /// `CREW_TEST_SANDBOX=require` a missing feature fails the test;
    /// otherwise the test is skipped with the reason printed.
    pub fn host_has(available: bool, what: &str) -> bool {
        if available {
            return true;
        }
        if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
            panic!("CREW_TEST_SANDBOX=require but the host lacks {what}");
        }
        eprintln!("skipped: the host lacks {what}");
        false
    }
}
