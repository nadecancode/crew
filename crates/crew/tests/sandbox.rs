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
    write_executable(&fake.join("bwrap"), "#!/bin/sh\nexit 1\n");
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
/// Write the program `contents` to `path`, from a child process. A file
/// this test process writes can be held open, for a moment, by a child
/// another test is forking, and running it then fails with "Text file
/// busy" (ETXTBSY).
fn write_executable(path: &Path, contents: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "writing {}",
        path.display()
    );
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
    write_executable(&s.workspace.join(".githooks/pre-commit"), &hook);
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
    write_executable(&bin, "#!/bin/sh\ntouch \"$1\"\nexec sleep 600\n");
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

#[test]
fn agent_spawn_does_not_run_branch_hooks_or_filters_unsandboxed() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    let marks = tempfile::tempdir_in(s.root()).unwrap();
    let hook_mark = marks.path().join("post-checkout");
    let filter_mark = marks.path().join("smudge");
    std::fs::create_dir_all(s.workspace.join(".githooks")).unwrap();
    write_executable(
        &s.workspace.join(".githooks/post-checkout"),
        &format!("#!/bin/sh\ntouch '{}'\n", hook_mark.display()),
    );
    workspace_repo(
        &s,
        &[
            (".gitattributes", "*.txt filter=mark\n"),
            ("data.txt", "data\n"),
        ],
    );
    repo_config(&s, "core.hooksPath", ".githooks");
    let smudge = format!("sh -c \"touch '{}'; cat\"", filter_mark.display());
    repo_config(&s, "filter.mark.smudge", &smudge);
    // Positive control: run on the host, both write their marks.
    let ran = Command::new("sh")
        .args([
            "-c",
            &format!(
                "{} </dev/null >/dev/null; '{}'",
                smudge,
                s.workspace.join(".githooks/post-checkout").display()
            ),
        ])
        .status()
        .unwrap()
        .success();
    assert!(ran && hook_mark.exists() && filter_mark.exists());
    std::fs::remove_file(&hook_mark).unwrap();
    std::fs::remove_file(&filter_mark).unwrap();

    s.run(&sandboxed(
        &s,
        vec![agent("a", "", &["--on-start=sleep 600000"])],
        vec![],
    ));
    s.wait("a to start", |snap| find_agent(snap, "a").pid.is_some());
    let worktree = s.workspace.join(".crew/worktrees/a");
    assert_eq!(
        std::fs::read_to_string(worktree.join("data.txt")).unwrap(),
        "data\n"
    );
    assert!(!hook_mark.exists(), "post-checkout ran on the host");
    assert!(!filter_mark.exists(), "the smudge filter ran on the host");
    s.stop();
}

