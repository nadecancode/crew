//! The sandbox launch path end to end, through the real `crew` binary:
//! `crew sandbox-exec` (the shim) and, with bubblewrap, `crew sandbox-run`.
//!
//! Tests that need a host feature (Landlock, bwrap, user namespaces) are
//! skipped with a printed reason when it is missing, and fail instead under
//! `CREW_TEST_SANDBOX=require`.

use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output};

const CREW: &str = env!("CARGO_BIN_EXE_crew");

fn host_has(available: bool, what: &str) -> bool {
    if available {
        return true;
    }
    if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
        panic!("CREW_TEST_SANDBOX=require but the host lacks {what}");
    }
    eprintln!("skipped: the host lacks {what}");
    false
}

fn landlock_scopes() -> bool {
    crew::sandbox::landlock::abi().is_ok_and(|abi| abi >= crew::sandbox::landlock::MIN_ABI)
}

fn shim(extra: &[&str], script: &str) -> Command {
    let mut cmd = Command::new(CREW);
    cmd.arg("sandbox-exec")
        .args(extra)
        .args(["--", "sh", "-c", script]);
    cmd
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_shim_runs_the_program_once_confined() {
    if !host_has(landlock_scopes(), "Landlock ABI 9") {
        return;
    }
    let out = shim(&[], "echo confined").output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "confined\n");
}

#[test]
fn the_shim_fails_closed_before_the_program_runs() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("ran");
    let script = format!("echo ran > '{}'", sentinel.display());
    let out = shim(&["--min-landlock-abi", "999"], &script)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(
        !sentinel.exists(),
        "the program ran although Landlock failed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("crew sandbox-exec: "), "{stderr}");
    assert!(stderr.contains("Landlock"), "{stderr}");
}

#[test]
fn the_shim_closes_inherited_descriptors() {
    if !host_has(landlock_scopes(), "Landlock ABI 9") {
        return;
    }
    let sentinel = tempfile::NamedTempFile::new().unwrap();
    let held = std::fs::File::open(sentinel.path()).unwrap();
    let with_fd_50 = |mut cmd: Command| {
        use std::os::fd::AsRawFd;
        let fd = held.as_raw_fd();
        // SAFETY: dup2 is async-signal-safe; the duplicate has no CLOEXEC,
        // so the program inherits it unless something closes it.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(fd, 50) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.output().unwrap()
    };
    let script = "test -e /proc/$$/fd/50 && echo inherited || echo closed";
    let mut direct = Command::new("sh");
    direct.args(["-c", script]);
    let control = with_fd_50(direct);
    assert_eq!(
        String::from_utf8_lossy(&control.stdout),
        "inherited\n",
        "{}",
        text(&control)
    );
    let out = with_fd_50(shim(&[], script));
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "closed\n");
}

#[test]
fn the_shim_keeps_the_program_from_signalling_outside() {
    if !host_has(landlock_scopes(), "Landlock ABI 9") {
        return;
    }
    let script = format!("kill -0 {} && echo reached", std::process::id());
    let control = Command::new("sh").args(["-c", &script]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&control.stdout), "reached\n");
    let out = shim(&[], &script).output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

// ---------------------------------------------------------------------
// `crew sandbox-run` with real bubblewrap.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crew::sandbox::profile::{build, Extra, Host, Request, Role, Session};
use crew::sandbox::run::prefix;
use crew::sandbox::spec::{launcher_env, Spec};

/// bubblewrap on PATH, new enough, with working user namespaces, and a
/// Landlock that scopes: what every test below needs.
fn bwrap() -> Option<(PathBuf, u32)> {
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("bwrap"))
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok());
    let abi = crew::sandbox::landlock::abi().unwrap_or(0);
    let usable = found.as_ref().is_some_and(|bwrap| {
        Command::new(bwrap)
            .args(["--unshare-user", "--ro-bind", "/", "/", "true"])
            .status()
            .is_ok_and(|s| s.success())
    });
    if !host_has(
        usable && abi >= crew::sandbox::landlock::MIN_ABI,
        "bubblewrap with user namespaces and Landlock ABI 9",
    ) {
        return None;
    }
    Some((found.unwrap(), abi))
}

/// A session laid out the way the daemon lays it out, with a daemon
/// socket that records who connects.
struct Layout {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ws: PathBuf,
    git: PathBuf,
    run: PathBuf,
    socket: PathBuf,
    cargo: PathBuf,
    connected: mpsc::Receiver<String>,
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
        ws.join(".crew/agents/b/rw"),
        ws.join(".crew/worktrees/a"),
        ws.join(".crew/worktrees/b"),
        ws.join(".crew/worktrees/_integration"),
        ws.join("src"),
        run.join("0123/a"),
        cargo.clone(),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    for id in ["a", "b", "_integration"] {
        let admin = git.join("worktrees").join(id);
        std::fs::create_dir_all(&admin).unwrap();
        for f in ["commondir", "gitdir", "config.worktree"] {
            std::fs::write(admin.join(f), "").unwrap();
        }
    }
    let socket = run.join("0123.sock");
    for (path, text) in [
        (ws.join(".crew/crew.db"), "db".to_string()),
        (ws.join(".crew/agents/b/identity.json"), "b".to_string()),
        (ws.join("secret.txt"), "secret".to_string()),
        (
            crew_core::paths::operator_token_path(&socket),
            "operator-token".to_string(),
        ),
        (
            ws.join(".crew/agents/a/identity.json"),
            format!(
                r#"{{"socket":"{}","agent_id":"a","token":"agent-token"}}"#,
                socket.display()
            ),
        ),
    ] {
        std::fs::write(path, text).unwrap();
    }
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, connected) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(stream).read_line(&mut line);
            let _ = tx.send(line);
        }
    });
    Layout {
        _dir: dir,
        root,
        ws,
        git,
        run,
        socket,
        cargo,
        connected,
    }
}

