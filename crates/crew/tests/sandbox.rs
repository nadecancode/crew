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
use common::{agent, find_agent, find_task, task, Session};
use crew_core::config::{AgentSpec, SandboxMode, TaskSpec, TeamConfig};
use crew_core::{AgentStatus, TaskStatus};

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
    let team = sandboxed(&s, vec![agent("a", "", &[])], vec![]);
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
    team.agents.push(agent(
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

/// The words a report reached, in order (lines like `REACHED <what>`).
fn reached(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter_map(|l| l.strip_prefix("REACHED "))
        .collect()
}

/// A shell snippet: print `REACHED <what>` if `cmd` succeeds.
fn reach(what: &str, cmd: &str) -> String {
    format!("( {cmd} ) >/dev/null 2>&1 && echo 'REACHED {what}'\n")
}

/// The workspace's state directory and the daemon's socket and token.
fn daemon_paths(s: &Session) -> (PathBuf, PathBuf, PathBuf) {
    let state = s.workspace.canonicalize().unwrap().join(".crew");
    let info = crew_core::paths::read_daemon_info(&state).expect("daemon.json");
    let socket = PathBuf::from(info.socket);
    let token = crew_core::paths::operator_token_path(&socket);
    (state, socket, token)
}

/// A snippet that sets `$sock` and `$token` from the agent's identity file.
const FIND_SOCKET: &str = r#"sock=$(sed -n 's/.*"socket": *"\([^"]*\)".*/\1/p' "$CREW_IDENTITY"); token="${sock%.sock}.token""#;

/// Write the team file and start it with `crew run`, with `env` added to
/// the daemon's environment.
fn run_with_env(s: &Session, team: &TeamConfig, env: &[(&str, &str)]) {
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .envs(env.iter().copied())
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "crew run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Set `key` in the workspace repository's own config, as its operator.
fn repo_config(s: &Session, key: &str, value: &str) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(&s.workspace)
        .args(["config", key, value])
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

/// Mark `name` in the workspace executable (before its first commit).
fn executable(s: &Session, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        s.workspace.join(name),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

/// Pids of processes whose arguments, after the program, start with `args`.
fn processes_with(args: &[&str]) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| {
                let argv: Vec<&[u8]> = c.split(|b| *b == 0).collect();
                argv.len() > args.len()
                    && argv[1..=args.len()]
                        .iter()
                        .zip(args)
                        .all(|(a, b)| *a == b.as_bytes())
            })
        })
        .collect()
}

