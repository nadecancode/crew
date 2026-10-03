//! `crew sandbox-exec`: the last step inside the sandbox before the agent's
//! program. Each step must succeed or the program does not run:
//! 1. close every inherited descriptor except stdio (and with it the PTY);
//! 2. set `no_new_privs`;
//! 3. install the terminal-injection seccomp filter;
//! 4. enter a Landlock domain that scopes abstract unix sockets and signals
//!    (agents keep the host network, so the network namespace does not
//!    separate abstract sockets) and pathname sockets;
//! 5. exec the program.
//!
//! The process must be single-threaded: Landlock and seccomp apply to the
//! calling thread only. Nothing here starts a thread, and the shim checks.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use super::{landlock, seccomp, EXIT_SETUP_FAILED};

#[derive(clap::Args, Clone, Debug)]
pub struct ExecArgs {
    /// Oldest Landlock ABI to accept
    #[arg(long, value_name = "N", default_value_t = landlock::MIN_ABI)]
    pub min_landlock_abi: u32,
    /// Directory beneath which pathname sockets made outside the sandbox
    /// may be reached
    #[arg(long = "allow-unix", value_name = "DIR")]
    pub allow_unix: Vec<PathBuf>,
    /// The program and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "PROGRAM")]
    pub command: Vec<OsString>,
}

/// Confine this process and exec the program. Never returns: on any failure
/// it reports the step on stderr and exits with [`EXIT_SETUP_FAILED`].
pub fn run(args: ExecArgs) -> ! {
    let error = confine_and_exec(&args);
    eprintln!("crew sandbox-exec: {error}");
    std::process::exit(EXIT_SETUP_FAILED)
}

/// Returns only on failure, with the reason.
fn confine_and_exec(args: &ExecArgs) -> String {
    let Some((program, rest)) = args.command.split_first() else {
        return "no program to run".into();
    };
    match thread_count() {
        Ok(1) => {}
        Ok(n) => return format!("refusing to confine a process with {n} threads"),
        Err(e) => return format!("counting threads: {e}"),
    }
    if let Err(e) = close_inherited_descriptors() {
        return format!("closing inherited descriptors: {e}");
    }
    if let Err(e) = set_no_new_privs() {
        return format!("setting no_new_privs: {e}");
    }
    if let Err(e) = seccomp::install() {
        return format!("installing the seccomp filter: {e}");
    }
    let unix_dirs: Vec<&Path> = args.allow_unix.iter().map(PathBuf::as_path).collect();
    if let Err(e) = landlock::restrict_self(args.min_landlock_abi, &unix_dirs) {
        return e;
    }
    let error = std::process::Command::new(program).args(rest).exec();
    format!("running {}: {error}", Path::new(program).display())
}

fn thread_count() -> io::Result<usize> {
    Ok(std::fs::read_dir("/proc/self/task")?.count())
}

fn close_inherited_descriptors() -> io::Result<()> {
    // SAFETY: closes descriptors 3 and up; nothing in this process holds one
    // it still needs (Rust's stdio is 0-2).
    let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_no_new_privs() -> io::Result<()> {
    // SAFETY: plain prctl with constant arguments.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