impl Layout {
    fn session(&self) -> Session<'_> {
        Session {
            workspace: &self.ws,
            git_dir: &self.git,
            runtime_dir: &self.run,
            daemon_socket: &self.socket,
            cargo_home: &self.cargo,
        }
    }

    fn agent(&self) -> PathBuf {
        self.ws.join(".crew/agents/a")
    }

    fn worker_spec(&self, host: &Host, terminal: bool) -> Spec {
        let agent = self.agent();
        let worktree = self.ws.join(".crew/worktrees/a");
        let admin = self.git.join("worktrees/a");
        let sidecar = self.run.join("0123/a");
        let request = Request {
            session: self.session(),
            role: Role::Worker {
                agent_dir: &agent,
                worktree: &worktree,
                admin: &admin,
                sidecar_dir: Some(&sidecar),
            },
            extra: &Extra::default(),
            cli: None,
            pass_env: &["CREW_IDENTITY".into(), "ANTHROPIC_API_KEY".into()],
            terminal,
            nix_daemon: None,
        };
        build(host, &request).unwrap().spec
    }

    fn launch_env(&self) -> Vec<(String, String)> {
        vec![
            (
                "CREW_IDENTITY".into(),
                self.agent().join("identity.json").display().to_string(),
            ),
            ("ANTHROPIC_API_KEY".into(), "sk-test-secret-value".into()),
        ]
    }
}

fn host(bwrap: &Path, abi: u32) -> Host {
    let crew = Path::new(CREW).canonicalize().unwrap();
    Host::detect(&crew, bwrap, abi)
}

/// `crew sandbox-run` for `script`, started the way the daemon must start
/// it: argument prefix from the spec, environment exactly the launcher's.
fn sandboxed(spec: &Spec, launch: &[(String, String)], script: &str) -> Command {
    let argv = prefix(spec).unwrap();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .args(["sh", "-c", script])
        .env_clear()
        .envs(launcher_env(spec, std::env::vars_os(), launch))
        .stdin(std::process::Stdio::null());
    cmd
}

fn lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_worker_sees_its_own_tree_and_nothing_of_the_operators() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let host = host(&bwrap, abi);
    let spec = l.worker_spec(&host, false);
    let agent = l.agent();
    let script = format!(
        r#"
        echo "cwd=$(pwd)"
        echo "home=$HOME"
        for p in '{token}' '{db}' '{other}' '{secret}' '{src}' '{run_dir}/0123/b'; do
            [ -e "$p" ] && echo "VISIBLE $p"
        done
        cat '{agent}/identity.json' >/dev/null && echo identity-readable
        ( echo x > '{agent}/planted' ) 2>/dev/null && echo AGENT-DIR-WRITABLE
        echo x > '{agent}/rw/note' && echo rw-writable
        echo x > ./work.txt && echo worktree-writable
        ( echo x > '{git}/config.worktree.new' ) 2>/dev/null && echo GIT-DIR-WRITABLE
        ( echo x > '{git}/worktrees/a/config.worktree' ) 2>/dev/null && echo CONFIG-WORKTREE-WRITABLE
        [ -S '{socket}' ] && echo daemon-socket
        echo "key=$ANTHROPIC_API_KEY"
        echo "dbus=${{DBUS_SESSION_BUS_ADDRESS:-none}}"
        # find trusts readdir's type, which hides sockets bind-mounted over a
        # file; mount points are listed from mountinfo. /nix/store is left out:
        # walking it takes minutes, and the Landlock socket rule guards it.
        {{ find / -path /proc -prune -o -path /nix/store -prune -o -type s -print
          while read -r _ _ _ _ point _; do [ -S "$point" ] && echo "$point"; done < /proc/self/mountinfo
        }} 2>/dev/null | sort -u | sed 's/^/socket /'
        "#,
        token = crew_core::paths::operator_token_path(&l.socket).display(),
        db = l.ws.join(".crew/crew.db").display(),
        other = l.ws.join(".crew/agents/b/identity.json").display(),
        secret = l.ws.join("secret.txt").display(),
        src = l.ws.join("src").display(),
        run_dir = l.run.display(),
        agent = agent.display(),
        git = l.git.display(),
        socket = l.socket.display(),
    );
    let out = sandboxed(&spec, &l.launch_env(), &script)
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let lines = lines(&out);
    let expected = [
        format!("cwd={}", l.ws.join(".crew/worktrees/a").display()),
        format!("home={}", agent.join("rw/home").display()),
        "identity-readable".to_string(),
        "rw-writable".to_string(),
        "worktree-writable".to_string(),
        "daemon-socket".to_string(),
        "key=sk-test-secret-value".to_string(),
        "dbus=none".to_string(),
        format!("socket {}", l.socket.display()),
    ];
    assert_eq!(lines, expected, "{}", text(&out));
    assert!(l.ws.join(".crew/worktrees/a/work.txt").exists());
    assert!(!agent.join("planted").exists());
}

