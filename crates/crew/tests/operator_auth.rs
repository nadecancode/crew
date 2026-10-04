//! Who may speak to the daemon as whom: raw protocol frames on the daemon's
//! socket, with no CLI in between, as an agent's own code could send them.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use common::*;
use crew_core::rpc::PROTOCOL_VERSION;
use crew_core::AgentStatus;
use serde_json::{json, Value};

/// One connection to the daemon, spoken to in raw JSON lines.
struct Raw {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Raw {
    fn connect(s: &Session) -> Self {
        let info: Value = serde_json::from_str(
            &std::fs::read_to_string(s.workspace.join(".crew/daemon.json")).unwrap(),
        )
        .unwrap();
        let stream = UnixStream::connect(info["socket"].as_str().unwrap()).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next_id: 1,
        }
    }

    /// Send one call and return its reply frame (pushes are skipped).
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": method, "params": params });
        writeln!(self.writer, "{request}").unwrap();
        loop {
            let mut line = String::new();
            assert_ne!(
                self.reader.read_line(&mut line).unwrap(),
                0,
                "closed before reply"
            );
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }

    fn hello_operator(&mut self, token: Option<&str>) -> Value {
        let mut params =
            json!({ "role": "operator", "client": "test", "protocol": PROTOCOL_VERSION });
        if let Some(token) = token {
            params["token"] = json!(token);
        }
        self.call("hello", params)
    }

    fn hello_agent(&mut self, s: &Session, agent: &str) -> Value {
        let identity: Value =
            serde_json::from_str(&std::fs::read_to_string(s.identity_path(agent)).unwrap())
                .unwrap();
        self.call(
            "hello",
            json!({
                "role": "agent",
                "agent_id": agent,
                "token": identity["token"],
                "client": "test",
                "protocol": PROTOCOL_VERSION,
            }),
        )
    }
}

fn error_code(frame: &Value) -> &str {
    frame["error"]["code"].as_str().unwrap_or("")
}