#[test]
fn integrate_hooks_have_no_authority() {
    if require_sandbox().is_none() {
        return;
    }
    let Some(socat) = require_tool("socat") else {
        return;
    };
    let s = Session::new();
    let marks = tempfile::tempdir_in(s.root()).unwrap();
    let host_mark = marks.path().join("from-the-hook");
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in tcp.incoming().flatten() {
            let _ = std::io::Write::write_all(&mut stream, b"pong\n");
        }
    });
    let ws = s.workspace.clone();
    std::fs::create_dir_all(&ws).unwrap();
    let ws_c = ws.canonicalize().unwrap();
    let identity = ws_c.join(".crew/agents/a/identity.json");
    // The hook writes what it reached into the integration worktree.
    let checks = [
        reach("host-file", &format!("touch '{}'", host_mark.display())),
        reach("identity-file", &format!("cat '{}'", identity.display())),
        reach("identity-env", "test -n \"$CREW_IDENTITY\""),
        // An agent call as `a`, its identity named outright (operator
        // commands are refused under CREW_IDENTITY anyway, since G1).
        reach(
            "daemon",
            &format!("CREW_IDENTITY='{}' '{CREW}' ctl inbox", identity.display()),
        ),
        reach(
            "network",
            &format!(
                "'{}' -t2 -T2 - TCP:127.0.0.1:{port} </dev/null | grep -q pong",
                socat.display()
            ),
        ),
    ]
    .concat();
    std::fs::create_dir_all(ws.join(".githooks")).unwrap();
    write_executable(
        &ws.join(".githooks/post-merge"),
        &format!("#!/bin/sh\n{{\n{checks}\n}} > hook-report 2>&1\n"),
    );
    workspace_repo(&s, &[]);
    repo_config(&s, "core.hooksPath", ".githooks");
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
    s.run(&team);
    s.wait("the task", |snap| {
        find_task(snap, "t").status == TaskStatus::Done
    });
    // Positive controls: every target is reachable from outside.
    let control = Command::new("sh")
        .args([
            "-c",
            &checks.replace(
                &host_mark.display().to_string(),
                &marks.path().join("control").display().to_string(),
            ),
        ])
        .env("CREW_IDENTITY", &identity)
        .output()
        .unwrap();
    let control = String::from_utf8_lossy(&control.stdout).into_owned();
    assert_eq!(
        reached(&control),
        [
            "host-file",
            "identity-file",
            "identity-env",
            "daemon",
            "network"
        ],
        "{control}"
    );

    let out = s.crew(&["integrate", "-C", &ws.display().to_string()]);
    assert!(out.status.success(), "{}", common::stderr(&out));
    let report_file = ws.join(".crew/worktrees/_integration/hook-report");
    let report = std::fs::read_to_string(&report_file).expect("the post-merge hook ran");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    assert!(!host_mark.exists());
    s.stop();
}

#[test]
fn git_status_ignores_agent_fsmonitor() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let marks = tempfile::tempdir_in(s.root()).unwrap();
    let mark = marks.path().join("fsmonitor-ran");
    let ws = s.workspace.canonicalize().unwrap();
    let admin = ws.join(".git/worktrees/a");
    let script = format!(
        "{}{}",
        reach(
            "config-worktree",
            &format!("echo '[core]' > '{}/config.worktree'", admin.display())
        ),
        reach(
            "git-config",
            &format!(
                "git config --worktree core.fsmonitor \"touch '{}'\"",
                mark.display()
            )
        ),
    );
    s.run(&sandboxed(&s, vec![probing(&s, "a", &script)], vec![]));
    let report = report(&s, "a");
    assert_eq!(reached(&report), Vec::<&str>::new(), "{report}");
    assert_eq!(
        std::fs::read_to_string(admin.join("config.worktree")).unwrap(),
        ""
    );
    for (tool, args) in [("git_status", "{}"), ("git_commit", r#"{"message":"m"}"#)] {
        let _ = s.ctl("a", &["tool", tool, args]);
    }
    assert!(!mark.exists(), "fsmonitor ran on the host");
    s.stop();
}
/// Wrappers for `claude`, `codex` and `pi` that run `crew fake-cli`,
/// installed the way the real CLIs are: the program in `real/`, a symlink to
/// it in `bin/` (the directory that goes on PATH), beside the crew binary
/// (mounted in a sandbox; `/tmp` is not). The `codex` wrapper also notes,
/// in its `$CODEX_HOME`, the mount namespace and arguments of a queue run.
fn fake_clis() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let base = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::fs::create_dir_all(base.join("bin")).unwrap();
    for name in ["claude", "codex", "pi"] {
        let note = if name == "codex" {
            "[ \"$1\" = queue ] && { readlink /proc/self/ns/mnt > \"$CODEX_HOME/queue-ns\"; \
printf '%s\\n' \"$@\" > \"$CODEX_HOME/queue-args\"; }\n\
[ \"$1\" = --remote ] && [ -n \"$CREW_IDENTITY\" ] && { a=$(dirname \"$CREW_IDENTITY\"); \
ln -sf /nonexistent/sentinel \"$a/briefing.md\" 2>/dev/null && echo REACHED link >> \"$CODEX_HOME/tamper\"; \
rm -f \"$a/briefing.md\" 2>/dev/null && echo REACHED unlink >> \"$CODEX_HOME/tamper\"; \
echo tried >> \"$CODEX_HOME/tamper\"; }\n"
        } else {
            ""
        };
        let path = base.join("real").join(name);
        write_executable(
            &path,
            &format!("#!/bin/sh\n{note}exec '{CREW}' fake-cli {name} \"$@\"\n"),
        );
        std::os::unix::fs::symlink(&path, base.join("bin").join(name)).unwrap();
    }
    (dir, base.join("bin"))
}