#[test]
fn a_sandboxed_agent_reaches_the_daemon_socket() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = l.worker_spec(&host(&bwrap, abi), false);
    let out = sandboxed(&spec, &l.launch_env(), &format!("'{CREW}' ctl list"))
        .output()
        .unwrap();
    let hello = l
        .connected
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("nothing connected; {}", text(&out)));
    assert!(hello.contains("agent-token"), "{hello}");
}

#[test]
fn a_helper_gets_no_identity_credentials_socket_or_network() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let host = host(&bwrap, abi);
    let worktree = l.ws.join(".crew/worktrees/_integration");
    let admin = l.git.join("worktrees/_integration");
    let request = Request {
        session: l.session(),
        role: Role::Helper {
            worktree: &worktree,
            admin: &admin,
        },
        extra: &Extra::default(),
        cli: None,
        pass_env: &[],
        terminal: false,
        nix_daemon: None,
    };
    let spec = build(&host, &request).unwrap().spec;
    let script = format!(
        r#"
        echo "identity=${{CREW_IDENTITY:-none}} key=${{ANTHROPIC_API_KEY:-none}}"
        [ -e '{socket}' ] && echo SOCKET
        [ -e '{agents}' ] && echo AGENTS
        grep -v -e Inter -e face -e ' lo:' /proc/net/dev | sed 's/^/IFACE /'
        echo done
        "#,
        socket = l.socket.display(),
        agents = l.ws.join(".crew/agents").display(),
    );
    // The daemon passes nothing, but even if it did, the spec lets nothing through.
    let out = sandboxed(&spec, &l.launch_env(), &script).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        lines(&out),
        ["identity=none key=none", "done"],
        "{}",
        text(&out)
    );
}

#[test]
fn a_source_changed_after_the_spec_was_built_stops_the_launch() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = l.worker_spec(&host(&bwrap, abi), false);
    let rw = l.agent().join("rw");
    std::fs::rename(&rw, l.root.join("moved-rw")).unwrap();
    std::fs::create_dir(&rw).unwrap();
    let sentinel = l.root.join("ran");
    let out = sandboxed(
        &spec,
        &l.launch_env(),
        &format!("echo ran > '{}'", sentinel.display()),
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("changed after it was checked"));
    assert!(!sentinel.exists());
}

#[test]
fn a_tampered_identity_or_kind_for_a_system_tree_fails_closed() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = l.worker_spec(&host(&bwrap, abi), false);
    // /bin and /usr/bin are envfs (FUSE) on NixOS: tamper with whichever
    // system tree is bound first.
    let json = serde_json::to_value(&spec).unwrap();
    let first_system = json["mounts"]
        .as_array()
        .unwrap()
        .iter()
        .position(|m| m["kind"] == "bind")
        .unwrap();
    for (field, value) in [
        ("ino", serde_json::json!(1)),
        ("kind", serde_json::json!("file")),
    ] {
        let mut tampered = json.clone();
        tampered["mounts"][first_system]["id"][field] = value;
        let tampered: Spec = serde_json::from_value(tampered).unwrap();
        let sentinel = l.root.join(format!("ran-{field}"));
        let out = sandboxed(
            &tampered,
            &l.launch_env(),
            &format!("echo ran > '{}'", sentinel.display()),
        )
        .output()
        .unwrap();
        assert_eq!(out.status.code(), Some(125), "{field}: {}", text(&out));
        assert!(!sentinel.exists());
    }
}

