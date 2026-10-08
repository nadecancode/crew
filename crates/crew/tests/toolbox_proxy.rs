//! crew's toolbox MCP proxy, `crew toolbox mcp <name>`, against the stand-in
//! server `crew fake-mcp` (notes/toolboxes.md, Prompt cache rules, rule 5;
//! acceptance cases T14 and T20, and the redaction and scrubbing of T05).
//! The launch is read from a file here instead of the daemon.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crew::mcp::proxy::{definition_digest, pin, snapshot_path, PinOutcome};
use crew_core::rpc::{Redaction, ToolboxServerLaunch, Upstream};
use serde_json::{json, Value};

const CREW: &str = env!("CARGO_BIN_EXE_crew");

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The agent directory the daemon would pin snapshots in.
    fn agent_dir(&self) -> PathBuf {
        self.path("agent")
    }

    /// A launch of `crew fake-mcp` with `args`, written where the proxy reads
    /// it.
    fn launch(
        &self,
        name: &str,
        args: &[&str],
        env: &[(&str, &str)],
        redact: &[(&str, &str)],
    ) -> PathBuf {
        let mut command = vec![CREW.to_string(), "fake-mcp".to_string()];
        command.extend(args.iter().map(|a| a.to_string()));
        let launch = ToolboxServerLaunch {
            name: name.to_string(),
            upstream: Upstream::Stdio {
                command,
                cwd: None,
                env: env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<BTreeMap<_, _>>(),
            },
            redact: redact
                .iter()
                .map(|(name, value)| Redaction {
                    name: name.to_string(),
                    value: value.to_string(),
                })
                .collect(),
            scrub: redact.iter().map(|(name, _)| name.to_string()).collect(),
            definition: "a".repeat(64),
            snapshot: snapshot_path(&self.agent_dir(), name)
                .to_string_lossy()
                .into_owned(),
            refresh: false,
        };
        let path = self.path(&format!("{name}.launch.json"));
        std::fs::write(&path, serde_json::to_vec(&launch).unwrap()).unwrap();
        path
    }
}

/// Run a preflight; its exit status and what it printed.
fn preflight(launch: &Path, within: Option<&str>) -> (bool, Value) {
    let mut cmd = Command::new(CREW);
    cmd.args(["toolbox", "mcp", "x", "--preflight", "--launch-file"])
        .arg(launch)
        .env_remove("CREW_IDENTITY");
    if let Some(within) = within {
        cmd.args(["--preflight-within", within]);
    }
    let out = cmd.output().unwrap();
    let printed: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    (out.status.success(), printed)
}

/// Preflight and pin, as the daemon does before a launch.
fn preflight_and_pin(fx: &Fixture, launch: &Path, refresh: bool) -> PinOutcome {
    let (ok, printed) = preflight(launch, None);
    assert!(ok, "{printed}");
    pin(
        &fx.agent_dir(),
        "x",
        &"a".repeat(64),
        &printed["listing"],
        refresh,
    )
    .unwrap()
    .outcome
}

/// A session proxy, spoken to as the runtime would.
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<Value>,
    next: u64,
}