/// PATH with `first` in front of this process's.
fn path_with(first: &Path) -> String {
    let mut dirs = vec![first.to_path_buf()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap().into_string().unwrap()
}

/// A Codex agent (the fake one, with its app-server as sidecar).
fn codex_agent(id: &str) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        role: "implementation".to_string(),
        runtime: Some(crew_core::RuntimeKind::Codex),
        ..AgentSpec::default()
    }
}

/// The mount namespace of process `pid`.
fn mount_ns(pid: u32) -> String {
    std::fs::read_link(format!("/proc/{pid}/ns/mnt"))
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Wait until agent `id` is up: started, and no longer starting.
fn wait_up(s: &Session, id: &str) {
    s.wait(&format!("{id} to be up"), |snap| {
        let a = common::find_agent(snap, id);
        a.pid.is_some() && a.status != crew_core::AgentStatus::Spawning
    });
}

/// The `--listen` socket of the fake app-server agent `id` runs, from its
/// own record, and the pids serving it.
fn sidecar_of(s: &Session, id: &str) -> Vec<u32> {
    let record = home_of(s, id).join("codex/fake-codex-server.json");
    s.wait_for("the sidecar's record", || record.is_file());
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    let listen = record["args"][2].as_str().unwrap().to_string();
    processes_with(&["fake-cli", "codex", "app-server", "--listen", &listen])
}

#[test]
fn codex_sidecar_and_queue_run_sandboxed() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let (_fakes, bin) = fake_clis();
    let team = sandboxed(&s, vec![codex_agent("x")], vec![]);
    run_with_env(&s, &team, &[("PATH", &path_with(&bin))]);
    wait_up(&s, "x");
    let codex_home = home_of(&s, "x").join("codex");

    // The sidecar runs, and not in our mount namespace.
    s.wait_for("the fake app-server", || {
        !processes_with(&["fake-cli", "codex", "app-server"]).is_empty()
    });
    let own = mount_ns(std::process::id());
    for pid in processes_with(&["fake-cli", "codex", "app-server"]) {
        assert_ne!(mount_ns(pid), own, "the sidecar shares our mounts");
    }
    assert!(codex_home.join("fake-codex-server.json").is_file());

    // Its session id arrives the way Codex's notify hook brings it; then a
    // message goes in through the queue command.
    let identity = s.workspace.join(".crew/agents/x/identity.json");
    let out = s
        .command()
        .args(["hook", "codex-notify", "--identity"])
        .arg(&identity)
        .arg(r#"{"type":"agent-turn-complete","thread-id":"fake-thread"}"#)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    s.wait("the session id", |snap| {
        common::find_agent(snap, "x").session_id.as_deref() == Some("fake-thread")
    });
    s.send("x", "through the queue");
    s.wait_for("the queue command", || {
        codex_home.join("queue-args").is_file()
    });
    let args = std::fs::read_to_string(codex_home.join("queue-args")).unwrap();
    assert!(args.contains("through the queue"), "{args}");
    let queue_ns = std::fs::read_to_string(codex_home.join("queue-ns")).unwrap();
    assert_ne!(queue_ns.trim(), own, "the queue command shares our mounts");
    s.wait("the message delivered", |snap| {
        common::find_agent(snap, "x").delivery == "socket"
    });
    s.stop();
}

#[test]
fn kill_tears_down_the_sidecar_tree() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let (_fakes, bin) = fake_clis();
    let team = sandboxed(&s, vec![codex_agent("x")], vec![]);
    run_with_env(&s, &team, &[("PATH", &path_with(&bin))]);
    wait_up(&s, "x");
    // This team's sidecar: the one listening where its own record says.
    let record = home_of(&s, "x").join("codex/fake-codex-server.json");
    s.wait_for("the sidecar's record", || record.is_file());
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    let listen = record["args"][2].as_str().unwrap().to_string();
    let mine = processes_with(&["fake-cli", "codex", "app-server", "--listen", &listen]);
    assert!(!mine.is_empty(), "this team's sidecar");
    s.agent_cmd(&["kill", "x"]);
    s.wait_for("the sidecar to be gone", || {
        mine.iter()
            .all(|pid| !Path::new(&format!("/proc/{pid}")).exists())
    });
    s.stop();
}