#[test]
fn no_secret_reaches_a_command_line_and_no_descriptor_leaks_in() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = l.worker_spec(&host(&bwrap, abi), false);
    let secret = "sk-test-secret-value";
    for arg in prefix(&spec).unwrap() {
        assert!(!arg.to_string_lossy().contains(secret));
    }
    let held = std::fs::File::open(l.ws.join("secret.txt")).unwrap();
    let mut cmd = sandboxed(
        &spec,
        &l.launch_env(),
        "test -e /proc/self/fd/50 && echo LEAKED; sleep 5",
    );
    {
        use std::os::fd::AsRawFd;
        let fd = held.as_raw_fd();
        // SAFETY: dup2 is async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(fd, 50) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd.stdout(std::process::Stdio::piped()).spawn().unwrap();
    // sandbox-run starts bwrap as its child and waits for it.
    let run_pid = child.id();
    let fds_of = |pid: u32| -> Vec<OsString> {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .map(|dir| dir.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default()
    };
    let mut seen_bwrap = false;
    for _ in 0..50 {
        let Some(pid) = children(run_pid).into_iter().next() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline.starts_with(bwrap.as_os_str().as_encoded_bytes()) {
            seen_bwrap = true;
            assert!(!String::from_utf8_lossy(&cmdline).contains(secret));
            let fds = fds_of(pid);
            assert!(
                !fds.contains(&OsString::from("50")),
                "bwrap inherited fd 50: {fds:?}"
            );
            // sandbox-run itself: no secret in its command line, and only
            // stdio left while it waits (pinned, mask and inherited
            // descriptors all closed).
            let own = std::fs::read(format!("/proc/{run_pid}/cmdline")).unwrap();
            assert!(!String::from_utf8_lossy(&own).contains(secret));
            // It closes them right after starting bubblewrap; the program
            // (sleep 5) is still running when that has happened.
            let until = std::time::Instant::now() + READY_WAIT;
            loop {
                let mut own_fds = fds_of(run_pid);
                own_fds.sort();
                if own_fds == ["0", "1", "2"] {
                    break;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "after {READY_WAIT:?}, sandbox-run still holds more than stdio: {own_fds:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    assert!(seen_bwrap, "never saw bwrap running");
    assert!(out.status.success(), "{}", text(&out));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("LEAKED"));
}

#[test]
fn the_pty_path_runs_the_program_confined_on_its_terminal() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = l.worker_spec(&host(&bwrap, abi), true);
    let argv = prefix(&spec).unwrap();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = portable_pty::CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.args([
        "sh",
        "-c",
        "test -t 0 && echo on-a-tty; echo pid=$$; echo key=$ANTHROPIC_API_KEY",
    ]);
    cmd.env_clear();
    for (name, value) in launcher_env(&spec, std::env::vars_os(), &l.launch_env()) {
        cmd.env(name, value);
    }
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut output = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "{output}");
    assert!(output.contains("on-a-tty"), "{output}");
    // A fresh PID namespace: the shell is a low pid there.
    let pid: u32 = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("{output}"));
    assert!(pid < 10, "{output}");
    assert!(output.contains("key=sk-test-secret-value"), "{output}");
}

#[test]
fn a_socket_in_a_bound_tree_is_masked() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let extra_dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let extra = extra_dir.path().canonicalize().unwrap();
    let _host_socket = UnixListener::bind(extra.join("host.sock")).unwrap();
    std::fs::write(extra.join("readable"), "ok").unwrap();
    let host = host(&bwrap, abi);
    let agent = l.agent();
    let worktree = l.ws.join(".crew/worktrees/a");
    let admin = l.git.join("worktrees/a");
    let extra_mounts = Extra {
        read_only: vec![extra.clone()],
        ..Extra::default()
    };
    let request = Request {
        session: l.session(),
        role: Role::Worker {
            agent_dir: &agent,
            worktree: &worktree,
            admin: &admin,
            sidecar_dir: None,
        },
        extra: &extra_mounts,
        cli: None,
        pass_env: &[],
        terminal: false,
        nix_daemon: None,
    };
    let spec = build(&host, &request).unwrap().spec;
    let script = format!(
        "cat '{d}/readable'; echo; [ -S '{d}/host.sock' ] && echo SOCKET || echo masked",
        d = extra.display()
    );
    let out = sandboxed(&spec, &[], &script).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(lines(&out), ["ok", "masked"], "{}", text(&out));
}

#[test]
fn resolv_conf_into_run_shows_only_the_target_file() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let resolve = l.root.join("fake-run/systemd/resolve");
    std::fs::create_dir_all(&resolve).unwrap();
    std::fs::write(resolve.join("stub-resolv.conf"), "nameserver 127.0.0.53\n").unwrap();
    std::fs::write(resolve.join("sentinel"), "hidden").unwrap();
    let _neighbour = UnixListener::bind(resolve.join("io.systemd.Resolve")).unwrap();
    std::fs::create_dir_all(l.root.join("fake-etc")).unwrap();
    std::os::unix::fs::symlink(
        "../fake-run/systemd/resolve/stub-resolv.conf",
        l.root.join("fake-etc/resolv.conf"),
    )
    .unwrap();
    let mut host = host(&bwrap, abi);
    host.resolv_conf = l.root.join("fake-etc/resolv.conf");
    let spec = l.worker_spec(&host, false);
    let script = format!(
        "cat '{etc}/resolv.conf'; ls -A '{resolve}'",
        etc = l.root.join("fake-etc").display(),
        resolve = resolve.display()
    );
    let out = sandboxed(&spec, &[], &script).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        lines(&out),
        ["nameserver 127.0.0.53", "stub-resolv.conf"],
        "{}",
        text(&out)
    );
}

#[test]
fn the_probe_finds_this_host_able_to_run_sandboxes() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let crew = Path::new(CREW).canonicalize().unwrap();
    let available = crew::sandbox::probe::probe(&crew, std::env::var_os("PATH").as_ref()).unwrap();
    assert_eq!(available.bwrap, bwrap);
    assert_eq!(available.landlock_abi, abi);
    assert!(available.bwrap_version >= crew::sandbox::probe::MIN_BWRAP);
}

