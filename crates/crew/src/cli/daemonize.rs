//! Starting the daemon: in the foreground (`crew daemon`) or detached from
//! the invoking terminal so it outlives the dashboard and the shell.

use std::fs::OpenOptions;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crew_core::paths::{self, DaemonInfo};

use crate::daemon::{self, DaemonOptions};

/// Environment variables that mark the *invoking* session (a Claude Code or
/// multiplexer session crew was started from). Agents must not inherit them,
/// or they would believe they are that session.
const SESSION_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CREW_IDENTITY",
    "CREW_AGENT_ID",
];

pub fn run_foreground(workspace: &Path) -> Result<()> {
    let log_to_stderr = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .try_init();
    let _ = log_to_stderr;
    let options = DaemonOptions {
        workspace: workspace.to_path_buf(),
        crew_bin: std::env::current_exe().context("locating the crew binary")?,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async move {
        let running = daemon::start(options).await?;
        running.serve().await
    })
}

fn daemon_reachable(info: &DaemonInfo) -> bool {
    paths::pid_alive(info.pid) && UnixStream::connect(&info.socket).is_ok()
}

/// The daemon for `workspace`, starting one if none is running.
pub fn ensure_daemon(workspace: &Path) -> Result<DaemonInfo> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolving {}", workspace.display()))?;
    let state_dir = paths::state_dir(&workspace);
    if let Some(info) = paths::read_daemon_info(&state_dir).filter(daemon_reachable) {
        return Ok(info);
    }
    paths::ensure_private_dir(&state_dir)?;
    let log_path = state_dir.join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let exe = std::env::current_exe().context("locating the crew binary")?;

    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("--workspace")
        .arg(&workspace)
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    for marker in SESSION_MARKERS {
        cmd.env_remove(marker);
    }
    // A session of its own: the daemon must survive the terminal (and the
    // dashboard) it was started from.
    // SAFETY: setsid is async-signal-safe and runs in the child before exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting the crew daemon")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(info) = paths::read_daemon_info(&state_dir).filter(daemon_reachable) {
            return Ok(info);
        }
        if let Some(status) = child.try_wait()? {
            let tail = std::fs::read_to_string(&log_path)
                .map(|s| {
                    let lines: Vec<&str> = s.lines().collect();
                    lines[lines.len().saturating_sub(8)..].join("\n")
                })
                .unwrap_or_default();
            bail!("the crew daemon exited during startup ({status}):\n{tail}");
        }
        if Instant::now() > deadline {
            bail!(
                "the crew daemon did not come up within 15s; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}