#[test]
fn compaction_does_not_read_through_symlinks() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let (_fakes, bin) = fake_clis();
    let team = sandboxed(&s, vec![codex_agent("x")], vec![]);
    run_with_env(&s, &team, &[("PATH", &path_with(&bin))]);
    wait_up(&s, "x");
    let ws = s.workspace.canonicalize().unwrap();
    let briefing = ws.join(".crew/agents/x/briefing.md");
    let codex_home = home_of(&s, "x").join("codex");

    // From inside, the agent tried to put a link in place of its briefing,
    // and to remove it; both failed.
    s.wait_for("the agent's attempt", || {
        std::fs::read_to_string(codex_home.join("tamper")).is_ok_and(|t| t.contains("tried"))
    });
    let tamper = std::fs::read_to_string(codex_home.join("tamper")).unwrap();
    assert_eq!(reached(&tamper), Vec::<&str>::new(), "{tamper}");
    let real = std::fs::read_to_string(&briefing).unwrap();
    assert!(std::fs::symlink_metadata(&briefing).unwrap().is_file());

    // Its session id and a finished turn, as Codex's notify hook brings them.
    let identity = ws.join(".crew/agents/x/identity.json");
    let out = s
        .command()
        .args(["hook", "codex-notify", "--identity"])
        .arg(&identity)
        .arg(r#"{"type":"agent-turn-complete","thread-id":"fake-thread"}"#)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    s.wait("the session id", |snap| {
        common::find_agent(snap, "x").session_id.as_deref() == Some("fake-thread")
    });
    // A compaction is queued and asked for; then the agent's own session
    // file says it happened (anything there is the agent's to write).
    s.agent_cmd(&["compact", "x"]);
    s.wait_for("/compact to be typed", || {
        s.agent_cmd(&["output", "x"]).contains("/compact")
    });
    let rollout =
        codex_home.join("sessions/2026/10/03/rollout-2026-10-03T12-00-00-fake-thread.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap();
    writeln!(
        file,
        r#"{{"timestamp":"2026-10-03T12:00:05Z","type":"compacted","payload":{{"message":""}}}}"#
    )
    .unwrap();
    drop(file);

    // The daemon tells the agent where its work stands, through the queue.
    // Its standing instructions are in the system prompt, which a compaction
    // keeps: nothing is read back from the agent's directory for it.
    s.wait_for("the compaction note", || {
        std::fs::read_to_string(codex_home.join("queue-args"))
            .is_ok_and(|a| a.contains("Your conversation was just compacted"))
    });
    let sent = std::fs::read_to_string(codex_home.join("queue-args")).unwrap();
    assert!(
        !sent.contains("Your standing instructions, again"),
        "{sent}"
    );
    let first = real.lines().find(|l| !l.trim().is_empty()).unwrap();
    assert!(!sent.contains(first), "{sent}");
    assert_eq!(std::fs::read_to_string(&briefing).unwrap(), real);
    assert!(std::fs::symlink_metadata(&briefing).unwrap().is_file());
    s.stop();
}