#[test]
fn a_writable_entry_cannot_reach_a_read_only_toolchain_but_data_stays_writable() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    // Outside the protected set (not under /tmp): next to the crew binary.
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let root = dir.path().canonicalize().unwrap();
    let toolchain = root.join("rustup");
    let data = root.join("data");
    std::fs::create_dir_all(toolchain.join("bin")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(toolchain.join("bin/operator-tool"), "trusted").unwrap();
    std::os::unix::fs::symlink(&toolchain, root.join("alias")).unwrap();
    let mut host = host(&bwrap, abi);
    host.rustup_home = toolchain.clone();
    let agent = l.agent();
    let worktree = l.ws.join(".crew/worktrees/a");
    let admin = l.git.join("worktrees/a");
    let build_with = |extra: &Extra| {
        let request = Request {
            session: l.session(),
            role: Role::Worker {
                agent_dir: &agent,
                worktree: &worktree,
                admin: &admin,
                sidecar_dir: None,
            },
            extra,
            cli: None,
            pass_env: &[],
            terminal: false,
            nix_daemon: None,
        };
        build(&host, &request)
    };
    for bad in [
        toolchain.join("bin/operator-tool"),
        root.join("alias/bin/operator-tool"),
        root.clone(),
    ] {
        let extra = Extra {
            read_write: vec![bad.clone()],
            ..Extra::default()
        };
        assert!(build_with(&extra).is_err(), "{bad:?} was accepted");
    }
    let extra = Extra {
        read_write: vec![data.clone()],
        ..Extra::default()
    };
    let spec = build_with(&extra).unwrap().spec;
    let script = format!(
        "( echo evil > '{tool}' ) 2>/dev/null && echo TOOL-WRITTEN; \
         cat '{tool}'; echo; echo ok > '{data}/out' && echo data-written",
        tool = toolchain.join("bin/operator-tool").display(),
        data = data.display(),
    );
    let out = sandboxed(&spec, &[], &script).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(lines(&out), ["trusted", "data-written"], "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(toolchain.join("bin/operator-tool")).unwrap(),
        "trusted"
    );
    assert_eq!(std::fs::read_to_string(data.join("out")).unwrap(), "ok\n");
}

#[test]
fn a_socket_made_in_a_mounted_tree_after_launch_is_refused() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    // A read-only tree outside the protected set, empty at launch.
    let dir = tempfile::tempdir_in(Path::new(CREW).parent().unwrap()).unwrap();
    let tree = dir.path().canonicalize().unwrap();
    let socket = tree.join("late.sock");
    let identity = tree.join("identity.json");
    std::fs::write(
        &identity,
        format!(
            r#"{{"socket":"{}","agent_id":"a","token":"t"}}"#,
            socket.display()
        ),
    )
    .unwrap();
    let host = host(&bwrap, abi);
    let agent = l.agent();
    let worktree = l.ws.join(".crew/worktrees/a");
    let admin = l.git.join("worktrees/a");
    let extra = Extra {
        read_only: vec![tree.clone()],
        ..Extra::default()
    };
    let request = Request {
        session: l.session(),
        role: Role::Worker {
            agent_dir: &agent,
            worktree: &worktree,
            admin: &admin,
            sidecar_dir: None,
        },
        extra: &extra,
        cli: None,
        pass_env: &[],
        terminal: false,
        nix_daemon: None,
    };
    let spec = build(&host, &request).unwrap().spec;
    assert!(!spec.mounts.iter().any(
        |m| matches!(m, crew::sandbox::spec::Mount::Mask { path } if path.starts_with(&tree))
    ));
    // The agent waits for the socket, then tries it with `crew ctl`, which
    // connects to the socket its identity file names.
    let script = format!(
        "while [ ! -S '{sock}' ]; do sleep 0.05; done; echo started; \
         CREW_IDENTITY='{id}' '{CREW}' ctl list 2>&1 | head -3; echo done",
        sock = socket.display(),
        id = identity.display(),
    );
    let child = sandboxed(&spec, &[], &script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("started\n"), "{}", text(&out));
    assert!(stdout.contains("Permission denied"), "{}", text(&out));
    assert!(
        listener.accept().is_err(),
        "the agent reached the host socket"
    );
    // Control: the same client outside the sandbox reaches it.
    let control = Command::new(CREW)
        .args(["ctl", "list"])
        .env("CREW_IDENTITY", &identity)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut reached = false;
    for _ in 0..100 {
        if listener.accept().is_ok() {
            reached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(listener);
    let _ = control.wait_with_output();
    assert!(reached, "the control client did not reach the socket");
}

// ---------------------------------------------------------------------
// When `crew sandbox-run` has exited with a code, the whole sandbox is gone.

/// A process by pid and start time, so a recycled pid is not mistaken for it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Proc {
    pid: u32,
    start: u64,
}

fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22, counted after the parenthesised command name.
    let after = &stat[stat.rfind(')')? + 2..];
    after.split(' ').nth(19)?.parse().ok()
}

fn alive(p: Proc) -> bool {
    start_time(p.pid) == Some(p.start)
}