#[test]
fn agent_cannot_read_operator_token_or_secrets() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[("secret.txt", "ws-secret")]);
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let home_secret = tempfile::tempdir_in(&home).unwrap();
    let home_file = home_secret.path().join("sentinel");
    std::fs::write(&home_file, "home-secret").unwrap();
    let ws = s.workspace.canonicalize().unwrap();
    let state = ws.join(".crew");
    let mut script = format!("{FIND_SOCKET}\necho \"socket=$sock\"\n");
    script.push_str(&reach("token", r#"cat "$token""#));
    for (what, path) in [
        ("db", state.join("crew.db")),
        ("identity-of-b", state.join("agents/b/identity.json")),
        ("home-sentinel", home_file.clone()),
        ("ws-sentinel", ws.join("secret.txt")),
    ] {
        script.push_str(&reach(what, &format!("cat '{}'", path.display())));
    }
    let team = sandboxed(
        &s,
        vec![
            agent("b", "", &["--on-start=sleep 600000"]),
            probing(&s, "a", &script),
        ],
        vec![],
    );
    s.run(&team);
    let report = report(&s, "a");
    // Positive controls: every target is readable from outside.
    let (_, socket, token) = daemon_paths(&s);
    assert!(
        report.contains(&format!("socket={}", socket.display())),
        "{report}"
    );
    for path in [
        token,
        state.join("crew.db"),
        state.join("agents/b/identity.json"),
        home_file,
        ws.join("secret.txt"),
    ] {
        assert!(
            std::fs::read(&path).is_ok(),
            "{} is not readable outside",
            path.display()
        );
    }
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    s.stop();
}

#[test]
fn agent_cannot_reach_host_sockets() {
    if require_sandbox().is_none() {
        return;
    }
    let Some(socat) = require_tool("socat") else {
        return;
    };
    let s = Session::new();
    workspace_repo(&s, &[]);
    let run_dir = s.root().join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let targets = HostTargets::new(s.root(), &run_dir);
    targets.assert_reachable_from_the_host(&socat);
    let ws = s.workspace.canonicalize().unwrap();
    let late = ws.join(".git/late.sock");
    let home = home_of(&s, "a");
    let script = format!(
        r#"{FIND_SOCKET}
echo "socket=$sock"
{probe}
{{ find / \( -path /proc -o -path /nix/store \) -prune -o -type s -print
  while read -r _ _ _ _ p _; do [ -S "$p" ] && echo "$p"; done < /proc/self/mountinfo; }} | sort -u | sed 's/^/SOCKET-SEEN /'
echo ready > '{home}/ready'
while [ ! -e '{home}/go' ]; do sleep 0.1; done
'{socat}' -t2 -T2 - UNIX-CONNECT:'{late}' </dev/null 2>/dev/null | grep -q pong && echo 'REACHED late-socket'
true"#,
        probe = targets
            .probe(&socat)
            .replace("echo FILE", "echo 'REACHED file'")
            .replace("echo SOCKET", "echo 'REACHED socket'")
            .replace("echo ABSTRACT", "echo 'REACHED abstract'"),
        home = home.display(),
        socat = socat.display(),
        late = late.display(),
    );
    s.run(&sandboxed(&s, vec![probing(&s, "a", &script)], vec![]));
    s.wait_for("the probe to be ready", || home.join("ready").exists());
    // A socket made in a mounted read-only tree after the agent started.
    serve_pong(std::os::unix::net::UnixListener::bind(&late).unwrap());
    let control = Command::new(&socat)
        .args([
            "-t2",
            "-T2",
            "-",
            &format!("UNIX-CONNECT:{}", late.display()),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&control.stdout).contains("pong"),
        "control: late socket"
    );
    std::fs::write(home.join("go"), "").unwrap();
    let report = report(&s, "a");
    let (_, socket, _) = daemon_paths(&s);
    let seen: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("SOCKET-SEEN "))
        .collect();
    assert_eq!(seen, [socket.to_str().unwrap()], "{report}");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    s.stop();
}

#[test]
fn agent_without_crew_identity_cannot_become_operator() {
    if require_sandbox().is_none() {
        return;
    }
    let Some(socat) = require_tool("socat") else {
        return;
    };
    let s = Session::new();
    workspace_repo(&s, &[]);
    let hello = |token: &str| {
        let token = if token.is_empty() {
            String::new()
        } else {
            format!(r#","token":"{token}""#)
        };
        format!(
            r#"{{"id":1,"method":"hello","params":{{"role":"operator","client":"x","protocol":{}{token}}}}}"#,
            crew_core::rpc::PROTOCOL_VERSION
        )
    };
    let guessed = "0".repeat(64);
    let script = format!(
        r#"{FIND_SOCKET}
for attempt in none guessed; do
  case $attempt in none) frame='{none}';; guessed) frame='{guess}';; esac
  reply=$(printf '%s\n' "$frame" | '{socat}' -t2 -T2 - UNIX-CONNECT:"$sock" 2>&1)
  echo "HELLO $attempt $reply"
  case "$reply" in *'"result"'*) echo "REACHED operator-$attempt";; esac