#[test]
fn second_hello_is_refused_for_operator_and_agent_and_authority_is_unchanged() {
    let s = Session::new();
    let team = s.team(
        "hello",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("the agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });

    // An agent cannot become the operator on its own connection...
    let mut as_agent = Raw::connect(&s);
    assert!(as_agent.hello_agent(&s, "a")["result"].is_object());
    let operator_token = live_token(&socket_of(&s));
    let again = as_agent.hello_operator(Some(&operator_token));
    assert_eq!(error_code(&again), "forbidden", "{again}");
    let message = again["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("already authenticated as agent 'a'"),
        "{message}"
    );
    // ...and is still only that agent afterwards.
    let kill = as_agent.call("agent_kill", json!({ "id": "b" }));
    assert_eq!(error_code(&kill), "forbidden", "{kill}");
    assert!(as_agent.call("agents_list", json!(null))["result"].is_array());
    assert!(find_agent(&s.snapshot(), "b").pid.is_some());

    // The operator's connection cannot turn into an agent's either, and
    // keeps its operator authority.
    let mut as_operator = Raw::connect(&s);
    assert!(as_operator.hello_operator(Some(&operator_token))["result"].is_object());
    let again = as_operator.hello_agent(&s, "a");
    assert_eq!(error_code(&again), "forbidden", "{again}");
    let message = again["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("already authenticated as the operator"),
        "{message}"
    );
    let snapshot = as_operator.call("snapshot", json!({}));
    assert!(snapshot["result"]["agents"].is_array(), "{snapshot}");

    // A first hello that fails leaves the connection open to a valid one.
    let mut retry = Raw::connect(&s);
    let wrong = retry.call(
        "hello",
        json!({
            "role": "agent",
            "agent_id": "a",
            "token": "not-the-token",
            "client": "test",
            "protocol": PROTOCOL_VERSION,
        }),
    );
    assert_eq!(error_code(&wrong), "unauthorized", "{wrong}");
    assert!(retry.hello_agent(&s, "a")["result"].is_object());
}

// ---------------------------------------------------------------------------
// The operator token (design-b3.md §2.1): written by every daemon that wins
// the workspace lock, next to its socket, and never anywhere else.
// ---------------------------------------------------------------------------

/// The daemon's socket, from its registration.
fn socket_of(s: &Session) -> std::path::PathBuf {
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(s.workspace.join(".crew/daemon.json")).unwrap(),
    )
    .unwrap();
    info["socket"].as_str().unwrap().into()
}

fn live_token(socket: &std::path::Path) -> String {
    crew_core::paths::read_operator_token(socket)
        .unwrap()
        .expect("the daemon wrote its operator token")
}

fn start_empty_daemon() -> Session {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    s.ok(&["start", "-C", &s.workspace.to_string_lossy()]);
    s
}

#[test]
fn operator_with_token_succeeds() {
    let s = start_empty_daemon();
    let token = live_token(&socket_of(&s));
    let mut raw = Raw::connect(&s);
    let hello = raw.hello_operator(Some(&token));
    assert_eq!(hello["result"]["role"], "operator", "{hello}");
    assert!(raw.call("snapshot", json!({}))["result"].is_object());
}

#[test]
fn operator_without_token_is_refused() {
    let s = start_empty_daemon();
    let token = live_token(&socket_of(&s));
    let mut positive = Raw::connect(&s);
    assert!(positive.hello_operator(Some(&token))["result"].is_object());
    let mut raw = Raw::connect(&s);
    let reply = raw.hello_operator(None);
    assert_eq!(error_code(&reply), "unauthorized", "{reply}");
    assert_eq!(reply["error"]["message"], "operator token required");
    assert_eq!(error_code(&raw.call("snapshot", json!({}))), "unauthorized");
    let mut null = Raw::connect(&s);
    let reply = null.call(
        "hello",
        json!({"role":"operator", "token":null,
        "client":"test", "protocol":PROTOCOL_VERSION}),
    );
    assert_eq!(error_code(&reply), "unauthorized", "{reply}");
}

#[test]
fn wrong_token_is_refused() {
    let s = start_empty_daemon();
    let token = live_token(&socket_of(&s));
    let mut positive = Raw::connect(&s);
    assert!(positive.hello_operator(Some(&token))["result"].is_object());
    let same_length = format!(
        "{}{}",
        if token.starts_with('a') { "b" } else { "a" },
        &token[1..]
    );
    for wrong in ["", "guessed", &same_length, &format!("{token}x")] {
        let mut raw = Raw::connect(&s);
        let reply = raw.hello_operator(Some(wrong));
        assert_eq!(error_code(&reply), "unauthorized", "{reply}");
        assert_eq!(
            reply["error"]["message"],
            "operator token does not match this daemon (it may have restarted; reconnect)"
        );
        assert!(!reply.to_string().contains(&token));
        assert_eq!(error_code(&raw.call("snapshot", json!({}))), "unauthorized");
    }
}

#[test]
fn failed_first_hello_then_valid_retry() {
    let s = start_empty_daemon();
    let token = live_token(&socket_of(&s));
    let mut raw = Raw::connect(&s);
    for supplied in [None, Some("wrong")] {
        let reply = raw.hello_operator(supplied);
        assert_eq!(error_code(&reply), "unauthorized", "{reply}");
        assert_eq!(error_code(&raw.call("snapshot", json!({}))), "unauthorized");
    }
    assert!(raw.hello_operator(Some(&token))["result"].is_object());
    assert!(raw.call("snapshot", json!({}))["result"].is_object());
}

#[test]
fn agent_calls_succeed_on_the_same_socket() {
    let s = Session::new();
    s.run(&s.team("agents", vec![agent("a", "", &[])], vec![]));
    s.wait("the agent to come up", |snap| {
        find_agent(snap, "a").status != AgentStatus::Spawning
    });
    let token = live_token(&socket_of(&s));
    let mut operator = Raw::connect(&s);
    assert!(operator.hello_operator(Some(&token))["result"].is_object());
    let mut raw = Raw::connect(&s);
    assert!(raw.hello_agent(&s, "a")["result"].is_object());
    assert!(raw.call("agents_list", json!(null))["result"].is_array());
    assert_eq!(
        error_code(&raw.call("agent_kill", json!({"id":"a"}))),
        "forbidden"
    );
    let hello = raw.hello_operator(Some(&token));
    assert_eq!(error_code(&hello), "forbidden", "{hello}");
    assert!(raw.call("agents_list", json!(null))["result"].is_array());
}

#[test]
fn protocol_2_client_gets_mismatch() {
    let s = start_empty_daemon();
    let token = live_token(&socket_of(&s));
    let mut positive = Raw::connect(&s);
    assert!(positive.hello_operator(Some(&token))["result"].is_object());
    for supplied in [None, Some(token.as_str())] {
        let mut raw = Raw::connect(&s);
        let reply = raw.call(
            "hello",
            json!({"role":"operator", "token":supplied,
            "client":"v2", "protocol":2}),
        );
        assert_eq!(error_code(&reply), "protocol", "{reply}");
        assert_eq!(
            reply["error"]["message"],
            format!(
                "client speaks protocol 2, daemon speaks {}",
                crew_core::rpc::PROTOCOL_VERSION
            )
        );
        assert!(raw.hello_operator(Some(&token))["result"].is_object());
    }
}

fn kill_hard(pid: u32) {
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
}

#[test]
fn token_file_0600_and_rotates_on_restart() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&["start", "-C", &ws]);
    let socket = socket_of(&s);
    let path = crew_core::paths::operator_token_path(&socket);
    let meta = std::fs::symlink_metadata(&path).unwrap();
    assert!(meta.file_type().is_file());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.uid(), crew_core::paths::uid());
    let first = live_token(&socket);
    assert!(first.len() >= 32, "{} characters", first.len());
    assert!(Raw::connect(&s).hello_operator(Some(&first))["result"].is_object());

    // A clean stop takes it away; the next start writes a new one.
    s.stop();
    s.wait_for("the daemon to stop", || !socket.exists());
    assert!(!path.exists(), "the token outlived its daemon");
    s.ok(&["start", "-C", &ws]);
    let second = live_token(&socket);
    assert_ne!(second, first);
    assert!(Raw::connect(&s).hello_operator(Some(&second))["result"].is_object());
    let old = Raw::connect(&s).hello_operator(Some(&first));
    assert_eq!(error_code(&old), "unauthorized", "{old}");

    // A daemon killed outright leaves its token; the next one replaces it.
    kill_hard(s.snapshot().session.daemon_pid);
    s.wait_for("the token to be left behind", || path.exists());
    s.ok(&["start", "-C", &ws]);
    let third = live_token(&socket);
    assert_ne!(third, second);
    assert!(Raw::connect(&s).hello_operator(Some(&third))["result"].is_object());
    let old = Raw::connect(&s).hello_operator(Some(&second));
    assert_eq!(error_code(&old), "unauthorized", "{old}");
}