fn children(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// How long a test waits for a sandbox to reach a state it is about to
/// observe. Reaching it is the condition; the time is only a bound.
const READY_WAIT: Duration = Duration::from_secs(20);

/// Every process below `root`, now.
fn descendants(root: u32) -> Vec<Proc> {
    let mut found = Vec::new();
    let mut pending = children(root);
    while let Some(pid) = pending.pop() {
        pending.extend(children(pid));
        if let Some(start) = start_time(pid) {
            found.push(Proc { pid, start });
        }
    }
    found
}

fn cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect()
}

/// Wait until a process below `root` runs exactly `argv`, then return that
/// process and every process below `root` at that moment (it included).
fn once_running(root: u32, argv: &[&str]) -> (Proc, Vec<Proc>) {
    let until = std::time::Instant::now() + READY_WAIT;
    loop {
        let found = descendants(root);
        if let Some(payload) = found.iter().find(|p| cmdline(p.pid) == argv).copied() {
            return (payload, found);
        }
        assert!(
            std::time::Instant::now() < until,
            "after {READY_WAIT:?}, no process below {root} runs {argv:?}; saw {:?}",
            found
                .iter()
                .map(|p| (p.pid, cmdline(p.pid)))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until none of `procs` is running.
fn wait_all_gone(procs: &[Proc], what: &str) {
    let until = std::time::Instant::now() + READY_WAIT;
    while procs.iter().any(|p| alive(*p)) {
        assert!(
            std::time::Instant::now() < until,
            "after {READY_WAIT:?}, still running {what}: {:?}",
            procs.iter().filter(|p| alive(**p)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A FIFO in the agent's worktree, which the worker sandbox mounts
/// writable at the same path: the program blocks reading it until the test
/// releases it.
struct Gate(PathBuf);

impl Gate {
    fn new(l: &Layout, name: &str) -> Gate {
        let path = l.ws.join(".crew/worktrees/a").join(name);
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid C string; the result is checked.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        Gate(path)
    }

    /// The shell command that waits at the gate.
    fn wait(&self) -> String {
        format!("read _ < '{}'", self.0.display())
    }

    /// Let the program waiting at the gate go on.
    fn release(&self) {
        let until = std::time::Instant::now() + READY_WAIT;
        let file = loop {
            // Non-blocking, so that a program that never reaches the gate
            // fails the test instead of hanging it (ENXIO: no reader yet).
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.0)
            {
                Ok(file) => break file,
                Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                    assert!(
                        std::time::Instant::now() < until,
                        "after {READY_WAIT:?}, nothing waits at the gate {}",
                        self.0.display()
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("opening the gate {}: {e}", self.0.display()),
            }
        };
        std::io::Write::write_all(&mut &file, b"go\n").unwrap();
    }
}

fn worker_spec_for_lifetime(bwrap: &Path, abi: u32, l: &Layout) -> Spec {
    l.worker_spec(&host(bwrap, abi), false)
}

#[test]
fn when_sandbox_run_returns_a_code_every_sandbox_process_is_gone() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    // A descendant in its own session outlives the program, which exits
    // only once the test has seen that descendant running.
    let gate = Gate::new(&l, "gate");
    let script = format!(
        "setsid sleep 30 </dev/null >/dev/null 2>&1 & {}; exit 7",
        gate.wait()
    );
    let mut child = sandboxed(&spec, &[], &script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (descendant, seen) = once_running(child.id(), &["sleep", "30"]);
    assert!(seen.contains(&descendant));
    assert!(seen.len() >= 3, "saw too little of the sandbox: {seen:?}");
    gate.release();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(7));
    let left: Vec<Proc> = seen.into_iter().filter(|p| alive(*p)).collect();
    assert!(
        left.is_empty(),
        "still running after sandbox-run exited: {left:?}"
    );
}

#[test]
fn a_killed_bubblewrap_is_reported_as_its_signal_once_the_sandbox_is_gone() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    let mut child = sandboxed(&spec, &[], "sleep 30")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (_, seen) = once_running(child.id(), &["sleep", "30"]);
    // The bubblewrap sandbox-run started: its direct child.
    let direct = children(child.id());
    let bwrap_proc = seen
        .iter()
        .find(|p| direct.contains(&p.pid))
        .copied()
        .unwrap_or_else(|| panic!("no child of sandbox-run: {seen:?}"));
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", bwrap_proc.pid)).unwrap();
    assert_eq!(comm.trim(), "bwrap");
    // SAFETY: plain kill of a process this test started.
    unsafe { libc::kill(bwrap_proc.pid as i32, libc::SIGKILL) };
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(128 + libc::SIGKILL));
    let left: Vec<Proc> = seen.into_iter().filter(|p| alive(*p)).collect();
    assert!(left.is_empty(), "still running: {left:?}");
}

#[test]
fn a_killed_sandbox_run_takes_the_sandbox_with_it() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    let mut child = sandboxed(&spec, &[], "sleep 30")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (_, seen) = once_running(child.id(), &["sleep", "30"]);
    child.kill().unwrap();
    child.wait().unwrap();
    wait_all_gone(&seen, "after sandbox-run was killed");
}

#[test]
fn sandbox_run_dies_with_the_process_that_started_it() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    let argv = prefix(&spec).unwrap();
    // A parent that starts sandbox-run, prints its pid and waits.
    let mut parent = Command::new("/bin/sh");
    parent
        .args(["-c", r#""$@" & echo $!; wait"#, "sh"])
        .args(&argv)
        .args(["sh", "-c", "sleep 30"])
        .env_clear()
        .envs(launcher_env(&spec, std::env::vars_os(), &[]))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped());
    let mut parent = parent.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(parent.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let run_pid: u32 = line.trim().parse().unwrap();
    let run = Proc {
        pid: run_pid,
        start: start_time(run_pid).unwrap(),
    };
    let (_, mut seen) = once_running(run_pid, &["sleep", "30"]);
    seen.push(run);
    parent.kill().unwrap();
    parent.wait().unwrap();
    wait_all_gone(&seen, "after the parent of sandbox-run died");
}

// The marker that binds "the sandbox is gone" to one launch.

fn marker_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn the_done_marker_appears_only_once_the_sandbox_is_gone() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let private = marker_dir();
    let done = private.path().canonicalize().unwrap().join("launch-1.done");
    let mut spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    spec.done = Some(done.clone());
    let gate = Gate::new(&l, "gate");
    let script = format!(
        "setsid sleep 30 </dev/null >/dev/null 2>&1 & {}; exit 7",
        gate.wait()
    );
    let mut child = sandboxed(&spec, &[], &script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (descendant, seen) = once_running(child.id(), &["sleep", "30"]);
    assert!(seen.contains(&descendant));
    assert!(!done.exists(), "the marker exists while the sandbox runs");
    gate.release();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(7));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "exited 7\n");
    let mode = std::fs::metadata(&done).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );
    assert!(seen.iter().all(|p| !alive(*p)));
}

#[test]
fn the_done_marker_names_the_signal_that_killed_bubblewrap() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let private = marker_dir();
    let done = private.path().canonicalize().unwrap().join("launch-2.done");
    let mut spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    spec.done = Some(done.clone());
    let mut child = sandboxed(&spec, &[], "sleep 30")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    once_running(child.id(), &["sleep", "30"]);
    let bwrap_pid = children(child.id())
        .into_iter()
        .next()
        .expect("bubblewrap is sandbox-run's child");
    // SAFETY: plain kill of a process this test started.
    unsafe { libc::kill(bwrap_pid as i32, libc::SIGKILL) };
    assert_eq!(child.wait().unwrap().code(), Some(128 + libc::SIGKILL));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "signal 9\n");
}

