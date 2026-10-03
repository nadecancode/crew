//! The sandbox launch path end to end, through the real `crew` binary:
//! `crew sandbox-exec` (the shim) and, with bubblewrap, `crew sandbox-run`.
//!
//! Tests that need a host feature (Landlock, bwrap, user namespaces) are
//! skipped with a printed reason when it is missing, and fail instead under
//! `CREW_TEST_SANDBOX=require`.

use std::os::unix::process::CommandExt;
use std::process::{Command, Output};

const CREW: &str = env!("CARGO_BIN_EXE_crew");

fn host_has(available: bool, what: &str) -> bool {
    if available {
        return true;
    }
    if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
        panic!("CREW_TEST_SANDBOX=require but the host lacks {what}");
    }
    eprintln!("skipped: the host lacks {what}");
    false
}

fn landlock_scopes() -> bool {
    crew::sandbox::landlock::abi().is_ok_and(|abi| abi >= crew::sandbox::landlock::MIN_ABI)
}

fn shim(extra: &[&str], script: &str) -> Command {
    let mut cmd = Command::new(CREW);
    cmd.arg("sandbox-exec")
        .args(extra)
        .args(["--", "sh", "-c", script]);
    cmd
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_shim_runs_the_program_once_confined() {
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
        return;
    }
    let out = shim(&[], "echo confined").output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "confined\n");
}

#[test]
fn the_shim_fails_closed_before_the_program_runs() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("ran");
    let script = format!("echo ran > '{}'", sentinel.display());
    let out = shim(&["--min-landlock-abi", "999"], &script)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(
        !sentinel.exists(),
        "the program ran although Landlock failed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("crew sandbox-exec: "), "{stderr}");
    assert!(stderr.contains("Landlock"), "{stderr}");
}

#[test]
fn the_shim_closes_inherited_descriptors() {
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
        return;
    }
    let sentinel = tempfile::NamedTempFile::new().unwrap();
    let held = std::fs::File::open(sentinel.path()).unwrap();
    let with_fd_50 = |mut cmd: Command| {
        use std::os::fd::AsRawFd;
        let fd = held.as_raw_fd();
        // SAFETY: dup2 is async-signal-safe; the duplicate has no CLOEXEC,
        // so the program inherits it unless something closes it.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(fd, 50) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.output().unwrap()
    };
    let script = "test -e /proc/$$/fd/50 && echo inherited || echo closed";
    let mut direct = Command::new("sh");
    direct.args(["-c", script]);
    let control = with_fd_50(direct);
    assert_eq!(
        String::from_utf8_lossy(&control.stdout),
        "inherited\n",
        "{}",
        text(&control)
    );
    let out = with_fd_50(shim(&[], script));
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "closed\n");
}

#[test]
fn the_shim_keeps_the_program_from_signalling_outside() {
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
        return;
    }
    let script = format!("kill -0 {} && echo reached", std::process::id());
    let control = Command::new("sh").args(["-c", &script]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&control.stdout), "reached\n");
    let out = shim(&[], &script).output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}