#[test]
fn losing_concurrent_start_keeps_the_live_token() {
    use std::os::unix::fs::MetadataExt;
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    s.ok(&["start", "-C", &ws]);
    let socket = socket_of(&s);
    let path = crew_core::paths::operator_token_path(&socket);
    let token = live_token(&socket);
    let inode = std::fs::metadata(&path).unwrap().ino();

    // A second daemon for the same workspace loses the lock and touches
    // nothing: not the token, not the socket.
    let second = s.crew(&["daemon", "--workspace", &ws]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("already running"),
        "{}",
        stderr(&second)
    );
    assert_eq!(live_token(&socket), token);
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    assert!(UnixStream::connect(&socket).is_ok());
}

#[test]
fn startup_failure_removes_only_its_own_token() {
    let s = Session::new();
    // The database cannot be opened: the start fails after it has written
    // its token and bound its socket.
    std::fs::create_dir_all(s.workspace.join(".crew/crew.db")).unwrap();
    let runtime = s.root().join("run/crew");
    std::fs::create_dir_all(&runtime).unwrap();
    let unrelated = runtime.join("unrelated.token");
    std::fs::write(&unrelated, "someone else's").unwrap();

    let ws = s.workspace.to_string_lossy().into_owned();
    let failed = s.crew(&["daemon", "--workspace", &ws]);
    assert!(!failed.status.success(), "{}", stderr(&failed));
    let left: Vec<String> = std::fs::read_dir(&runtime)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["unrelated.token"], "{}", stderr(&failed));
    assert_eq!(
        std::fs::read_to_string(&unrelated).unwrap(),
        "someone else's"
    );
    assert!(!s.workspace.join(".crew/daemon.json").exists());
}