impl Session {
    fn start(launch: &Path, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(CREW);
        cmd.args(["toolbox", "mcp", "x", "--launch-file"])
            .arg(launch)
            .env_remove("CREW_IDENTITY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if let Ok(value) = serde_json::from_str(&line) {
                    if tx.send(value).is_err() {
                        return;
                    }
                }
            }
        });
        Session {
            child,
            stdin,
            lines,
            next: 0,
        }
    }

    fn notify(&mut self, method: &str) {
        let message = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{message}").unwrap();
    }

    /// Send a request and return its answer; anything else that arrives
    /// first is returned in the second element.
    fn request(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        self.next += 1;
        let id = self.next;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").unwrap();
        let mut other = Vec::new();
        loop {
            let message = self
                .lines
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            if message["id"] == json!(id) {
                return (message, other);
            }
            other.push(message);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn names(tools: &Value) -> Vec<String> {
    tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn log_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

// T14: the server lists its tools in another order every time and changes a
// description after its first listing. The runtime sees the pinned, sorted
// tools for the whole session, never `listChanged`; a refresh pins the new
// description and moves the definition digest.
#[test]
fn the_runtime_sees_the_pinned_sorted_tools_whatever_the_server_does() {
    let fx = Fixture::new();
    let state = fx.path("listings");
    let log = fx.path("server.log");
    let state_arg = state.to_string_lossy().into_owned();
    let log_arg = log.to_string_lossy().into_owned();
    let launch = fx.launch(
        "x",
        &[
            "--tools",
            "delta,alpha,charlie,bravo",
            "--shuffle",
            "--state",
            &state_arg,
            "--change-description-after",
            "1",
            "--list-changed",
            "--page",
            "3",
            "--log",
            &log_arg,
        ],
        &[],
        &[],
    );
    assert_eq!(preflight_and_pin(&fx, &launch, false), PinOutcome::Written);
    let pinned = definition_digest(&fx.agent_dir(), &["x".to_string()]).unwrap();
    assert_eq!(pinned.tools, 4);

    let _ = std::fs::remove_file(&log);
    let mut session = Session::start(&launch, &[]);
    let (init, _) = session.request("initialize", json!({"protocolVersion": "2025-03-26"}));
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    session.notify("notifications/initialized");
    for _ in 0..3 {
        let (listed, other) = session.request("tools/list", json!({}));
        assert_eq!(
            names(&listed["result"]["tools"]),
            ["alpha", "bravo", "charlie", "delta"]
        );
        assert!(listed["result"].get("nextCursor").is_none());
        let descriptions: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["description"].as_str().unwrap())
            .collect();
        assert!(
            descriptions.iter().all(|d| !d.contains("Changed")),
            "{descriptions:?}"
        );
        assert!(
            other
                .iter()
                .all(|m| m["method"] != "notifications/tools/list_changed"),
            "{other:?}"
        );
    }
    // A call goes through to the server and back.
    let (called, _) = session.request(
        "tools/call",
        json!({"name": "alpha", "arguments": {"n": 1}}),
    );
    assert_eq!(called["result"]["content"][0]["text"], r#"{"n":1}"#);
    drop(session);
    // The runtime's own initialize and tools/list never reached the server:
    // it saw the proxy's handshake, its one listing (two pages, the second
    // possibly after the call) and the call.
    let mut seen = log_lines(&log);
    assert_eq!(seen[..2], ["initialize", "notifications/initialized"]);
    seen.sort();
    assert_eq!(
        seen,
        [
            "initialize",
            "notifications/initialized",
            "tools/call",
            "tools/list",
            "tools/list"
        ]
    );

    // A later preflight sees the changed description: logged, not pinned.
    assert_eq!(
        preflight_and_pin(&fx, &launch, false),
        PinOutcome::KeptDiffers
    );
    assert_eq!(
        definition_digest(&fx.agent_dir(), &["x".to_string()]).unwrap(),
        pinned
    );
    // `crew resume --restart` refreshes it.
    assert_eq!(preflight_and_pin(&fx, &launch, true), PinOutcome::Written);
    let refreshed = definition_digest(&fx.agent_dir(), &["x".to_string()]).unwrap();
    assert_ne!(refreshed.sha256, pinned.sha256);
    let mut session = Session::start(&launch, &[]);
    session.request("initialize", json!({"protocolVersion": "2025-06-18"}));
    let (listed, _) = session.request("tools/list", json!({}));
    // The stand-in changes the first tool it was given: delta, last once sorted.
    assert_eq!(listed["result"]["tools"][3]["name"], "delta");
    assert!(listed["result"]["tools"][3]["description"]
        .as_str()
        .unwrap()
        .contains("Changed"));
}

#[test]
fn a_snapshot_taken_for_another_definition_is_replaced() {
    let fx = Fixture::new();
    let launch = fx.launch("x", &[], &[], &[]);
    let (ok, printed) = preflight(&launch, None);
    assert!(ok);
    let dir = fx.agent_dir();
    assert_eq!(
        pin(&dir, "x", &"a".repeat(64), &printed["listing"], false)
            .unwrap()
            .outcome,
        PinOutcome::Written
    );
    assert_eq!(
        pin(&dir, "x", &"a".repeat(64), &printed["listing"], false)
            .unwrap()
            .outcome,
        PinOutcome::Kept
    );
    assert_eq!(
        pin(&dir, "x", &"b".repeat(64), &printed["listing"], false)
            .unwrap()
            .outcome,
        PinOutcome::Written
    );
}

// T20: a server that exits during initialize, or does not answer tools/list
// in time, fails its preflight with the reason.
#[test]
fn a_preflight_fails_with_the_reason() {
    let fx = Fixture::new();
    let exits = fx.launch("x", &["--exit-during", "initialize"], &[], &[]);
    let (ok, printed) = preflight(&exits, None);
    assert!(!ok);
    assert_eq!(printed["ok"], false);
    assert!(
        printed["reason"].as_str().unwrap().contains("initialize"),
        "{printed}"
    );

    let hangs = fx.launch("x", &["--hang", "tools/list"], &[], &[]);
    let (ok, printed) = preflight(&hangs, Some("1"));
    assert!(!ok);
    assert!(
        printed["reason"]
            .as_str()
            .unwrap()
            .contains("did not answer tools/list in time"),
        "{printed}"
    );
}

// T20: a server that passed preflight and stops later: the session keeps its
// pinned view, and calls are refused with the reason.
#[test]
fn a_server_that_stops_in_session_leaves_the_pinned_view_in_place() {
    let fx = Fixture::new();
    let launch = fx.launch("x", &["--exit-after", "1.5"], &[], &[]);
    assert_eq!(preflight_and_pin(&fx, &launch, false), PinOutcome::Written);
    let mut session = Session::start(&launch, &[]);
    session.request("initialize", json!({"protocolVersion": "2025-06-18"}));
    std::thread::sleep(Duration::from_millis(2500));
    let (listed, _) = session.request("tools/list", json!({}));
    assert_eq!(
        names(&listed["result"]["tools"]),
        ["alpha", "beta", "gamma"]
    );
    let (called, _) = session.request("tools/call", json!({"name": "alpha", "arguments": {}}));
    assert!(
        called["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not running"),
        "{called}"
    );
}

// T05 (proxy part): the server gets its secret under its destination name
// only; the member's secret names are not passed on; a value the server
// echoes reaches the runtime redacted.
#[test]
fn secrets_reach_the_server_under_their_destination_name_and_come_back_redacted() {
    let fx = Fixture::new();
    let secret = "s3cr3t-0123456789";
    let digest: String = {
        use sha2::Digest;
        sha2::Sha256::digest(secret.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let expect = format!("GITHUB_TOKEN={digest}");
    let launch = fx.launch(
        "x",
        &[
            "--echo-token-in-error",
            "GITHUB_TOKEN",
            "--expect-sha256",
            &expect,
        ],
        &[("GITHUB_TOKEN", secret)],
        &[("SHOP_GITHUB_TOKEN", secret)],
    );
    assert_eq!(preflight_and_pin(&fx, &launch, false), PinOutcome::Written);
    // The proxy itself runs with the reference name set, as a session would
    // inherit it from an unscrubbed daemon; the server must not get it.
    let mut session = Session::start(&launch, &[("SHOP_GITHUB_TOKEN", secret)]);
    session.request("initialize", json!({"protocolVersion": "2025-06-18"}));
    let (env, _) = session.request("tools/call", json!({"name": "env", "arguments": {}}));
    let text = env["result"]["content"][0]["text"].as_str().unwrap();
    let seen: Value = serde_json::from_str(text).unwrap();
    let names: Vec<&str> = seen["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert!(names.contains(&"GITHUB_TOKEN"));
    assert!(!names.contains(&"SHOP_GITHUB_TOKEN"));
    assert_eq!(seen["matches"]["GITHUB_TOKEN"], true);
    let (echoed, _) = session.request("tools/call", json!({"name": "echo_token", "arguments": {}}));
    let message = echoed["error"]["message"].as_str().unwrap();
    assert_eq!(message, "bad token [redacted $SHOP_GITHUB_TOKEN]");
    assert!(!echoed.to_string().contains(secret));
}
