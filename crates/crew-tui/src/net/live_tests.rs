//! Actual CLI daemon, scripted PTY agent and terminal host adapter. Build the
//! CLI first and supply its absolute path; no operator service is reused.

use super::*;
use crate::{app::Modal, App};
use crew_core::config::TeamConfig;
use crew_core::util::now_ms;
use serde_json::json;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Instant;

struct Daemon {
    root: tempfile::TempDir,
    binary: PathBuf,
    workspace: PathBuf,
    socket: PathBuf,
}

impl Daemon {
    fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new(&self.binary);
        command.current_dir(self.root.path()).stdin(Stdio::null());
        // Match the existing real-daemon integration isolation without changing
        // the parent test process's environment or repository configuration.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_")
                || name.to_string_lossy().starts_with("HERDR_")
            {
                command.env_remove(name);
            }
        }
        for name in ["CREW_IDENTITY", "CREW_SOCKET", "CREW_LAUNCH"] {
            command.env_remove(name);
        }
        for (name, directory) in [
            ("XDG_RUNTIME_DIR", "run"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CONFIG_HOME", "config"),
            ("CODEX_HOME", "codex"),
            ("CLAUDE_CONFIG_DIR", "claude"),
        ] {
            command.env(name, self.root.path().join(directory));
        }
        command.env(
            "CREW_GLOBAL_CONFIG",
            self.root.path().join("etc/config.toml"),
        );
        command
    }

    fn start() -> Self {
        let binary = std::fs::canonicalize(
            std::env::var_os("CREW_TUI_TEST_BINARY")
                .expect("set CREW_TUI_TEST_BINARY to the built CLI"),
        )
        .unwrap();
        let root = tempfile::Builder::new()
            .prefix("crew-tui-live-")
            .tempdir_in("/tmp")
            .unwrap();
        let workspace = root.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut daemon = Self {
            root,
            binary,
            workspace,
            socket: PathBuf::new(),
        };
        let team: TeamConfig = serde_json::from_value(json!({
            "project": "tui-shared-live",
            "objective": "terminal shared client integration",
            "workspace": { "path": daemon.workspace, "init": false, "isolation": "shared", "sandbox": { "mode": "off" } },
            "backend": "pty", "defaults": { "runtime": "shell" },
            "orchestrator": { "agent": false },
            "agents": [{ "id": "worker", "role": "test", "command": [
                daemon.binary, "scripted-agent",
                "--on-task=send operator question choose one",
                "--on-task=wait-message operator", "--on-task=wait-message operator"
            ] }],
            "tasks": [{ "id": "work", "title": "Live work", "description": "hold until the operator responds", "agent": "worker" }]
        })).unwrap();
        std::fs::write(
            daemon.root.path().join("team.yaml"),
            team.to_yaml().unwrap(),
        )
        .unwrap();
        let result = daemon
            .command()
            .args(["run", "team.yaml", "--no-attach"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated CLI launch failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let info =
            crew_core::paths::read_daemon_info(&crew_core::paths::state_dir(&daemon.workspace))
                .expect("launched daemon metadata");
        daemon.socket = PathBuf::from(info.socket);
        assert!(
            daemon.socket.starts_with(daemon.root.path()),
            "use only the isolated test daemon"
        );
        daemon
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self
            .command()
            .arg("stop")
            .arg("-C")
            .arg(&self.workspace)
            .output();
    }
}

fn pump(
    app: &mut App,
    requests: &Requests,
    rx: &Receiver<UiEvent>,
    started: Instant,
) -> Option<Reply> {
    let event = rx.recv_timeout(Duration::from_millis(100)).ok()?;
    let result = if let UiEvent::Reply(reply) = &event {
        Some((**reply).clone())
    } else {
        None
    };
    app.shared.set_clock(crew_client::ClockSample {
        wall_ms: now_ms(),
        monotonic_ms: started.elapsed().as_millis() as u64,
    });
    crate::handle(
        app,
        event,
        requests,
        20,
        ratatui::layout::Rect::new(0, 0, 100, 30),
    )
    .unwrap();
    result
}

fn perform(
    app: &mut App,
    requests: &Requests,
    rx: &Receiver<UiEvent>,
    command: Command,
    started: Instant,
) -> crew_client::CommandResult {
    let id = if matches!(command, Command::Keys { .. }) {
        // The actual UI route captures and coalesces RawKeys, then asks the UI
        // thread to plan that burst before the worker writes it.
        crate::send_shared(app, command, requests, now_ms());
        None
    } else {
        let planned = app.prepare(command, now_ms());
        assert_eq!(planned.len(), 1, "real shared planner emits one dispatch");
        let id = planned[0].operation_id;
        crate::send_planned(app, planned.into_iter().next().unwrap(), requests, now_ms());
        Some(id)
    };
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < until, "scoped request did not complete");
        if let Some(Reply::Shared {
            operation_id,
            outcome,
            ..
        }) = pump(app, requests, rx, started)
        {
            if id.is_none_or(|id| operation_id == id) {
                let crew_client::OperationOutcome::Succeeded { result } = outcome else {
                    panic!("real request failed: {outcome:?}");
                };
                return result;
            }
        }
    }
}

