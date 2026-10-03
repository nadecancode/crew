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
            self.reader.read_line(&mut line).unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }

    fn hello_operator(&mut self) -> Value {
        self.call(
            "hello",
            json!({ "role": "operator", "client": "test", "protocol": PROTOCOL_VERSION }),
        )
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
    let again = as_agent.hello_operator();
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
    assert!(as_operator.hello_operator()["result"].is_object());
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

    // A clean stop takes it away; the next start writes a new one.
    s.stop();
    s.wait_for("the daemon to stop", || !socket.exists());
    assert!(!path.exists(), "the token outlived its daemon");
    s.ok(&["start", "-C", &ws]);
    let second = live_token(&socket);
    assert_ne!(second, first);

    // A daemon killed outright leaves its token; the next one replaces it.
    kill_hard(s.snapshot().session.daemon_pid);
    s.wait_for("the token to be left behind", || path.exists());
    s.ok(&["start", "-C", &ws]);
    let third = live_token(&socket);
    assert_ne!(third, second);
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