#[test]
fn no_done_marker_when_sandbox_run_is_killed() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let private = marker_dir();
    let done = private.path().canonicalize().unwrap().join("launch-3.done");
    let mut spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    spec.done = Some(done.clone());
    let mut child = sandboxed(&spec, &[], "sleep 30")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (_, seen) = once_running(child.id(), &["sleep", "30"]);
    child.kill().unwrap();
    child.wait().unwrap();
    wait_all_gone(&seen, "after sandbox-run was killed");
    assert!(!done.exists(), "a killed sandbox-run left a marker");
}

#[test]
fn an_existing_done_marker_stops_the_launch_and_is_left_alone() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let private = marker_dir();
    let dir = private.path().canonicalize().unwrap();
    let done = dir.join("launch-4.done");
    std::fs::write(&done, "earlier\n").unwrap();
    // Somewhere the program can write and the test can see: its worktree.
    let sentinel = l.ws.join(".crew/worktrees/a/ran");
    let mut spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    spec.done = Some(done.clone());
    let write_sentinel = format!("echo ran > '{}'", sentinel.display());
    // Positive control: without a marker the program writes the sentinel.
    let mut control = spec.clone();
    control.done = None;
    let out = sandboxed(&control, &[], &write_sentinel).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(sentinel.exists());
    std::fs::remove_file(&sentinel).unwrap();

    let out = sandboxed(&spec, &[], &write_sentinel).output().unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(!sentinel.exists(), "the program ran");
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "earlier\n");

    // A symlink planted at the marker's path is refused the same way.
    std::fs::remove_file(&done).unwrap();
    std::os::unix::fs::symlink(dir.join("elsewhere"), &done).unwrap();
    let out = sandboxed(&spec, &[], "true").output().unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(!dir.join("elsewhere").exists());

    // A marker that appears while the sandbox runs is never overwritten:
    // sandbox-run exits 125 without a marker of its own. The marker is
    // written once the program is seen running, then the program may end.
    std::fs::remove_file(&done).unwrap();
    let gate = Gate::new(&l, "gate");
    let child = sandboxed(&spec, &[], &format!("sleep 30 & {}", gate.wait()))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    once_running(child.id(), &["sleep", "30"]);
    std::fs::write(&done, "someone else\n").unwrap();
    gate.release();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(125));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "someone else\n");
}

// "not started": a failure before bubblewrap was started is reported too.

/// A spec that needs no host feature to be judged: no mounts, so nothing
/// to pin, and a bwrap that may not exist.
fn bare_spec(bwrap: &Path, done: Option<PathBuf>) -> Spec {
    Spec {
        bwrap: bwrap.to_path_buf(),
        crew_bin: Path::new(CREW).canonicalize().unwrap(),
        mounts: Vec::new(),
        env: Vec::new(),
        pass_env: Vec::new(),
        cwd: "/".into(),
        network: false,
        new_session: true,
        min_landlock_abi: crew::sandbox::landlock::MIN_ABI,
        allow_unix: Vec::new(),
        done,
    }
}