#[test]
#[ignore = "requires an explicitly built CLI in CREW_TUI_TEST_BINARY"]
fn shared_terminal_adapter_against_a_real_daemon() {
    if crate::test_support::isolate_operator_test(
        "net::live_tests::shared_terminal_adapter_against_a_real_daemon",
    ) {
        return;
    }
    let daemon = Daemon::start();
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut app = App::new();
    let mut requests = spawn_scoped_requester(
        daemon.socket.clone(),
        tx.clone(),
        Some(app.shared.dispatch_guard()),
    );
    requests.subscriber = Some(spawn_subscriber(daemon.socket.clone(), tx, stop));
    let started = Instant::now();
    let until = started + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < until, "real task/question did not arrive");
        pump(&mut app, &requests, &rx, started);
        if app.connected
            && app
                .open_questions()
                .iter()
                .any(|question| question.body == "choose one")
            && app.work_of("worker", now_ms()).is_some()
        {
            break;
        }
    }
    assert_eq!(app.work_of("worker", now_ms()).unwrap().title, "Live work");
    assert!(!app.work_of("worker", now_ms()).unwrap().done);
    let output = perform(
        &mut app,
        &requests,
        &rx,
        Command::FetchOutput {
            id: "worker".into(),
        },
        started,
    );
    let crew_client::CommandResult::Output(output) = output else {
        panic!("typed output");
    };
    assert!(output.alive);
    assert!(output.text.contains("scripted agent"), "actual PTY capture");

    // A raw key remains AgentInput on the dedicated worker, with the daemon's
    // real acknowledgement. It does not establish a native attachment.
    assert_eq!(
        perform(
            &mut app,
            &requests,
            &rx,
            Command::Keys {
                id: "worker".into(),
                text: "x".into()
            },
            started
        ),
        crew_client::CommandResult::Ok
    );
    let question = app
        .open_questions()
        .iter()
        .find(|question| question.body == "choose one")
        .unwrap()
        .id
        .clone();
    let answered = perform(
        &mut app,
        &requests,
        &rx,
        Command::Answer {
            id: question.clone(),
            body: "one".into(),
        },
        started,
    );
    assert!(matches!(answered, crew_client::CommandResult::Sent(_)));
    assert!(!app
        .open_questions()
        .iter()
        .any(|message| message.id == question));
    let sent = perform(
        &mut app,
        &requests,
        &rx,
        Command::Send {
            to: "worker".into(),
            body: "follow up".into(),
        },
        started,
    );
    let crew_client::CommandResult::Sent(sent) = sent else {
        panic!("typed send");
    };
    assert_eq!(sent.message.body, "follow up");
    assert!(!sent.duplicate);

    app.modal = Some(Modal::ConfirmKill("worker".into()));
    app.capture_confirmation(now_ms());
    let killed = perform(
        &mut app,
        &requests,
        &rx,
        Command::Kill("worker".into()),
        started,
    );
    assert!(matches!(killed, crew_client::CommandResult::Agent(_)));
    requests.shutdown();
    assert!(requests.workers.is_empty());
    assert!(requests.subscriber.is_none());
}