done
'{crew}' ctl inbox >/dev/null 2>&1 && echo 'AGENT-CALL ok'
true"#,
        none = hello(""),
        guess = hello(&guessed),
        socat = socat.display(),
        crew = CREW,
    );
    s.run(&sandboxed(&s, vec![probing(&s, "a", &script)], vec![]));
    let report = report(&s, "a");
    assert!(
        report.contains("AGENT-CALL ok"),
        "its own agent calls work: {report}"
    );
    for attempt in ["none", "guessed"] {
        let line = report
            .lines()
            .find(|l| l.starts_with(&format!("HELLO {attempt} ")))
            .unwrap_or_else(|| panic!("no {attempt} attempt: {report}"));
        assert!(line.contains("unauthorized"), "{line}");
    }
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    // Positive control: with the real token the same frame is accepted.
    let (_, socket, token) = daemon_paths(&s);
    let token = std::fs::read_to_string(token).unwrap();
    let mut control = Command::new(&socat)
        .args([
            "-t2",
            "-T2",
            "-",
            &format!("UNIX-CONNECT:{}", socket.display()),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(control.stdin.take().unwrap(), "{}", hello(token.trim())).unwrap();
    let out = control.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(r#""result""#),
        "control: {out:?}"
    );
    s.stop();
}

#[test]
fn agent_cannot_see_host_processes() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let me = std::process::id();
    // Positive control: this process is visible and signalable outside.
    assert!(std::fs::read(format!("/proc/{me}/environ")).is_ok());
    let script = format!(
        "{}{}{}",
        reach("environ", &format!("cat /proc/{me}/environ")),
        reach("root", &format!("ls /proc/{me}/root/")),
        reach("kill", &format!("kill -0 {me}")),
    );
    s.run(&sandboxed(&s, vec![probing(&s, "a", &script)], vec![]));
    let report = report(&s, "a");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    s.stop();
}

