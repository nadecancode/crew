//! The PTY backend running an agent in a sandbox, with real bubblewrap:
//! the program is confined on its terminal, and killing the agent takes the
//! whole sandbox down, descendants that started their own session included.
//! Skipped with a printed reason where the host cannot run sandboxes;
//! `CREW_TEST_SANDBOX=require` makes that a failure.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crew::agents::LaunchSpec;
use crew::process::pty::PtyManager;
use crew::process::{ProcEvent, ProcessManager};
use crew::sandbox::probe;
use crew::sandbox::profile::{self, Extra, Host, Request, Role, Session};
use crew::vcs::Store;
use crew_core::RuntimeKind;
use tokio::sync::mpsc;

const CREW: &str = env!("CARGO_BIN_EXE_crew");

struct Layout {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    git: PathBuf,
    store: Store,
    run: PathBuf,
    socket: PathBuf,
    cargo: PathBuf,
    _listener: std::os::unix::net::UnixListener,
}

fn layout() -> Layout {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let ws = root.join("ws");
    let git = ws.join(".git");
    let run = root.join("run");
    let cargo = root.join("cache/cargo");
    for d in [
        git.join("objects"),
        git.join("refs/heads"),
        git.join("logs"),
        ws.join(".crew/agents/a/rw/home"),
        ws.join(".crew/worktrees/a"),
        run.join("0123/a"),
        cargo.clone(),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let admin = git.join("worktrees/a");
    std::fs::create_dir_all(&admin).unwrap();
    for f in ["commondir", "gitdir", "config.worktree"] {
        std::fs::write(admin.join(f), "").unwrap();
    }
    for f in ["config", "HEAD", "packed-refs"] {
        std::fs::write(git.join(f), "").unwrap();
    }
    let socket = run.join("0123.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    Layout {
        _dir: dir,
        ws,
        store: Store::Git {
            git_dir: git.clone(),
        },
        git,
        run,
        socket,
        cargo,
        _listener: listener,
    }
}

fn sandbox_available() -> Option<probe::Available> {
    let path = std::env::var_os("PATH");
    match probe::probe(Path::new(CREW), path.as_ref()) {
        Ok(available) => Some(available),
        Err(e) => {
            if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
                panic!("CREW_TEST_SANDBOX=require but sandboxes are unavailable: {e}");
            }
            eprintln!("skipped: sandboxes are unavailable: {e}");
            None
        }
    }
}

fn launch(l: &Layout, available: &probe::Available, script: &str) -> LaunchSpec {
    let host = Host::detect(Path::new(CREW), &available.bwrap, available.landlock_abi);
    let agent = l.ws.join(".crew/agents/a");
    let worktree = l.ws.join(".crew/worktrees/a");
    let admin = l.git.join("worktrees/a");
    let sidecar = l.run.join("0123/a");
    let built = profile::build(
        &host,
        &Request {
            session: Session {
                workspace: &l.ws,
                store: &l.store,
                runtime_dir: &l.run,
                daemon_socket: &l.socket,
                cargo_home: &l.cargo,
            },
            role: Role::Worker {
                agent_dir: &agent,
                worktree: &worktree,
                admin: Some(&admin),
                sidecar_dir: Some(&sidecar),
            },
            extra: &Extra::default(),
            cli: None,
            pass_env: &["AGENT_SECRET".to_string()],
            terminal: true,
            nix_daemon: None,
        },
    )
    .unwrap_or_else(|e| panic!("building the sandbox: {e}"));
    LaunchSpec {
        agent_id: "a".into(),
        kind: RuntimeKind::Shell,
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        env: vec![("AGENT_SECRET".into(), "s3cret".into())],
        cwd: worktree,
        prompt: None,
        prompt_as_arg: false,
        sidecar: None,
        sandbox: Some(built.spec),
    }
}

async fn screen_has(mgr: &PtyManager, needle: &str) -> String {
    for _ in 0..200 {
        let screen = mgr.screen("a", 50).await.unwrap();
        if screen.contains(needle) {
            return screen;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{needle:?} never appeared: {:?}", mgr.screen("a", 50).await);
}

fn host_processes_with(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) {
            if String::from_utf8_lossy(&cmdline)
                .replace('\0', " ")
                .contains(marker)
            {
                found.push(pid);
            }
        }
    }
    found
}

#[tokio::test]
async fn the_pty_backend_runs_the_agent_confined_on_its_terminal() {
    let Some(available) = sandbox_available() else {
        return;
    };
    let l = layout();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mgr = PtyManager::new(tx);
    let script = "test -t 0 && echo on-a-tty; echo pid=$$; echo secret=$AGENT_SECRET; \
echo path-hidden=$(test -e '<STATE>/crew.db' && echo no || echo yes); sleep 60";
    let script = script.replace("<STATE>", &l.ws.join(".crew").display().to_string());
    std::fs::write(l.ws.join(".crew/crew.db"), "db").unwrap();
    mgr.spawn(&launch(&l, &available, &script)).await.unwrap();
    let screen = screen_has(&mgr, "path-hidden=").await;
    assert!(screen.contains("on-a-tty"), "{screen}");
    assert!(screen.contains("secret=s3cret"), "{screen}");
    assert!(screen.contains("path-hidden=yes"), "{screen}");
    let pid: u32 = screen
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(pid < 10, "not in a fresh PID namespace: {screen}");
    mgr.kill("a").await.unwrap();
}

#[tokio::test]
async fn kill_takes_down_a_sandboxed_agent_and_a_descendant_in_its_own_session() {
    let Some(available) = sandbox_available() else {
        return;
    };
    let l = layout();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mgr = PtyManager::new(tx);
    // A sleep length only this run uses, so no other process matches.
    let length = 86400 + std::process::id() % 1000;
    let marker = format!("sleep {length}");
    let marker = marker.as_str();
    assert!(host_processes_with(marker).is_empty());
    let script = format!("setsid sleep {length} & echo started; wait");
    mgr.spawn(&launch(&l, &available, &script)).await.unwrap();
    screen_has(&mgr, "started").await;
    // Positive control: the descendant is running on the host.
    let mut running = Vec::new();
    for _ in 0..100 {
        running = host_processes_with(marker);
        if !running.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!running.is_empty(), "the descendant never started");
    mgr.kill("a").await.unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no exit event")
            .expect("channel closed");
        if matches!(event, ProcEvent::Exited { .. }) {
            break;
        }
    }
    let mut left = running;
    for _ in 0..100 {
        left = host_processes_with(marker);
        if left.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(left.is_empty(), "descendants survived kill(): {left:?}");
}
