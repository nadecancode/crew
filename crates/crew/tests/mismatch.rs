//! A daemon of another build of crew is said, never suffered
//! (notes/binary-pinning.md B8): the commands an agent and the operator run
//! against a daemon of another protocol, here a stand-in in a process of its
//! own that refuses every `hello` as daemons before this change did.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use crew_core::rpc::PROTOCOL_VERSION;
use serde_json::{json, Value};

const STAND_IN: &str = "CREW_MISMATCH_STAND_IN";

/// The daemon of another protocol, when run with `STAND_IN` set to its
/// socket; otherwise nothing.
#[test]
fn stand_in_daemon() {
    let Some(socket) = std::env::var_os(STAND_IN) else {
        return;
    };
    let listener = UnixListener::bind(&socket).unwrap();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap_or(0)
            == 0
        {
            continue;
        }
        let hello: Value = serde_json::from_str(&line).unwrap();
        let refusal = json!({
            "id": hello["id"],
            "error": {
                "code": "protocol",
                "message": format!(
                    "client speaks protocol {}, daemon speaks {}",
                    hello["params"]["protocol"],
                    PROTOCOL_VERSION - 1
                ),
            },
        });
        let _ = writeln!(stream, "{refusal}");
    }
}

fn crew(args: &[&str], home: &Path, identity: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_crew"));
    command
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("CREW_SOCKET")
        .env_remove("CREW_IDENTITY");
    if let Some(identity) = identity {
        command.env("CREW_IDENTITY", identity);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn every_command_names_both_sides_and_crew_stop_still_stops_the_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let socket = home.join("crew.sock");
    let mut daemon = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "stand_in_daemon", "--nocapture"])
        .env(STAND_IN, &socket)
        .spawn()
        .unwrap();
    let pid = daemon.id();
    // Reaped as soon as it ends, so `crew stop` does not wait on a zombie.
    let reaper = std::thread::spawn(move || daemon.wait().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "the stand-in did not listen");
        std::thread::sleep(Duration::from_millis(10));
    }
    let sentence = format!(
        "crew {} (protocol {PROTOCOL_VERSION}) cannot talk to the daemon at {}, which speaks \
protocol {}: restart the team (`crew stop`, then `crew run`)",
        env!("CARGO_PKG_VERSION"),
        socket.display(),
        PROTOCOL_VERSION - 1
    );

    let identity = home.join("identity.json");
    let write_identity = |socket: &Path| {
        let body = json!({ "socket": socket, "agent_id": "worker", "token": "t" });
        std::fs::write(&identity, body.to_string()).unwrap();
    };
    write_identity(&socket);

    // A hook says it, with an exit code that does not block the CLI
    // (Claude Code blocks on 2), and nothing on stdout.
    let hook = crew(
        &[
            "hook",
            "stop",
            "--identity",
            identity.to_str().unwrap(),
            "{}",
        ],
        home,
        None,
    );
    assert_eq!(hook.status.code(), Some(1), "{}", text(&hook.stderr));
    assert_eq!(text(&hook.stdout), "");
    assert!(
        text(&hook.stderr).contains(&sentence),
        "{}",
        text(&hook.stderr)
    );

    // `crew ctl` says it.
    let ctl = crew(&["ctl", "tasks"], home, Some(&identity));
    assert!(!ctl.status.success());
    assert!(
        text(&ctl.stderr).contains(&sentence),
        "{}",
        text(&ctl.stderr)
    );

    // A hook with no daemon to reach still stays silent.
    write_identity(&home.join("gone.sock"));
    let quiet = crew(
        &[
            "hook",
            "stop",
            "--identity",
            identity.to_str().unwrap(),
            "{}",
        ],
        home,
        None,
    );
    assert_eq!(quiet.status.code(), Some(0));
    assert_eq!(
        (text(&quiet.stdout), text(&quiet.stderr)),
        (String::new(), String::new())
    );

    // The operator: `crew stop` cannot ask a daemon of another protocol, so
    // it stops the process that holds the socket.
    let workspace = home.join("ws");
    std::fs::create_dir_all(workspace.join(".crew")).unwrap();
    let info = json!({
        "workspace": workspace, "state_dir": workspace.join(".crew"), "socket": socket,
        "pid": pid, "started_at": 0,
    });
    std::fs::write(workspace.join(".crew/daemon.json"), info.to_string()).unwrap();
    let status = crew(&["-C", workspace.to_str().unwrap(), "status"], home, None);
    assert!(!status.status.success());
    assert!(
        text(&status.stderr).contains(&sentence),
        "{}",
        text(&status.stderr)
    );
    let stop = crew(&["-C", workspace.to_str().unwrap(), "stop"], home, None);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    assert_eq!(
        text(&stop.stdout).trim_end(),
        format!(
            "stopped the crew daemon for {} with SIGTERM, as with --keep-agents: it spoke protocol {}",
            workspace.display(),
            PROTOCOL_VERSION - 1
        )
    );
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(reaper.join().unwrap().signal(), Some(libc::SIGTERM));
}