#[test]
fn no_inherited_descriptors_or_env_secrets() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let script = r#"env | grep -q CREW_TEST_SENTINEL && echo 'REACHED env'
for fd in /proc/self/fd/*; do
  t=$(readlink "$fd")
  case "$t" in *crew.db*|*daemon.lock*|socket:*) echo "REACHED fd $t";; esac
done
true"#;
    run_with_env(
        &s,
        &sandboxed(&s, vec![probing(&s, "a", script)], vec![]),
        &[("CREW_TEST_SENTINEL", "daemon-secret")],
    );
    let report = report(&s, "a");
    // Positive control: the daemon has the sentinel, the database and the
    // lock open, so a leak would show.
    let state = s.workspace.canonicalize().unwrap().join(".crew");
    let pid = crew_core::paths::read_daemon_info(&state).unwrap().pid;
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    assert!(String::from_utf8_lossy(&environ).contains("CREW_TEST_SENTINEL=daemon-secret"));
    let open: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert!(open.iter().any(|p| p.ends_with("crew.db")), "{open:?}");
    assert!(open.iter().any(|p| p.ends_with("daemon.lock")), "{open:?}");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    s.stop();
}

#[test]
fn sandboxed_agent_can_do_its_work() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    let hook = format!(
        "#!/bin/sh\n{FIND_SOCKET}\ncat \"$token\" >/dev/null 2>&1 && echo REACHED > \"$HOME/hook-token\"\necho ran > \"$HOME/hook-ran\"\n"
    );
    std::fs::create_dir_all(s.workspace.join(".githooks")).unwrap();
    std::fs::write(s.workspace.join(".githooks/pre-commit"), &hook).unwrap();
    executable(&s, ".githooks/pre-commit");
    workspace_repo(&s, &[]);
    repo_config(&s, "core.hooksPath", ".githooks");
    let team = sandboxed(
        &s,
        vec![agent(
            "a",
            "",
            &[
                "--on-task=progress 50 halfway",
                "--on-task=write work.txt done",
                "--on-task=commit add work",
                "--on-task=done finished",
            ],
        )],
        vec![task("t", "a", &[])],
    );
    s.run(&team);
    let done = s.wait("the task", |snap| {
        matches!(
            find_task(snap, "t").status,
            TaskStatus::Done | TaskStatus::Failed
        )
    });
    let t = find_task(&done, "t");
    assert_eq!(t.status, TaskStatus::Done, "{:?}", t.note);
    assert_eq!(t.result.as_deref(), Some("finished"));
    assert!(
        t.artifacts.iter().any(|a| a.kind == "commit"),
        "{:?}",
        t.artifacts
    );
    let home = home_of(&s, "a");
    assert!(
        home.join("hook-ran").exists(),
        "the pre-commit hook did not run"
    );
    let (_, _, token) = daemon_paths(&s);
    assert!(
        std::fs::read(&token).is_ok(),
        "control: the token is readable outside"
    );
    assert!(
        !home.join("hook-token").exists(),
        "the hook read the operator token"
    );
    s.stop();
}

#[test]
fn orchestrator_reads_but_cannot_write_the_checkout() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[("readme.txt", "hello")]);
    let ws = s.workspace.canonicalize().unwrap();
    let w = ws.display().to_string();
    let script = format!(
        r#"cat '{w}/readme.txt' | grep -q hello && echo 'CAN read'
{}{}{}{}{}"#,
        reach("write-checkout", &format!("echo x > '{w}/new.txt'")),
        reach("write-existing", &format!("echo x >> '{w}/readme.txt'")),
        reach("crew-db", &format!("cat '{w}/.crew/crew.db'")),
        reach("other-agent", &format!("ls '{w}/.crew/agents/b'")),
        reach("git-dir", &format!("cat '{w}/.git/HEAD'")),
    ) + &format!("echo \"agents: $(ls '{w}/.crew/agents' | tr '\\n' ' ')\"\n");
    let mut team = sandboxed(
        &s,
        vec![agent("b", "", &["--on-start=sleep 600000"])],
        vec![],
    );
    team.orchestrator.agent = true;
    let crew_core::config::AgentSpec { command, .. } =
        probing(&s, crew_core::ORCHESTRATOR_ID, &script);
    team.orchestrator.command = command;
    s.run(&team);
    let report = report(&s, crew_core::ORCHESTRATOR_ID);
    assert!(report.contains("CAN read"), "{report}");
    // Its own directory is mounted beneath the masked .crew; nothing else is.
    assert!(report.contains("agents: orchestrator \n"), "{report}");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    assert!(ws.join(".crew/agents/b").is_dir() && ws.join(".crew/crew.db").exists());
    assert!(!ws.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(ws.join("readme.txt")).unwrap(),
        "hello"
    );
    s.stop();
}

#[test]
fn kill_tears_down_the_pty_tree() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let marker = format!("{}", 900_000 + std::process::id() % 99_999);
    let script = format!("setsid sleep {marker} </dev/null >/dev/null 2>&1 &\necho started");
    s.run(&sandboxed(&s, vec![probing(&s, "a", &script)], vec![]));
    report(&s, "a");
    let needle = [marker.as_str()];
    s.wait_for("the setsid descendant", || {
        !processes_with(&needle).is_empty()
    });
    s.agent_cmd(&["kill", "a"]);
    s.wait_for("the descendant to be gone", || {
        processes_with(&needle).is_empty()
    });
    s.stop();
}

#[test]
fn required_refuses_without_bwrap() {
    if require_sandbox().is_none() {
        return;
    }
    let path = std::env::var_os("PATH").unwrap();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let without: Vec<&PathBuf> = dirs.iter().filter(|d| !d.join("bwrap").exists()).collect();
    if !without.iter().any(|d| d.join("git").exists()) {
        eprintln!("skipped: git and bwrap share a PATH directory here");
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let team = sandboxed(&s, vec![agent("a", "", &[])], vec![]);
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("PATH", std::env::join_paths(without).unwrap())
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("bubblewrap (bwrap) is not on PATH"),
        "{stderr}"
    );
    assert!(stderr.contains("workspace.sandbox.mode: off"), "{stderr}");
}

#[test]
fn required_refuses_herdr_and_shared() {
    // Shared: the agents would write the operator's checkout.
    let s = Session::new();
    workspace_repo(&s, &[]);
    let mut team = sandboxed(&s, vec![agent("a", "", &[])], vec![]);
    team.workspace.isolation = crew_core::config::Isolation::Shared;
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s.crew(&["run", "team.yaml", "--no-attach"]);
    let stderr = common::stderr(&out);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("agents sharing the checkout cannot be sandboxed")
            && stderr.contains("workspace.sandbox.mode: off"),
        "{stderr}"
    );

    // herdr: it starts its own programs, so crew cannot put bubblewrap in
    // front of them. A herdr that answers its ping is enough to get there.
    let s = Session::new();
    workspace_repo(&s, &[]);
    let herdr = s.root().join("herdr.sock");
    let listener = UnixListener::bind(&herdr).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // Read the ping first: closing before it arrives fails its write.
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            let _ = std::io::BufRead::read_line(&mut reader, &mut line);
            let _ = reader
                .get_mut()
                .write_all(b"{\"id\":\"crew:ping\",\"result\":{}}\n");
        }
    });
    let mut team = sandboxed(&s, vec![agent("a", "", &[])], vec![]);
    team.backend = crew_core::config::BackendChoice::Herdr;
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("HERDR_SOCKET_PATH", &herdr)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    let stderr = common::stderr(&out);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("herdr backend starts its own programs")
            && stderr.contains("workspace.sandbox.mode: off"),
        "{stderr}"
    );
    // Control: with sandboxes off the same herdr team gets past the check.
    let mut off = team;
    off.workspace.sandbox.mode = SandboxMode::Off;
    std::fs::write(s.root().join("team.yaml"), off.to_yaml().unwrap()).unwrap();
    let out = s
        .command()
        .env("HERDR_SOCKET_PATH", &herdr)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(!common::stderr(&out).contains("starts its own programs"));
    s.stop();
}

#[test]
fn a_sandbox_that_cannot_start_never_runs_the_program() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    // A program only the host has (/tmp is private in a sandbox), which
    // leaves a sentinel (its argument) when it runs.
    let bin = s.root().join("hostonly");
    let sentinel = s.root().join("ran-unsandboxed");
    std::fs::write(&bin, "#!/bin/sh\ntouch \"$1\"\nexec sleep 600\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Positive control: it runs on the host.
    let control = s.root().join("control");
    let mut child = Command::new(&bin).arg(&control).spawn().unwrap();
    s.wait_for("the control run", || control.exists());
    child.kill().unwrap();
    child.wait().unwrap();
    let mut spec = agent("a", "", &[]);
    spec.command = vec![
        bin.to_string_lossy().into_owned(),
        sentinel.to_string_lossy().into_owned(),
    ];
    s.run(&sandboxed(&s, vec![spec], vec![]));
    s.wait("the agent to give up", |snap| {
        matches!(
            find_agent(snap, "a").status,
            AgentStatus::Failed | AgentStatus::Stopped
        )
    });
    assert!(!sentinel.exists(), "the program ran outside a sandbox");
    s.stop();
}

#[test]
fn what_a_sandbox_leaves_out_is_reported_once_per_agent_start() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    // A PATH entry inside the protected XDG_RUNTIME_DIR: every profile
    // leaves it out.
    let dropped = s.root().join("run/bin");
    std::fs::create_dir_all(&dropped).unwrap();
    let mut path = vec![dropped.clone()];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let team = sandboxed(
        &s,
        vec![agent(
            "a",
            "",
            &[
                "--on-task=write a.txt work",
                "--on-task=commit work",
                "--on-task=done ok",
            ],
        )],
        vec![task("t", "a", &[])],
    );
    let path = std::env::join_paths(path).unwrap();
    run_with_env(&s, &team, &[("PATH", path.to_str().unwrap())]);
    s.wait("the task", |snap| {
        find_task(snap, "t").status == TaskStatus::Done
    });
    let out = s.crew(&["integrate", "-C", &s.workspace.display().to_string()]);
    assert!(out.status.success(), "{}", common::stderr(&out));
    let events = s.events();
    let about_it: Vec<&str> = events
        .iter()
        .filter(|e| e.event_type == "sandbox.warning")
        .map(|e| e.message.as_str())
        .filter(|m| m.contains(&dropped.display().to_string()))
        .collect();
    // One start of `a`: its fill, commit and the integration add nothing.
    assert_eq!(about_it.len(), 1, "{about_it:#?}");
    s.stop();
}