#[test]
fn daemon_writes_do_not_follow_planted_links() {
    use std::os::unix::fs::symlink;
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let (_fakes, bin) = fake_clis();
    // The operator's Codex settings, which seeding copies (filtered).
    let operator = s.root().join("codex-home");
    std::fs::create_dir_all(&operator).unwrap();
    std::fs::write(operator.join("config.toml"), "model = \"gpt-5\"\n").unwrap();
    let outside = tempfile::tempdir_in(s.root()).unwrap();
    let sentinel = outside.path().join("sentinel");
    std::fs::write(&sentinel, "outside\n").unwrap();

    let team = sandboxed(&s, vec![codex_agent("x")], vec![]);
    run_with_env(&s, &team, &[("PATH", &path_with(&bin))]);
    wait_up(&s, "x");
    let home = home_of(&s, "x");
    let config = home.join("codex/config.toml");
    assert!(std::fs::read_to_string(&config).unwrap().contains("gpt-5"));
    let rollouts = home.join("codex/sessions");
    s.wait_for("the session file", || rollouts.is_dir());
    let old_sidecar = sidecar_of(&s, "x");
    assert!(!old_sidecar.is_empty());

    // What the agent can do to its own tree before its next start: a link
    // where seeding writes.
    std::fs::remove_file(&config).unwrap();
    symlink(&sentinel, &config).unwrap();
    s.agent_cmd(&["restart", "x"]);
    wait_up(&s, "x");
    // Its previous sidecar was gone first; the link was replaced, not
    // followed; its session files are still there.
    assert!(old_sidecar
        .iter()
        .all(|pid| !Path::new(&format!("/proc/{pid}")).exists()));
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "outside\n");
    let meta = std::fs::symlink_metadata(&config).unwrap();
    assert!(meta.is_file(), "config.toml is still a link");
    assert!(std::fs::read_to_string(&config).unwrap().contains("gpt-5"));
    assert!(
        rollouts.is_dir(),
        "the session files did not survive the restart"
    );

    // And its whole home replaced by a link: the next start fails rather
    // than write there.
    s.agent_cmd(&["kill", "x"]);
    s.wait("x to stop", |snap| {
        common::find_agent(snap, "x").status == crew_core::AgentStatus::Stopped
    });
    std::fs::rename(&home, home.with_file_name("home.moved")).unwrap();
    symlink(outside.path(), &home).unwrap();
    s.agent_cmd(&["restart", "x"]);
    let failed = s.wait("the restart to fail", |snap| {
        common::find_agent(snap, "x").status == crew_core::AgentStatus::Failed
    });
    let error = common::find_agent(&failed, "x")
        .error
        .clone()
        .unwrap_or_default();
    assert!(error.contains("home"), "{error}");
    let left: Vec<_> = std::fs::read_dir(outside.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        left,
        [std::ffi::OsString::from("sentinel")],
        "written through the link"
    );
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "outside\n");
    s.stop();
}

