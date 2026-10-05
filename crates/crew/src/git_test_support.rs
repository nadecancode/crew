//! Test subprocesses must not borrow another repository or agent's authority.

use std::path::Path;
use std::process::{Command, Stdio};

pub fn clear_repository_environment(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy();
        if key.starts_with("GIT_")
            || key.starts_with("HERDR_")
            || matches!(
                key.as_ref(),
                "CREW_IDENTITY" | "CREW_SOCKET" | "CREW_LAUNCH"
            )
        {
            command.env_remove(name);
        }
    }
}

pub fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    clear_repository_environment(&mut command);
    command.arg("-C").arg(dir).args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
    ]);
    command.stdin(Stdio::null());
    command
}

/// Some fixtures also call production code that intentionally reads Git's
/// environment. Isolate the whole test in a child instead of changing that
/// code or mutating the environment of the parallel test process.
pub fn isolate_git_fixture(name: &str) -> bool {
    if !std::env::vars_os().any(|(name, _)| name.to_string_lossy().starts_with("GIT_")) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    clear_repository_environment(&mut command);
    let output = command
        .args(["--exact", name, "--nocapture"])
        .output()
        .expect("running isolated Git fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            && stdout.lines().any(|line| line == "running 1 test")
            && stdout.contains("1 passed; 0 failed"),
        "isolated {name} did not run and pass exactly once:\n{stdout}\n{stderr}"
    );
    true
}
