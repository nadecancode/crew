//! `crew sandbox-run SPEC -- PROGRAM ARGS`: the host side of a launch.
//!
//! The daemon cannot hand bubblewrap the pinned descriptors itself:
//! portable-pty closes every descriptor above 2 in the child it spawns. So
//! the daemon runs this fresh, single-threaded process in front of the
//! program instead. It checks the spec, marks every inherited descriptor
//! close-on-exec, pins each bind source and checks it is the object the
//! daemon validated, and starts bubblewrap with only those descriptors and
//! the allow-listed environment. Inside, bubblewrap runs
//! `crew sandbox-exec`, which confines the process and runs the program.
//!
//! It then stays as bubblewrap's parent, a child subreaper, until every
//! process of the sandbox is gone. bubblewrap reports the program's exit
//! before its own PID 1 has finished, and that PID 1 waits for the rest of
//! the namespace; as a subreaper, sandbox-run inherits it and waits for it
//! too. So when sandbox-run exits with a code, nothing of the sandbox is
//! left. It dies with the process that started it (`PR_SET_PDEATHSIG`),
//! and bubblewrap's `--die-with-parent` then takes the sandbox down.
//!
//! Any failure exits with [`EXIT_SETUP_FAILED`] before bubblewrap runs;
//! there is no fallback to binding by path.

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;

use super::pin::{pin, Kind};
use super::spec::{Mount, Spec, MAX_SPEC_LEN};
use super::EXIT_SETUP_FAILED;

#[derive(clap::Args, Clone, Debug)]
pub struct RunArgs {
    /// The sandbox, as JSON (paths and identities only)
    #[arg(value_name = "SPEC")]
    pub spec: String,
    /// The program and its arguments, after `--`
    #[arg(last = true, required = true, value_name = "PROGRAM")]
    pub command: Vec<OsString>,
}

/// The argument prefix that runs `program` in the sandbox `spec`
/// describes: `[crew_bin, "sandbox-run", SPEC, "--"]`. Start the process
/// with exactly [`super::spec::launcher_env`] as its environment. A spec
/// `Spec::check` refuses is refused here, before any host process sees it.
pub fn prefix(spec: &Spec) -> Result<Vec<OsString>, String> {
    spec.check()?;
    let json = serde_json::to_string(spec).map_err(|e| format!("encoding the sandbox: {e}"))?;
    if json.len() > MAX_SPEC_LEN {
        return Err(format!(
            "the sandbox description is {} bytes, more than {MAX_SPEC_LEN}",
            json.len()
        ));
    }
    Ok(vec![
        spec.crew_bin.clone().into(),
        "sandbox-run".into(),
        json.into(),
        "--".into(),
    ])
}

/// Set up bubblewrap, run it and wait until the whole sandbox is gone.
/// Exits with bubblewrap's exit code, or 128 plus the signal that killed
/// it. Never returns.
pub fn run(args: RunArgs) -> ! {
    let started = tie_to_parent().and_then(|()| prepare(&args.spec, &args.command));
    let (bwrap, pinned) = match started {
        Ok(started) => started,
        Err(error) => {
            eprintln!("crew sandbox-run: {error}");
            std::process::exit(EXIT_SETUP_FAILED)
        }
    };
    match supervise(bwrap, pinned) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("crew sandbox-run: {error}");
            std::process::exit(EXIT_SETUP_FAILED)
        }
    }
}

