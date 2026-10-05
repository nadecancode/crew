//! The stand-in agent CLIs (`crew fake-cli`) keep their files where the real
//! ones do, so tests of sandboxed launches have something to look at.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREW: &str = env!("CARGO_BIN_EXE_crew");

fn fake(kind: &str, args: &[&str], envs: &[(&str, &std::path::Path)]) -> std::process::Child {
    let mut cmd = Command::new(CREW);
    cmd.args(["fake-cli", kind])
        .args(args)
        .env_remove("CREW_IDENTITY")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for (name, value) in envs {
        cmd.env(name, value);
    }
    cmd.spawn().unwrap()
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_fake_claude_keeps_its_files_in_its_configuration_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = fake(
        "claude",
        &["--session-id", "abc-1"],
        &[("CLAUDE_CONFIG_DIR", dir.path())],
    );
    let transcript = dir.path().join("projects/-fake/abc-1.jsonl");
    wait_for("the transcript", || transcript.is_file());
    let seen: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("fake-claude.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(seen["args"][0], "--session-id");
    assert_eq!(
        seen["env"]["CLAUDE_CONFIG_DIR"],
        dir.path().to_str().unwrap()
    );
    writeln!(child.stdin.as_mut().unwrap(), "ping").unwrap();
    wait_for("the reply in the transcript", || {
        std::fs::read_to_string(&transcript).is_ok_and(|t| t.contains("echo: ping"))
    });
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn the_fake_codex_server_binds_its_socket_and_keeps_its_rollout_in_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let listen = format!("unix://{}", socket.display());
    let codex_home = dir.path().join("rw/home/codex");
    let started = chrono::Local::now().timestamp_millis();
    let files = crew::agents::files::AgentFiles::private(
        dir.path().to_path_buf(),
        std::fs::File::open(dir.path()).unwrap().into(),
    );
    let mut child = fake(
        "codex",
        &["app-server", "--listen", &listen],
        &[("CODEX_HOME", &codex_home)],
    );
    wait_for("the socket", || UnixStream::connect(&socket).is_ok());
    // Use the daemon's discovery instead of an expected date literal: a
    // rollout outside the launch-date window is invisible to compaction.
    wait_for("the rollout", || {
        files.codex_rollout("fake-thread", started).is_some()
    });
    let rollout = files.codex_rollout("fake-thread", started).unwrap();
    assert!(rollout.path.starts_with(&codex_home));
    let row: serde_json::Value = serde_json::from_reader(rollout.file).unwrap();
    let timestamp = chrono::DateTime::parse_from_rfc3339(row["timestamp"].as_str().unwrap())
        .unwrap()
        .timestamp_millis();
    assert!((started..=chrono::Local::now().timestamp_millis()).contains(&timestamp));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn the_fake_pi_writes_its_session_where_it_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let mut child = fake(
        "pi",
        &["--session-dir", sessions.to_str().unwrap()],
        &[("PI_CODING_AGENT_DIR", &dir.path().join("pi"))],
    );
    wait_for("the session file", || {
        std::fs::read_dir(&sessions).is_ok_and(|mut d| d.next().is_some())
    });
    assert!(dir.path().join("pi/fake-pi.json").is_file());
    child.kill().unwrap();
    child.wait().unwrap();
}