/// Run `crew sandbox-run` with a spec as given (not checked here).
fn run_raw(spec: &Spec) -> Output {
    Command::new(CREW)
        .arg("sandbox-run")
        .arg(serde_json::to_string(spec).unwrap())
        .args(["--", "true"])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_bubblewrap_that_cannot_be_started_is_marked_not_started() {
    let dir = marker_dir();
    let done = dir.path().canonicalize().unwrap().join("launch.done");
    let out = run_raw(&bare_spec(
        Path::new("/nonexistent/bwrap"),
        Some(done.clone()),
    ));
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "not started\n");
    let mode = std::fs::metadata(&done).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );
}

#[test]
fn a_refused_spec_is_marked_not_started_when_its_marker_path_is_sound() {
    let dir = marker_dir();
    let private = dir.path().canonicalize().unwrap();
    let done = private.join("launch.done");
    let mut spec = bare_spec(Path::new("/nonexistent/bwrap"), Some(done.clone()));
    spec.cwd = "relative".into();
    let out = run_raw(&spec);
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "not started\n");
}

#[test]
fn a_marker_path_that_fails_its_own_checks_is_never_written() {
    let dir = marker_dir();
    let private = dir.path().canonicalize().unwrap();
    let shown = private.join("shown");
    std::fs::create_dir(&shown).unwrap();
    // Visible inside: a bind covers it.
    let mut spec = bare_spec(Path::new("/nonexistent/bwrap"), Some(shown.join("done")));
    spec.mounts.push(crew::sandbox::spec::Mount::Bind {
        path: shown.clone(),
        id: crew::sandbox::pin::identify(&shown).unwrap(),
        writable: false,
    });
    let out = run_raw(&spec);
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(
        !shown.join("done").exists(),
        "wrote a marker the sandbox could see"
    );
    // A path with `..`.
    let dotted = private.join("shown/../dotted.done");
    let out = run_raw(&bare_spec(Path::new("/nonexistent/bwrap"), Some(dotted)));
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(!private.join("dotted.done").exists());
}

#[test]
fn a_pin_failure_is_marked_not_started_and_the_program_never_runs() {
    let Some((bwrap, abi)) = bwrap() else { return };
    let l = layout();
    let dir = marker_dir();
    let done = dir.path().canonicalize().unwrap().join("launch.done");
    let mut spec = worker_spec_for_lifetime(&bwrap, abi, &l);
    spec.done = Some(done.clone());
    // The agent's rw directory is replaced after the spec was built.
    let rw = l.agent().join("rw");
    std::fs::rename(&rw, l.root.join("moved-rw")).unwrap();
    std::fs::create_dir(&rw).unwrap();
    let sentinel = l.root.join("ran");
    let out = sandboxed(&spec, &[], &format!("echo ran > '{}'", sentinel.display()))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125), "{}", text(&out));
    assert!(!sentinel.exists());
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "not started\n");
}

/// rev-core's forced failure (m-d579d6b1848c), kept as a regression: once
/// bubblewrap is started, a failure must abort rather than exit with a
/// code. `true` stands in for bubblewrap; a seccomp filter in sandbox-run
/// lets close_range mark descriptors close-on-exec (flags 4) but refuses
/// the plain close (flags 0) that comes after the spawn.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn a_failure_after_bubblewrap_started_aborts_without_a_marker() {
    use std::os::unix::process::ExitStatusExt;
    // Not canonicalized: `true` may be a multi-call binary that goes by
    // the name it is run under.
    let truth = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("true"))
        .find(|p| p.is_file() && crew::sandbox::validate::is_plain_absolute(p))
        .expect("a `true` program on PATH");
    let dir = marker_dir();
    let done = dir.path().canonicalize().unwrap().join("launch.done");
    let spec = bare_spec(&truth, Some(done.clone()));
    let argv = || {
        let mut cmd = Command::new(CREW);
        cmd.arg("sandbox-run")
            .arg(serde_json::to_string(&spec).unwrap())
            .args(["--", "true"])
            .env_clear()
            .stdin(std::process::Stdio::null());
        cmd
    };
    // Positive control: the stand-in "sandbox" ends and is reported.
    let out = argv().output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "exited 0\n");
    std::fs::remove_file(&done).unwrap();

    const SYS_CLOSE_RANGE: u32 = 436; // the same on x86_64 and aarch64
    let filter = [
        // load the syscall number
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 3,
            k: SYS_CLOSE_RANGE,
        },
        // load the low half of the third argument (flags)
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 32,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: 0,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let mut cmd = argv();
    // SAFETY: setrlimit and prctl are async-signal-safe; `filter` outlives
    // the call because the closure owns it.
    unsafe {
        cmd.pre_exec(move || {
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &no_core);
            let prog = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &prog) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    assert_eq!(out.status.signal(), Some(libc::SIGABRT), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("closing inherited descriptors"),
        "{}",
        text(&out)
    );
    assert!(
        !done.exists(),
        "a marker was written although the end is unknown"
    );
}