/// Die with the process that started this one, as bubblewrap did when it
/// was that process's child, and become a subreaper so that bubblewrap's
/// PID 1 comes back here when bubblewrap exits first.
fn tie_to_parent() -> Result<(), String> {
    // SAFETY: getppid has no failure modes.
    let parent = unsafe { libc::getppid() };
    // SAFETY: plain prctl calls with constant arguments.
    let rc = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
    if rc != 0 {
        return Err(format!(
            "tying this process to its parent: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: getppid has no failure modes.
    if unsafe { libc::getppid() } != parent {
        return Err("the process that started this one is already gone".into());
    }
    // SAFETY: plain prctl call with constant arguments.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(format!(
            "becoming a subreaper: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Start bubblewrap, drop this process's copies of the pinned descriptors,
/// and reap children until none is left. Returns bubblewrap's exit code,
/// or 128 plus the signal that killed it.
fn supervise(mut bwrap: Command, pinned: Vec<OwnedFd>) -> Result<i32, String> {
    let child = bwrap.spawn().map_err(|e| format!("running bwrap: {e}"))?;
    let bwrap_pid = child.id() as libc::pid_t;
    drop(pinned);
    // Dropping the handle neither waits nor kills; children are reaped below.
    drop(child);
    // Hold nothing but stdio while waiting: whatever this process inherited
    // was only marked close-on-exec, and it may wait for a long time.
    // SAFETY: nothing in this process still owns a descriptor from 3 up.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
        return Err(format!(
            "closing inherited descriptors: {}",
            io::Error::last_os_error()
        ));
    }
    let mut bwrap_status = None;
    loop {
        let mut status = 0;
        // SAFETY: waits for any child; `status` is a valid out-pointer.
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid > 0 {
            if pid == bwrap_pid {
                bwrap_status = Some(status);
            }
            continue;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => break,
            // Not knowing whether the sandbox is gone, never report a code.
            _ => std::process::abort(),
        }
    }
    match bwrap_status {
        Some(status) if libc::WIFEXITED(status) => Ok(libc::WEXITSTATUS(status)),
        Some(status) if libc::WIFSIGNALED(status) => Ok(128 + libc::WTERMSIG(status)),
        _ => std::process::abort(),
    }
}

/// The bubblewrap command for a spec, with the descriptors it refers to,
/// which must stay open until bubblewrap is started.
pub fn prepare(json: &str, command: &[OsString]) -> Result<(Command, Vec<OwnedFd>), String> {
    if json.len() > MAX_SPEC_LEN {
        return Err(format!(
            "the sandbox description is longer than {MAX_SPEC_LEN} bytes"
        ));
    }
    let spec: Spec =
        serde_json::from_str(json).map_err(|e| format!("reading the sandbox description: {e}"))?;
    spec.check()?;
    if command.is_empty() {
        return Err("no program to run".into());
    }
    mark_inherited_cloexec().map_err(|e| format!("closing inherited descriptors: {e}"))?;
    let (args, fds) = bwrap_args(&spec, command)?;
    let mut bwrap = Command::new(&spec.bwrap);
    bwrap
        .args(args)
        .env_clear()
        .envs(spec.sandbox_env(std::env::vars_os()));
    Ok((bwrap, fds))
}

/// bubblewrap's arguments. Each bind source is pinned here; the returned
/// descriptors are the only ones bubblewrap inherits.
pub fn bwrap_args(
    spec: &Spec,
    command: &[OsString],
) -> Result<(Vec<OsString>, Vec<OwnedFd>), String> {
    let mut args: Vec<OsString> = [
        "--unshare-user",
        "--disable-userns",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--die-with-parent",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if !spec.network {
        args.push("--unshare-net".into());
    }
    if spec.new_session {
        args.push("--new-session".into());
    }
    for arg in ["--proc", "/proc", "--dev", "/dev"] {
        args.push(arg.into());
    }
    let mut fds = Vec::new();
    for mount in &spec.mounts {
        match mount {
            Mount::Bind { path, id, writable } => {
                let fd = pin(path, *id)?;
                let option = if *writable || id.kind == Kind::Socket {
                    "--bind-fd"
                } else {
                    "--ro-bind-fd"
                };
                args.push(option.into());
                args.push(fd.as_raw_fd().to_string().into());
                args.push(path.into());
                fds.push(fd);
            }
            Mount::Tmpfs { path } => {
                args.push("--tmpfs".into());
                args.push(path.into());
            }
            Mount::Dir { path } => {
                for arg in ["--perms", "0700", "--dir"] {
                    args.push(arg.into());
                }
                args.push(path.into());
            }
            Mount::Symlink { path, target } => {
                args.push("--symlink".into());
                args.push(target.into());
                args.push(path.into());
            }
            Mount::Mask { path } => {
                let fd = empty_file().map_err(|e| format!("masking {}: {e}", path.display()))?;
                args.push("--ro-bind-data".into());
                args.push(fd.as_raw_fd().to_string().into());
                args.push(path.into());
                fds.push(fd);
            }
        }
    }
    args.push("--chdir".into());
    args.push(spec.cwd.clone().into());
    args.push("--".into());
    args.push(spec.crew_bin.clone().into());
    args.push("sandbox-exec".into());
    args.push("--min-landlock-abi".into());
    args.push(spec.min_landlock_abi.to_string().into());
    for dir in &spec.allow_unix {
        args.push("--allow-unix".into());
        args.push(dir.into());
    }
    args.push("--".into());
    args.extend(command.iter().cloned());
    Ok((args, fds))
}

/// Mark every descriptor from 3 up close-on-exec, so only the ones pinned
/// afterwards (opened without it) reach bubblewrap.
fn mark_inherited_cloexec() -> io::Result<()> {
    const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;
    // SAFETY: only changes descriptor flags.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// An empty, inheritable memory file for `--ro-bind-data`.
fn empty_file() -> io::Result<OwnedFd> {
    let name = CString::new("crew-mask").expect("no NUL");
    // SAFETY: `name` is a valid C string; the result is checked.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::pin::identify;
    use std::path::PathBuf;

    fn spec(mounts: Vec<Mount>) -> Spec {
        Spec {
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: "/opt/crew".into(),
            mounts,
            env: vec![("HOME".into(), "/home/u".into())],
            pass_env: vec!["SECRET_KEY".into()],
            cwd: "/work".into(),
            network: true,
            new_session: false,
            min_landlock_abi: 9,
            allow_unix: vec!["/run/crew".into()],
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn mounts_become_descriptor_binds_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        let rw = dir.path().join("rw");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::create_dir_all(&rw).unwrap();
        let spec = spec(vec![
            Mount::Tmpfs {
                path: "/tmp".into(),
            },
            Mount::Dir {
                path: "/tmp/run".into(),
            },
            Mount::Bind {
                path: ro.clone(),
                id: identify(&ro).unwrap(),
                writable: false,
            },
            Mount::Bind {
                path: rw.clone(),
                id: identify(&rw).unwrap(),
                writable: true,
            },
            Mount::Symlink {
                path: "/lib64".into(),
                target: "usr/lib".into(),
            },
            Mount::Mask {
                path: ro.join("sock"),
            },
        ]);
        let (args, fds) = bwrap_args(&spec, &["sh".into(), "-c".into(), "true".into()]).unwrap();
        let args = strings(&args);
        let fd = |i: usize| fds[i].as_raw_fd().to_string();
        let expected: Vec<String> = [
            "--unshare-user",
            "--disable-userns",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--die-with-parent",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--perms",
            "0700",
            "--dir",
            "/tmp/run",
            "--ro-bind-fd",
            &fd(0),
            ro.to_str().unwrap(),
            "--bind-fd",
            &fd(1),
            rw.to_str().unwrap(),
            "--symlink",
            "usr/lib",
            "/lib64",
            "--ro-bind-data",
            &fd(2),
            ro.join("sock").to_str().unwrap(),
            "--chdir",
            "/work",
            "--",
            "/opt/crew",
            "sandbox-exec",
            "--min-landlock-abi",
            "9",
            "--allow-unix",
            "/run/crew",
            "--",
            "sh",
            "-c",
            "true",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(args, expected);
        // The pinned directories are the validated ones.
        for (i, path) in [&ro, &rw].iter().enumerate() {
            let through = std::fs::read_link(format!("/proc/self/fd/{}", fd(i))).unwrap();
            assert_eq!(&through, *path);
        }
    }

    #[test]
    fn inherited_descriptors_are_marked_close_on_exec_and_pinned_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let code = crate::sandbox::test_support::in_child(|| {
            let file = std::fs::File::open(dir.path()).unwrap();
            // SAFETY: plain descriptor calls in the forked child.
            unsafe {
                if libc::dup2(file.as_raw_fd(), 50) < 0 {
                    return 90;
                }
                if mark_inherited_cloexec().is_err() {
                    return 91;
                }
                if libc::fcntl(50, libc::F_GETFD) & libc::FD_CLOEXEC == 0 {
                    return 1;
                }
            }
            let pinned = pin(dir.path(), identify(dir.path()).unwrap()).unwrap();
            // SAFETY: F_GETFD on an open descriptor.
            if unsafe { libc::fcntl(pinned.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0 {
                return 2;
            }
            0
        });
        assert_eq!(code, 0, "check {code} failed");
    }

    #[test]
    fn helpers_lose_the_network_and_the_terminal_session() {
        let mut spec = spec(Vec::new());
        spec.network = false;
        spec.new_session = true;
        let (args, _) = bwrap_args(&spec, &["git".into()]).unwrap();
        let args = strings(&args);
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.contains(&"--new-session".to_string()));
    }

    #[test]
    fn a_source_that_changed_since_validation_stops_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let id = identify(&src).unwrap();
        std::fs::rename(&src, dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("moved"), &src).unwrap();
        let spec = spec(vec![Mount::Bind {
            path: src.clone(),
            id,
            writable: false,
        }]);
        assert!(bwrap_args(&spec, &["true".into()]).is_err());
    }

    #[test]
    fn prepare_refuses_bad_input_before_anything_runs() {
        let good = serde_json::to_string(&spec(Vec::new())).unwrap();
        assert!(prepare(&good, &[]).is_err());
        assert!(prepare("{", &["true".into()]).is_err());
        assert!(prepare(&" ".repeat(MAX_SPEC_LEN + 1), &["true".into()]).is_err());
        let mut bad = spec(Vec::new());
        bad.cwd = PathBuf::from("relative");
        let bad = serde_json::to_string(&bad).unwrap();
        assert!(prepare(&bad, &["true".into()]).is_err());
    }

    #[test]
    fn the_prefix_refuses_a_spec_that_fails_its_check() {
        let mut bad = spec(Vec::new());
        bad.pass_env.push("LD_PRELOAD".into());
        assert!(prefix(&bad).is_err());
        let mut bad = spec(Vec::new());
        bad.cwd = PathBuf::from("relative");
        assert!(prefix(&bad).is_err());
    }

    #[test]
    fn the_prefix_carries_no_values_of_passed_variables() {
        let spec = spec(Vec::new());
        let prefix = prefix(&spec).unwrap();
        assert_eq!(prefix[0], OsString::from("/opt/crew"));
        assert_eq!(prefix[1], OsString::from("sandbox-run"));
        assert_eq!(prefix[3], OsString::from("--"));
        let json = prefix[2].to_str().unwrap();
        assert!(json.contains("SECRET_KEY") && !json.contains("sk-"));
        assert_eq!(serde_json::from_str::<Spec>(json).unwrap(), spec);
    }
}