#[test]
fn token_never_leaks() {
    let s = Session::new();
    let env_dump = s.root().join("agent-env.txt");
    let mut team = s.team("secret", vec![agent("a", "", &[])], vec![]);
    team.agents[0].command = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "env > '{}'; exec {{crew}} scripted-agent",
            env_dump.display()
        ),
    ];
    s.run(&team);
    s.wait("the agent to come up", |snap| {
        find_agent(snap, "a").status != AgentStatus::Spawning
    });
    s.wait_for("the agent's environment", || env_dump.exists());
    let token = live_token(&socket_of(&s));
    let ws = s.workspace.to_string_lossy().into_owned();

    let mut places: Vec<(String, Vec<u8>)> = vec![
        (
            "status".into(),
            s.ok(&["status", "--json", "-C", &ws]).into_bytes(),
        ),
        (
            "events".into(),
            s.ok(&["logs", "--json", "-n", "2000", "-C", &ws])
                .into_bytes(),
        ),
        (
            "messages".into(),
            s.ok(&["messages", "--json", "-C", &ws]).into_bytes(),
        ),
        (
            "agent environment".into(),
            std::fs::read(&env_dump).unwrap(),
        ),
    ];
    // Everything the daemon keeps in its state directory: the database
    // (and its write-ahead log), identities, settings, its own log.
    let mut dirs = vec![s.workspace.join(".crew")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !path.ends_with("worktrees") {
                    dirs.push(path);
                }
            } else if path.is_file() {
                places.push((path.display().to_string(), std::fs::read(&path).unwrap()));
            }
        }
    }
    for (place, bytes) in &places {
        assert!(
            !String::from_utf8_lossy(bytes).contains(&token),
            "the operator token is in {place}"
        );
    }
}

#[test]
fn a_start_that_fails_to_register_leaves_nothing_behind() {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    // The session registry cannot be created: the start fails after it has
    // written its token, bound its socket and written daemon.json.
    let sessions = s.root().join("state/crew/sessions");
    std::fs::create_dir_all(sessions.parent().unwrap()).unwrap();
    std::fs::write(&sessions, "not a directory").unwrap();

    let ws = s.workspace.to_string_lossy().into_owned();
    let failed = s.crew(&["daemon", "--workspace", &ws]);
    assert!(!failed.status.success(), "{}", stderr(&failed));
    assert!(
        !s.workspace.join(".crew/daemon.json").exists(),
        "a half-written registration was left: {}",
        stderr(&failed)
    );
    let runtime = s.root().join("run/crew");
    let left: Vec<String> = std::fs::read_dir(&runtime)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_command_that_starts_a_daemon_is_refused_inside_an_agent_before_it_starts_one() {
    let s = Session::new();
    std::fs::create_dir_all(&s.workspace).unwrap();
    let team = s.team("refused", vec![agent("a", "w", &[])], vec![]);
    let team_file = s.root().join("team.yaml");
    std::fs::write(&team_file, team.to_yaml().unwrap()).unwrap();
    let ws = s.workspace.to_string_lossy().into_owned();
    let team_file = team_file.to_string_lossy().into_owned();
    for args in [
        vec!["-C", &ws, "start"],
        vec!["-C", &ws, "run", &team_file, "--no-attach"],
    ] {
        let refused = s
            .command()
            .env("CREW_IDENTITY", s.root().join("no-such-identity.json"))
            .args(&args)
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{args:?} succeeded");
        assert!(
            stderr(&refused).contains("CREW_IDENTITY is set"),
            "{args:?}: {}",
            stderr(&refused)
        );
        assert!(
            !s.workspace.join(".crew").exists(),
            "{args:?} left workspace state behind"
        );
        assert!(
            !s.root().join("run/crew").exists(),
            "{args:?} left a socket directory behind"
        );
    }
}

#[test]
fn a_start_that_loses_the_race_waits_for_the_winner_and_connects() {
    use std::os::fd::AsRawFd;
    let s = Session::new();
    let state = s.workspace.join(".crew");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // The winner of the race holds the workspace lock while it starts up.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("daemon.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor this test owns.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let ws = s.workspace.to_string_lossy().into_owned();
    let loser = s
        .command()
        .args(["start", "-C", &ws])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // The daemon the loser spawned finds the lock taken and exits; the
    // winner is not up yet.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    drop(lock);
    let mut winner = s
        .command()
        .args(["daemon", "--workspace", &ws])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let out = loser.wait_with_output().unwrap();
    let (said, err) = (stdout(&out), stderr(&out));
    winner.kill().unwrap();
    let _ = winner.wait();
    assert!(out.status.success(), "the loser failed: {err}");
    assert!(said.contains("daemon running (pid"), "{said}");
}
