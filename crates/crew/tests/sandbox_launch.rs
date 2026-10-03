//! The sandbox launch path end to end, through the real `crew` binary:
//! `crew sandbox-exec` (the shim) and, with bubblewrap, `crew sandbox-run`.
//!
//! Tests that need a host feature (Landlock, bwrap, user namespaces) are
//! skipped with a printed reason when it is missing, and fail instead under
//! `CREW_TEST_SANDBOX=require`.

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
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
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
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
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
    if !host_has(landlock_scopes(), "Landlock ABI 6") {
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
        "bubblewrap with user namespaces and Landlock ABI 6",
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
        (run.join("0123.token"), "operator-token".to_string()),
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
        # walking it takes minutes, and Landlock (ABI 9) guards it.
        {{ find / -path /proc -prune -o -path /nix/store -prune -o -type s -print
          while read -r _ _ _ _ point _; do [ -S "$point" ] && echo "$point"; done < /proc/self/mountinfo
        }} 2>/dev/null | sort -u | sed 's/^/socket /'
        "#,
        token = l.run.join("0123.token").display(),
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
    // sandbox-run execs bwrap in place: the child's pid is bwrap's.
    let pid = child.id();
    let mut seen_bwrap = false;
    for _ in 0..50 {
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline.starts_with(bwrap.as_os_str().as_encoded_bytes()) {
            seen_bwrap = true;
            assert!(!String::from_utf8_lossy(&cmdline).contains(secret));
            let fds: Vec<OsString> = std::fs::read_dir(format!("/proc/{pid}/fd"))
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert!(
                !fds.contains(&OsString::from("50")),
                "bwrap inherited fd 50: {fds:?}"
            );
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
