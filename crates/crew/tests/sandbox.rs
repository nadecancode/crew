//! What a sandboxed agent cannot do (design-b3.md §4), with real bubblewrap.
//!
//! Every test asks `require_sandbox()` first: on a host that cannot run
//! sandboxes it is skipped with the reason printed, and under
//! `CREW_TEST_SANDBOX=require` (CI, `just test-sandbox`) it fails instead.
//! Every negative check runs its positive control first: the same probe,
//! run unsandboxed, must reach the target, so "not reached" inside means
//! the sandbox stopped it and not that the probe is broken.

use std::io::Write;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::process::Command;

const CREW: &str = env!("CARGO_BIN_EXE_crew");

/// Skip (or, under `CREW_TEST_SANDBOX=require`, fail) because the host
/// lacks `what`. Returns `None` so callers can `let Some(..) = .. else`.
fn host_lacks<T>(what: impl std::fmt::Display) -> Option<T> {
    if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
        panic!("CREW_TEST_SANDBOX=require, but {what}");
    }
    eprintln!("skipped: {what}");
    None
}

/// The host can run agent sandboxes, as the daemon's own probe sees it.
fn require_sandbox() -> Option<crew::sandbox::probe::Available> {
    let crew = Path::new(CREW).canonicalize().unwrap();
    match crew::sandbox::probe::probe(&crew, std::env::var_os("PATH").as_ref()) {
        Ok(available) => Some(available),
        Err(why) => host_lacks(format!("this host cannot run sandboxes: {why}")),
    }
}

/// A program the probes need, from PATH (inside a sandbox PATH holds only
/// mounted entries, so a store path found here is found there too).
fn require_tool(name: &str) -> Option<PathBuf> {
    let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file());
    match found {
        Some(path) => Some(path),
        None => host_lacks(format!(
            "`{name}` is not on PATH (`just test-sandbox` provides it)"
        )),
    }
}

/// Things on the host an agent must not reach: a file, a pathname socket
/// and an abstract socket. The sockets answer `pong` to every connection.
struct HostTargets {
    _dir: tempfile::TempDir,
    file: PathBuf,
    secret: String,
    socket: PathBuf,
    abstract_name: String,
}

impl HostTargets {
    /// The file goes in `file_dir`, the pathname socket in `socket_dir`
    /// (say, the session's `XDG_RUNTIME_DIR`).
    fn new(file_dir: &Path, socket_dir: &Path) -> Self {
        let dir = tempfile::tempdir_in(file_dir).unwrap();
        let tag = dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let secret = format!("host-secret-{tag}");
        let file = dir.path().join("sentinel");
        std::fs::write(&file, &secret).unwrap();
        let socket = socket_dir.join(format!("{tag}.sock"));
        serve_pong(UnixListener::bind(&socket).unwrap());
        let abstract_name = format!("crew-test-{tag}");
        let addr = SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
        serve_pong(UnixListener::bind_addr(&addr).unwrap());
        Self {
            _dir: dir,
            file,
            secret,
            socket,
            abstract_name,
        }
    }

    /// A shell snippet that prints FILE, SOCKET and ABSTRACT for each target
    /// it reaches, and nothing for one it does not.
    fn probe(&self, socat: &Path) -> String {
        format!(
            r#"
            grep -qs '{secret}' '{file}' && echo FILE
            '{socat}' -t2 -T2 - UNIX-CONNECT:'{socket}' </dev/null 2>/dev/null | grep -q pong && echo SOCKET
            '{socat}' -t2 -T2 - ABSTRACT-CONNECT:'{name}' </dev/null 2>/dev/null | grep -q pong && echo ABSTRACT
            true
            "#,
            secret = self.secret,
            file = self.file.display(),
            socat = socat.display(),
            socket = self.socket.display(),
            name = self.abstract_name,
        )
    }

    /// The positive control: run unsandboxed, the probe reaches all three.
    fn assert_reachable_from_the_host(&self, socat: &Path) {
        let out = Command::new("sh")
            .args(["-c", &self.probe(socat)])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["FILE", "SOCKET", "ABSTRACT"],
            "the probe does not reach the host targets from outside: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Answer `pong` to every connection, for as long as the test runs.
fn serve_pong(listener: UnixListener) {
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(b"pong\n");
        }
    });
}