#[test]
fn host_toolchain_layout_starts() {
    use std::os::unix::fs::symlink;
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let ws = s.workspace.canonicalize().unwrap();
    // crew built in the checkout's target/, as on the operator's host: one
    // read-only file is bound from there.
    let release = ws.join("target/release");
    std::fs::create_dir_all(&release).unwrap();
    let crew = release.join("crew");
    std::fs::copy(CREW, &crew).unwrap();
    // A home with the toolchain where rustup puts it, git's config, and a
    // CLI installed the way Claude Code's native installer does (a versioned
    // binary, a symlink to it on PATH). Beside the build, not under /tmp,
    // which a sandbox never mounts.
    let fixture = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let home = fixture.path().canonicalize().unwrap();
    // The toolchain the tests themselves were built with.
    let (Some(rustup), Some(cargo_home)) = (
        std::env::var_os("RUSTUP_HOME").map(PathBuf::from),
        std::env::var_os("CARGO_HOME").map(PathBuf::from),
    ) else {
        host_lacks::<()>("RUSTUP_HOME and CARGO_HOME (a rustup toolchain)");
        return;
    };
    if !cargo_home.join("bin/cargo").exists() || !rustup.join("settings.toml").exists() {
        host_lacks::<()>("a rustup toolchain (CARGO_HOME/bin/cargo and RUSTUP_HOME)");
        return;
    }
    // Where rustup puts them; here the tests' own, linked in.
    symlink(&cargo_home, home.join(".cargo")).unwrap();
    symlink(&rustup, home.join(".rustup")).unwrap();
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = fixture\n").unwrap();
    // The agent's CLI: it checks its tools, as a CLI's own tool calls
    // would, then runs the fake Claude Code.
    let versions = home.join(".local/share/claude/versions");
    std::fs::create_dir_all(&versions).unwrap();
    write_executable(
        &versions.join("2.0.0"),
        &format!(
            r#"#!/bin/sh
{{ cargo --version && echo 'WORKS cargo'
  git --version && echo 'WORKS git'
  git config user.name | grep -q fixture && echo 'WORKS gitconfig'
}} > "$HOME/report" 2>&1
exec '{}' fake-cli claude "$@"
"#,
            crew.display()
        ),
    );
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    symlink(versions.join("2.0.0"), home.join(".local/bin/claude")).unwrap();
    let dir_of = |program: &str| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .find(|d| d.join(program).is_file())
            .unwrap()
    };
    let path = std::env::join_paths([
        home.join(".local/bin"),
        home.join(".cargo/bin"),
        dir_of("git"),
        dir_of("bwrap"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();

    let team = sandboxed(
        &s,
        vec![AgentSpec {
            id: "a".into(),
            role: "implementation".into(),
            runtime: Some(crew_core::RuntimeKind::Claude),
            ..AgentSpec::default()
        }],
        vec![],
    );
    std::fs::write(s.root().join("team.yaml"), team.to_yaml().unwrap()).unwrap();
    // Started by the copy, so that the daemon's crew binary is the one in
    // the checkout.
    let base = s.command();
    let mut cmd = Command::new(&crew);
    cmd.current_dir(s.root());
    for (name, value) in base.get_envs() {
        match value {
            Some(value) => cmd.env(name, value),
            None => cmd.env_remove(name),
        };
    }
    let out = cmd
        .env("HOME", &home)
        .env("CARGO_HOME", home.join(".cargo"))
        .env("RUSTUP_HOME", home.join(".rustup"))
        .env("PATH", &path)
        .args(["run", "team.yaml", "--no-attach"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    let agent_home = home_of(&s, "a");
    s.wait_for("the CLI to start", || {
        agent_home.join("claude/fake-claude.json").is_file()
    });
    let report = std::fs::read_to_string(agent_home.join("report")).unwrap();
    let works: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("WORKS "))
        .collect();
    assert_eq!(works, ["cargo", "git", "gitconfig"], "{report}");
    s.stop();
}

/// Two flat installations of `codex` and `git` (each program a file directly
/// in its directory, the case where the profile leaves that directory out of
/// the sandbox), beside the crew binary: `first/` and `second/`. Each notes
/// which installation ran: `codex` in `$CODEX_HOME/ran`, `git` (when run for
/// an agent) in `<A>/rw/git-ran`.
fn two_installations() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let base = dir.path().canonicalize().unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let git = std::env::split_paths(&path)
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    for which in ["first", "second"] {
        let bin = base.join(which);
        std::fs::create_dir_all(&bin).unwrap();
        let codex = format!(
            "#!/bin/sh\necho \"{which} $1\" >> \"$CODEX_HOME/ran\"\nexec '{CREW}' fake-cli codex \"$@\"\n"
        );
        let git = format!(
            "#!/bin/sh\n[ -n \"$CREW_IDENTITY\" ] && echo {which} >> \"$(dirname \"$CREW_IDENTITY\")/rw/git-ran\"\nexec '{}' \"$@\"\n",
            git.display()
        );
        for (name, script) in [("codex", codex), ("git", git)] {
            write_executable(&bin.join(name), &script);
        }
    }
    let mut dirs = vec![base.join("first"), base.join("second")];
    dirs.extend(std::env::split_paths(&path));
    let path = std::env::join_paths(dirs).unwrap().into_string().unwrap();
    (dir, path)
}

#[test]
fn each_sandboxed_launch_runs_the_cli_the_daemon_found_not_another() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    workspace_repo(&s, &[]);
    let (_installations, path) = two_installations();
    let team = sandboxed(
        &s,
        vec![
            codex_agent("x"),
            agent(
                "w",
                "",
                &[
                    "--on-task=write work.txt done",
                    "--on-task=commit add work",
                    "--on-task=done finished",
                ],
            ),
        ],
        vec![task("t", "w", &[])],
    );
    run_with_env(&s, &team, &[("PATH", &path)]);
    wait_up(&s, "x");
    let ran = home_of(&s, "x").join("codex/ran");

    // The session and its app-server, then a queue command.
    s.wait_for("the fake app-server", || {
        !processes_with(&["fake-cli", "codex", "app-server"]).is_empty()
    });
    let identity = s.workspace.join(".crew/agents/x/identity.json");
    let out = s
        .command()
        .args(["hook", "codex-notify", "--identity"])
        .arg(&identity)
        .arg(r#"{"type":"agent-turn-complete","thread-id":"fake-thread"}"#)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::stderr(&out));
    s.wait("the session id", |snap| {
        find_agent(snap, "x").session_id.as_deref() == Some("fake-thread")
    });
    s.send("x", "through the queue");
    s.wait_for("the queue command", || {
        std::fs::read_to_string(&ran).is_ok_and(|r| r.lines().any(|l| l.ends_with(" queue")))
    });
    // The worker's commit: its git, in its sandbox.
    let done = s.wait("the task", |snap| {
        matches!(
            find_task(snap, "t").status,
            TaskStatus::Done | TaskStatus::Failed
        )
    });
    assert_eq!(find_task(&done, "t").status, TaskStatus::Done);
    let git_ran = s.workspace.join(".crew/agents/w/rw/git-ran");
    let codex = std::fs::read_to_string(&ran).unwrap();
    let git = std::fs::read_to_string(&git_ran).unwrap_or_default();
    s.stop();

    assert!(
        codex.lines().any(|l| l.ends_with(" app-server")),
        "no app-server: {codex}"
    );
    assert!(
        codex.lines().count() >= 3,
        "session, app-server, queue: {codex}"
    );
    let other_codex: Vec<&str> = codex.lines().filter(|l| !l.starts_with("first ")).collect();
    let other_git: Vec<&str> = git.lines().filter(|l| *l != "first").collect();
    assert!(!git.is_empty(), "the worker's git left no note");
    assert!(
        other_codex.is_empty() && other_git.is_empty(),
        "launches ran another program than the one found first: codex {other_codex:?}, git \
{other_git:?}"
    );
}

#[test]
fn a_restart_writes_the_agents_next_identity_only_after_its_git_job_ends() {
    if require_sandbox().is_none() {
        return;
    }
    let s = Session::new();
    // A pre-commit hook that runs for a while, in the agent's git sandbox,
    // which mounts the agent's directory.
    let hook = "#!/bin/sh\necho > \"$HOME/hook-start\"\nsleep 3\necho > \"$HOME/hook-end\"\n";
    std::fs::create_dir_all(s.workspace.join(".githooks")).unwrap();
    write_executable(&s.workspace.join(".githooks/pre-commit"), hook);
    workspace_repo(&s, &[]);
    repo_config(&s, "core.hooksPath", ".githooks");
    let team = sandboxed(
        &s,
        vec![agent(
            "w",
            "",
            &[
                "--on-task=write work.txt done",
                "--on-task=commit add work",
                "--on-task=done finished",
            ],
        )],
        vec![task("t", "w", &[])],
    );
    s.run(&team);
    let home = home_of(&s, "w");
    let identity = s.workspace.join(".crew/agents/w/identity.json");
    s.wait_for("the hook to start", || home.join("hook-start").exists());
    let before = std::fs::metadata(&identity).unwrap().modified().unwrap();
    // Restarted while its commit's hook runs: the next identity is written
    // once the job (and its sandbox) is done, not beside it.
    s.agent_cmd(&["restart", "w"]);
    s.wait_for("the hook to end", || home.join("hook-end").exists());
    s.wait_for("the next identity", || {
        std::fs::metadata(&identity)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m > before)
    });
    let written = std::fs::metadata(&identity).unwrap().modified().unwrap();
    let ended = std::fs::metadata(home.join("hook-end"))
        .unwrap()
        .modified()
        .unwrap();
    s.stop();
    assert!(
        written >= ended,
        "the next identity was written while the agent's git hook still ran ({:?} before \
its end)",
        ended.duration_since(written).unwrap_or_default()
    );
}