#[test]
fn the_harness_finds_a_usable_sandbox_and_its_controls_reach_the_host() {
    let Some(available) = require_sandbox() else {
        return;
    };
    let Some(socat) = require_tool("socat") else {
        return;
    };
    assert!(available.landlock_abi >= crew::sandbox::landlock::MIN_ABI);
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    std::fs::create_dir(&run).unwrap();
    HostTargets::new(root.path(), &run).assert_reachable_from_the_host(&socat);
}

// ---------------------------------------------------------------------------
// Teams that require sandboxes
// ---------------------------------------------------------------------------

mod common;
use common::{find_task, task, Session};
use crew_core::config::{AgentSpec, SandboxMode, TaskSpec, TeamConfig};
use crew_core::TaskStatus;

/// `s.team(..)`, requiring sandboxes.
fn sandboxed(s: &Session, agents: Vec<AgentSpec>, tasks: Vec<TaskSpec>) -> TeamConfig {
    let mut team = s.team("sandboxed", agents, tasks);
    team.workspace.sandbox.mode = SandboxMode::Required;
    team
}

/// Make the workspace a repository whose first commit holds `files`.
fn workspace_repo(s: &Session, files: &[(&str, &str)]) {
    std::fs::create_dir_all(&s.workspace).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&s.workspace)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    for (name, contents) in files {
        let path = s.workspace.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "--allow-empty", "-m", "first"]);
}

/// `<A>/rw/home` of agent `id`, as the daemon makes it.
fn home_of(s: &Session, id: &str) -> PathBuf {
    std::fs::create_dir_all(&s.workspace).unwrap();
    s.workspace
        .canonicalize()
        .unwrap()
        .join(".crew/agents")
        .join(id)
        .join("rw/home")
}

/// An agent that runs `script` with `sh -c`, writes what it prints to
/// `<A>/rw/home/report` and then `report.done` there (its home when
/// sandboxed; the path works unsandboxed too), and stays up.
fn probing(s: &Session, id: &str, script: &str) -> AgentSpec {
    let home = home_of(s, id);
    AgentSpec {
        id: id.to_string(),
        command: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "{{\n{script}\n}} > '{0}/report' 2>&1; touch '{0}/report.done'; exec sleep 600",
                home.display()
            ),
        ],
        ..AgentSpec::default()
    }
}

/// What agent `id`'s probe reported, once it is done.
fn report(s: &Session, id: &str) -> String {
    let home = home_of(s, id);
    s.wait_for(&format!("{id}'s report"), || {
        home.join("report.done").exists()
    });
    std::fs::read_to_string(home.join("report")).unwrap()
}

#[test]
fn required_starts_on_a_capable_host_and_is_refused_with_the_probes_reason_otherwise() {
    // Refused: on a host whose bubblewrap does not work (here a `bwrap`
    // first on PATH that prints no version), with what the probe says.
    let s = Session::new();
    workspace_repo(&s, &[]);
    let fake = s.root().join("fake-bwrap");
    std::fs::create_dir(&fake).unwrap();
    std::fs::write(fake.join("bwrap"), "#!/bin/sh\nexit 1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fake.join("bwrap"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let mut dirs = vec![fake];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(dirs).unwrap();
    let crew = Path::new(CREW).canonicalize().unwrap();
    let why = crew::sandbox::probe::probe(&crew, Some(&path)).expect_err("the probe refuses");
    let team = sandboxed(&s, vec![common::agent("a", "", &[])], vec![]);
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&why.cause) && stderr.contains(&why.fix),
        "{stderr}"
    );
    assert!(stderr.contains("workspace.sandbox.mode: off"), "{stderr}");

    // Starts: on this host, if it can run sandboxes, the team works and its
    // agent is in a mount namespace of its own.
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let team = sandboxed(
        &s,
        vec![probing(&s, "a", "readlink /proc/self/ns/mnt")],
        vec![task("t", "worker", &[])],
    );
    let mut team = team;
    team.agents.push(common::agent(
        "worker",
        "",
        &[
            "--on-task=write work.txt done",
            "--on-task=commit add work",
            "--on-task=done finished",
        ],
    ));
    s.run(&team);
    let done = s.wait("the task", |snap| {
        matches!(
            find_task(snap, "t").status,
            TaskStatus::Done | TaskStatus::Failed
        )
    });
    assert_eq!(find_task(&done, "t").status, TaskStatus::Done);
    let own = std::fs::read_link("/proc/self/ns/mnt").unwrap();
    let inside = report(&s, "a");
    assert!(inside.trim().starts_with("mnt:["), "{inside}");
    assert_ne!(
        inside.trim(),
        own.to_string_lossy(),
        "the agent shares our mounts"
    );
    s.stop();
}
