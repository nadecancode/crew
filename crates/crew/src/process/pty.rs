//! Daemon-owned pseudo-terminals.
//!
//! Each agent runs on its own PTY as a session leader. A reader thread feeds
//! the output into a terminal emulator (so the dashboard can show the rendered
//! screen rather than raw escape codes) and fans it out to attached clients.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use crew_core::{Agent, RuntimeKind};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, mpsc};

use super::{paste_bytes, Attachment, ProcEvent, ProcessManager, Spawned};
use crate::agents::LaunchSpec;

const DEFAULT_ROWS: u16 = 50;
const DEFAULT_COLS: u16 = 160;
/// How often output activity is reported to the daemon.
const OUTPUT_TICK: Duration = Duration::from_millis(400);
/// How long an exit waits for the process's last output to be read.
const EXIT_DRAIN: Duration = Duration::from_millis(500);
/// Pause between pasting a prompt and pressing Enter, so the TUI treats the
/// Enter as a submit and not as part of the paste.
const SUBMIT_DELAY: Duration = Duration::from_millis(250);

/// Variables that would make a child believe it lives in the terminal
/// multiplexer pane the daemon happened to be started from.
const SCRUBBED_ENV: &[&str] = &[
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_STARTUP_CWD",
    "TMUX",
    "TMUX_PANE",
];

/// How often the waiter checks a group that outlived its leader.
const GROUP_WATCH_INTERVAL: Duration = Duration::from_millis(200);

struct PtyProc {
    kind: RuntimeKind,
    pid: Option<u32>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<AtomicBool>,
    /// The process group is known to be empty. Once set, its id may be
    /// recycled, so it is never signalled or probed again.
    group_retired: AtomicBool,
    output: broadcast::Sender<Vec<u8>>,
}

pub struct PtyManager {
    procs: Mutex<HashMap<String, Arc<PtyProc>>>,
    events: mpsc::UnboundedSender<ProcEvent>,
}

impl PtyManager {
    pub fn new(events: mpsc::UnboundedSender<ProcEvent>) -> Self {
        Self {
            procs: Mutex::new(HashMap::new()),
            events,
        }
    }

    fn get(&self, agent_id: &str) -> Result<Arc<PtyProc>> {
        self.procs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent_id)
            .cloned()
            .ok_or_else(|| anyhow!("agent '{agent_id}' has no process"))
    }

    fn live(&self, agent_id: &str) -> Result<Arc<PtyProc>> {
        let proc = self.get(agent_id)?;
        if !proc.alive.load(Ordering::SeqCst) {
            bail!("agent '{agent_id}' is not running");
        }
        Ok(proc)
    }
}

/// Whether any process is still in the group led by `pid` (a zombie counts
/// until it is reaped).
fn group_exists(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: signal 0 only probes for existence.
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Signal the agent's whole process group (the child is a session leader, so
/// its pid is also its process-group id).
fn signal_group(pid: u32, signal: libc::c_int) -> Result<()> {
    if pid <= 1 {
        bail!("refusing to signal pid {pid}");
    }
    // SAFETY: plain syscall; a negative pid addresses the process group.
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err).context("signalling the agent process");
        }
    }
    Ok(())
}

/// Answer the terminal queries TUIs send at startup. Nothing is attached to
/// the PTY to answer them, and e.g. crossterm-based UIs give up without a
/// cursor-position report.
fn answer_queries(chunk: &[u8], parser: &vt100::Parser, writer: &Mutex<Box<dyn Write + Send>>) {
    let mut reply: Vec<u8> = Vec::new();
    if contains(chunk, b"\x1b[6n") {
        let (row, col) = parser.screen().cursor_position();
        reply.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
    }
    if contains(chunk, b"\x1b[c") || contains(chunk, b"\x1b[0c") {
        // "VT220 with ANSI colour".
        reply.extend_from_slice(b"\x1b[?62;22c");
    }
    if contains(chunk, b"\x1b]10;?") {
        reply.extend_from_slice(b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\");
    }
    if contains(chunk, b"\x1b]11;?") {
        reply.extend_from_slice(b"\x1b]11;rgb:0000/0000/0000\x1b\\");
    }
    if !reply.is_empty() {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(&reply);
            let _ = w.flush();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Drop terminal queries from output forwarded to an attached client: the
/// daemon already answered them, and a second answer from the client's real
/// terminal would arrive as stray input.
pub fn strip_queries(chunk: &[u8]) -> Vec<u8> {
    const QUERIES: [&[u8]; 3] = [b"\x1b[6n", b"\x1b[0c", b"\x1b[c"];
    let mut out = Vec::with_capacity(chunk.len());
    let mut i = 0;
    'outer: while i < chunk.len() {
        for q in QUERIES {
            if chunk[i..].starts_with(q) {
                i += q.len();
                continue 'outer;
            }
        }
        out.push(chunk[i]);
        i += 1;
    }
    out
}

/// The last `lines` rows with their colours, as the terminal would show
/// them: each row's text with its SGR codes inline.
fn screen_styled(parser: &vt100::Parser, lines: usize) -> String {
    let screen = parser.screen();
    let plain = screen.contents();
    let plain: Vec<&str> = plain.lines().collect();
    let shown = plain
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |at| at + 1);
    let (_, cols) = screen.size();
    let rows: Vec<String> = screen
        .rows_formatted(0, cols)
        .take(shown)
        .map(|row| String::from_utf8_lossy(&row).into_owned())
        .collect();
    let start = rows.len().saturating_sub(lines.max(1));
    rows[start..].join("\n")
}

fn screen_text(parser: &vt100::Parser, lines: usize) -> String {
    let contents = parser.screen().contents();
    let mut rows: Vec<&str> = contents.lines().map(str::trim_end).collect();
    while rows.last().is_some_and(|l| l.is_empty()) {
        rows.pop();
    }
    let start = rows.len().saturating_sub(lines.max(1));
    rows[start..].join("\n")
}

/// The command that starts `spec` on a PTY, and whether the first prompt went
/// in as its last argument.
fn command_for(spec: &LaunchSpec) -> Result<(CommandBuilder, bool)> {
    // Structured argv: the program and each argument are separate strings
    // handed to exec, never a shell command line. In a sandbox the program
    // runs behind `crew sandbox-run`, which pins its mounts and execs
    // bubblewrap (a PTY child has every descriptor above 2 closed, so
    // nothing could be handed down any other way).
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    if let Some(sandbox) = &spec.sandbox {
        argv.extend(crate::sandbox::run::prefix(sandbox).map_err(|e| anyhow!(e))?);
    }
    argv.push(spec.program.clone().into());
    argv.extend(spec.args.iter().map(Into::into));
    let mut prompt_delivered = false;
    if spec.prompt_as_arg {
        if let Some(prompt) = &spec.prompt {
            argv.push(prompt.into());
            prompt_delivered = true;
        }
    }
    let mut cmd = CommandBuilder::from_argv(argv);
    cmd.cwd(&spec.cwd);
    let mut launch_env: Vec<(String, String)> = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
    ];
    launch_env.extend(spec.env.iter().cloned());
    match &spec.sandbox {
        Some(sandbox) => {
            // Exactly the launcher environment: the allow-listed names of the
            // daemon's own plus the variables the spec passes by name.
            cmd.env_clear();
            for (name, value) in
                crate::sandbox::spec::launcher_env(sandbox, std::env::vars_os(), &launch_env)
            {
                cmd.env(name, value);
            }
        }
        None => {
            for key in SCRUBBED_ENV {
                cmd.env_remove(key);
            }
            for (key, value) in &launch_env {
                cmd.env(key, value);
            }
        }
    }
    Ok((cmd, prompt_delivered))
}

#[async_trait]
impl ProcessManager for PtyManager {
    fn name(&self) -> &'static str {
        "pty"
    }

    async fn spawn(&self, spec: &LaunchSpec) -> Result<Spawned> {
        if let Ok(old) = self.get(&spec.agent_id) {
            if old.alive.load(Ordering::SeqCst) {
                bail!("agent '{}' is already running", spec.agent_id);
            }
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: DEFAULT_ROWS,
                cols: DEFAULT_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("opening a pty: {e}"))?;

        let (cmd, prompt_delivered) = command_for(spec)?;

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow!("starting `{}`: {e}", spec.program))?;
        // Close our copy of the slave so the reader sees EOF when the child exits.
        drop(pair.slave);
        let pid = child.process_id();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow!("cloning the pty reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow!("taking the pty writer: {e}"))?;

        let (output, _) = broadcast::channel(512);
        let proc = Arc::new(PtyProc {
            kind: spec.kind,
            pid,
            master: Mutex::new(pair.master),
            writer: Arc::new(Mutex::new(writer)),
            parser: Arc::new(Mutex::new(vt100::Parser::new(
                DEFAULT_ROWS,
                DEFAULT_COLS,
                0,
            ))),
            alive: Arc::new(AtomicBool::new(true)),
            group_retired: AtomicBool::new(false),
            output,
        });

        // Closed when the reader is done: the exit is reported after what the
        // process wrote last has been read.
        let (drained_tx, drained_rx) = std::sync::mpsc::channel::<()>();

        // Reader: terminal emulation + fan-out + throttled liveness ticks.
        {
            let proc = proc.clone();
            let events = self.events.clone();
            let agent_id = spec.agent_id.clone();
            std::thread::Builder::new()
                .name(format!("pty-read-{agent_id}"))
                .spawn(move || {
                    let _drained = drained_tx;
                    let mut buf = [0u8; 8192];
                    let mut last_tick: Option<Instant> = None;
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let chunk = &buf[..n];
                                if let Ok(mut parser) = proc.parser.lock() {
                                    parser.process(chunk);
                                    answer_queries(chunk, &parser, &proc.writer);
                                }
                                let _ = proc.output.send(chunk.to_vec());
                                if last_tick.is_none_or(|t| t.elapsed() >= OUTPUT_TICK) {
                                    last_tick = Some(Instant::now());
                                    let _ = events.send(ProcEvent::Output {
                                        agent_id: agent_id.clone(),
                                    });
                                }
                            }
                        }
                    }
                })
                .context("starting the pty reader thread")?;
        }

        // Waiter: reports the exit exactly once.
        {
            let proc = proc.clone();
            let events = self.events.clone();
            let agent_id = spec.agent_id.clone();
            std::thread::Builder::new()
                .name(format!("pty-wait-{agent_id}"))
                .spawn(move || {
                    let status = child.wait();
                    // Its last output may still be on its way through the
                    // pty; a child it left behind can hold that open, so
                    // only briefly.
                    let _ = drained_rx.recv_timeout(EXIT_DRAIN);
                    proc.alive.store(false, Ordering::SeqCst);
                    if pid.is_some_and(|pid| !group_exists(pid)) {
                        proc.group_retired.store(true, Ordering::SeqCst);
                    }
                    let (code, detail) = match status {
                        Ok(status) => match status.signal() {
                            Some(signal) => (None, format!("killed by signal {signal}")),
                            None => (
                                Some(status.exit_code() as i32),
                                format!("exited with code {}", status.exit_code()),
                            ),
                        },
                        Err(e) => (None, format!("wait failed: {e}")),
                    };
                    let _ = events.send(ProcEvent::Exited {
                        agent_id,
                        instance: pid.map(|p| p.to_string()).unwrap_or_default(),
                        code,
                        detail,
                    });
                    // Members that outlived the leader: keep observing the group
                    // until it is empty (kill() may retire it first), so its id
                    // is never probed once it can be recycled. There is no time
                    // limit: elapsed time is no evidence the group is not ours.
                    let Some(pid) = pid else { return };
                    loop {
                        if proc.group_retired.load(Ordering::SeqCst) {
                            return;
                        }
                        if !group_exists(pid) {
                            proc.group_retired.store(true, Ordering::SeqCst);
                            return;
                        }
                        std::thread::sleep(GROUP_WATCH_INTERVAL);
                    }
                })
                .context("starting the pty waiter thread")?;
        }

        self.procs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.agent_id.clone(), proc);
        Ok(Spawned {
            pid,
            backend_ref: None,
            prompt_delivered,
            ready: false,
        })
    }

    async fn submit_prompt(&self, agent_id: &str, text: &str) -> Result<()> {
        let proc = self.live(agent_id)?;
        if proc.kind == RuntimeKind::Shell {
            // Plain line-oriented stdin: one line, newline-terminated.
            let mut line = paste_bytes(text, false);
            line.push(b'\n');
            return write_all(&proc, line).await;
        }
        let bracketed = proc
            .parser
            .lock()
            .map(|p| p.screen().bracketed_paste())
            .unwrap_or(false);
        write_all(&proc, paste_bytes(text, bracketed)).await?;
        tokio::time::sleep(SUBMIT_DELAY).await;
        write_all(&proc, b"\r".to_vec()).await
    }

    async fn write_raw(&self, agent_id: &str, data: &[u8]) -> Result<()> {
        let proc = self.live(agent_id)?;
        write_all(&proc, data.to_vec()).await
    }

    async fn screen(&self, agent_id: &str, lines: usize) -> Result<String> {
        let proc = self.get(agent_id)?;
        let parser = proc
            .parser
            .lock()
            .map_err(|_| anyhow!("terminal state poisoned"))?;
        Ok(screen_text(&parser, lines))
    }

    async fn styled(&self, agent_id: &str, lines: usize) -> Result<String> {
        let proc = self.get(agent_id)?;
        let parser = proc
            .parser
            .lock()
            .map_err(|_| anyhow!("terminal state poisoned"))?;
        Ok(screen_styled(&parser, lines))
    }

    async fn pause(&self, agent_id: &str) -> Result<()> {
        let proc = self.live(agent_id)?;
        signal_group(proc.pid.ok_or_else(|| anyhow!("no pid"))?, libc::SIGSTOP)
    }

    async fn resume(&self, agent_id: &str) -> Result<()> {
        let proc = self.live(agent_id)?;
        signal_group(proc.pid.ok_or_else(|| anyhow!("no pid"))?, libc::SIGCONT)
    }

    async fn kill(&self, agent_id: &str) -> Result<()> {
        let Ok(proc) = self.get(agent_id) else {
            return Ok(());
        };
        let Some(pid) = proc.pid else { return Ok(()) };
        // The group is tracked by its own id, not by the leader: children can
        // outlive a leader that exits on TERM, and must still be killed.
        if proc.group_retired.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !group_exists(pid) {
            proc.group_retired.store(true, Ordering::SeqCst);
            return Ok(());
        }
        // A stopped process cannot handle SIGTERM until it is continued.
        signal_group(pid, libc::SIGCONT)?;
        signal_group(pid, libc::SIGTERM)?;
        for _ in 0..20 {
            if !group_exists(pid) {
                proc.group_retired.store(true, Ordering::SeqCst);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        signal_group(pid, libc::SIGKILL)?;
        for _ in 0..20 {
            if !group_exists(pid) {
                proc.group_retired.store(true, Ordering::SeqCst);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    fn is_alive(&self, agent_id: &str) -> bool {
        self.get(agent_id)
            .map(|p| p.alive.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    async fn adopt(&self, _agent: &Agent) -> Result<bool> {
        // The PTYs died with the previous daemon; there is nothing to adopt.
        Ok(false)
    }

    fn attach(&self, agent_id: &str) -> Result<Attachment> {
        let proc = self.live(agent_id)?;
        let parser = proc
            .parser
            .lock()
            .map_err(|_| anyhow!("terminal state poisoned"))?;
        // Subscribe while holding the parser lock so no output is lost or
        // duplicated between the snapshot and the stream.
        let output = proc.output.subscribe();
        let mut snapshot = b"\x1b[2J\x1b[H".to_vec();
        snapshot.extend_from_slice(&parser.screen().contents_formatted());
        Ok(Attachment { snapshot, output })
    }

    async fn resize(&self, agent_id: &str, rows: u16, cols: u16) -> Result<()> {
        let proc = self.live(agent_id)?;
        let (rows, cols) = (rows.clamp(5, 500), cols.clamp(20, 1000));
        proc.master
            .lock()
            .map_err(|_| anyhow!("pty poisoned"))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow!("resizing the pty: {e}"))?;
        if let Ok(mut parser) = proc.parser.lock() {
            parser.set_size(rows, cols);
        }
        Ok(())
    }
}

async fn write_all(proc: &Arc<PtyProc>, data: Vec<u8>) -> Result<()> {
    let writer = proc.writer.clone();
    // A PTY write blocks when the child is not reading; keep it off the runtime.
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut w = writer.lock().map_err(|_| anyhow!("pty writer poisoned"))?;
        w.write_all(&data)?;
        w.flush()?;
        Ok(())
    })
    .await
    .context("pty writer task")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec(id: &str, program: &str, args: &[&str]) -> LaunchSpec {
        LaunchSpec {
            agent_id: id.into(),
            kind: RuntimeKind::Shell,
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: vec![("CREW_TEST_VAR".into(), "from-crew".into())],
            cwd: PathBuf::from("/"),
            prompt: None,
            prompt_as_arg: false,
            sidecar: None,
            sandbox: None,
        }
    }

    async fn wait_for_exit(rx: &mut mpsc::UnboundedReceiver<ProcEvent>) -> ProcEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("timed out waiting for a process event")
                .expect("event channel closed");
            if matches!(event, ProcEvent::Exited { .. }) {
                return event;
            }
        }
    }

    async fn wait_for_screen(mgr: &PtyManager, id: &str, needle: &str) -> String {
        for _ in 0..100 {
            let screen = mgr.screen(id, 50).await.unwrap();
            if screen.contains(needle) {
                return screen;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "'{needle}' never appeared on screen: {:?}",
            mgr.screen(id, 50).await
        );
    }

    #[test]
    fn a_sandboxed_launch_runs_behind_sandbox_run_with_exactly_the_launchers_environment() {
        use crate::sandbox::spec::Spec;
        let sandbox = Spec {
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: "/opt/crew/bin/crew".into(),
            mounts: vec![],
            env: vec![("HOME".into(), "/home/agent".into())],
            pass_env: vec!["ANTHROPIC_API_KEY".into(), "CREW_IDENTITY".into()],
            cwd: "/work".into(),
            network: true,
            new_session: false,
            min_landlock_abi: crate::sandbox::landlock::MIN_ABI,
            allow_unix: vec![],
        };
        let mut launch = spec("a", "claude", &["--flag", "value"]);
        launch.prompt = Some("do the task".into());
        launch.prompt_as_arg = true;
        launch.cwd = "/work".into();
        launch.env = vec![
            ("ANTHROPIC_API_KEY".into(), "sk-secret".into()),
            ("CREW_IDENTITY".into(), "/id.json".into()),
            ("NOT_PASSED".into(), "dropped".into()),
        ];
        launch.sandbox = Some(sandbox.clone());
        let (cmd, delivered) = command_for(&launch).unwrap();
        assert!(delivered);
        let argv: Vec<String> = cmd
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "/opt/crew/bin/crew");
        assert_eq!(argv[1], "sandbox-run");
        assert_eq!(argv[3], "--");
        assert_eq!(&argv[4..], ["claude", "--flag", "value", "do the task"]);
        // The spec on the command line holds no secret.
        assert!(!argv[2].contains("sk-secret"), "{}", argv[2]);
        // The environment is exactly the launcher's: nothing the daemon has
        // beyond the allow-list, and no variable the spec does not pass.
        let env: std::collections::BTreeMap<String, String> = cmd
            .iter_full_env_as_str()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-secret")
        );
        assert_eq!(
            env.get("CREW_IDENTITY").map(String::as_str),
            Some("/id.json")
        );
        assert!(!env.contains_key("NOT_PASSED"), "{env:?}");
        assert!(
            !env.contains_key("PATH") && !env.contains_key("HOME"),
            "{env:?}"
        );
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        // Without a sandbox nothing changes.
        launch.sandbox = None;
        let (plain, _) = command_for(&launch).unwrap();
        assert_eq!(plain.get_argv()[0].to_string_lossy(), "claude");
    }

    #[tokio::test]
    async fn runs_a_process_and_reports_its_exit_code() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        let spawned = mgr
            .spawn(&spec(
                "a",
                "sh",
                &["-c", "echo hello-$CREW_TEST_VAR; exit 3"],
            ))
            .await
            .unwrap();
        assert!(spawned.pid.is_some());
        let exit = wait_for_exit(&mut rx).await;
        assert_eq!(
            exit,
            ProcEvent::Exited {
                agent_id: "a".into(),
                instance: spawned.pid.unwrap().to_string(),
                code: Some(3),
                detail: "exited with code 3".into()
            }
        );
        assert!(!mgr.is_alive("a"));
        // The screen survives the process, with the environment applied.
        assert!(mgr
            .screen("a", 10)
            .await
            .unwrap()
            .contains("hello-from-crew"));
    }

    #[tokio::test]
    async fn the_styled_screen_keeps_the_colours() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec(
            "c",
            "sh",
            &[
                "-c",
                "printf 'plain \\033[1;32mgreen\\033[0m end\\n'; exit 0",
            ],
        ))
        .await
        .unwrap();
        wait_for_exit(&mut rx).await;
        let plain = mgr.screen("c", 10).await.unwrap();
        assert!(
            plain.contains("plain green end") && !plain.contains('\x1b'),
            "{plain:?}"
        );
        let styled = mgr.styled("c", 10).await.unwrap();
        // Bold and green, in whichever order the terminal state writes them.
        let before = &styled[..styled.find("green").unwrap()];
        let codes = &before[before.rfind("\x1b[").unwrap()..];
        assert!(
            codes.contains("32") && codes.contains("1") && codes.ends_with("m"),
            "{styled:?}"
        );
        assert!(styled.contains(" end"), "{styled:?}");
        // Only the rows with something on them, like the plain screen.
        assert_eq!(styled.lines().count(), plain.lines().count(), "{styled:?}");
        assert_eq!(mgr.styled("c", 1).await.unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn arguments_are_never_interpreted_by_a_shell() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec("a", "printf", &["%s", "$(echo pwned); `id` && true"]))
            .await
            .unwrap();
        wait_for_exit(&mut rx).await;
        let screen = mgr.screen("a", 10).await.unwrap();
        assert!(screen.contains("$(echo pwned); `id` && true"), "{screen}");
    }

    #[tokio::test]
    async fn the_exit_event_comes_after_the_last_output_is_on_the_screen() {
        for i in 0..150 {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mgr = PtyManager::new(tx);
            mgr.spawn(&spec("a", "printf", &["%s", &format!("last-words-{i}")]))
                .await
                .unwrap();
            wait_for_exit(&mut rx).await;
            let screen = mgr.screen("a", 10).await.unwrap();
            assert!(
                screen.contains(&format!("last-words-{i}")),
                "run {i}: {screen:?}"
            );
        }
    }

    #[tokio::test]
    async fn prompts_reach_stdin_and_kill_stops_the_process() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec(
            "a",
            "sh",
            &["-c", "while read -r line; do echo \"got:$line\"; done"],
        ))
        .await
        .unwrap();
        assert!(mgr.is_alive("a"));
        mgr.submit_prompt("a", "ping\nsecond").await.unwrap();
        // Newlines are flattened so a multi-line message stays one line of input.
        wait_for_screen(&mgr, "a", "got:ping second").await;

        mgr.pause("a").await.unwrap();
        mgr.resume("a").await.unwrap();
        mgr.kill("a").await.unwrap();
        let exit = wait_for_exit(&mut rx).await;
        assert!(
            matches!(exit, ProcEvent::Exited { code: None, .. }),
            "{exit:?}"
        );
        assert!(!mgr.is_alive("a"));
        assert!(mgr.submit_prompt("a", "late").await.is_err());
        assert!(!mgr
            .adopt(&crate::process::pty::tests::dummy_agent())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn kill_takes_down_children_that_ignore_term_and_hup() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        let script = format!(
            "(trap '' TERM HUP; exec sleep 300) & echo $! > {}; wait",
            pidfile.display()
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec("a", "sh", &["-c", &script])).await.unwrap();
        let mut child = None;
        for _ in 0..100 {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<libc::pid_t>().ok())
            {
                child = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let child = child.expect("child pid was never written");
        mgr.kill("a").await.unwrap();
        wait_for_exit(&mut rx).await;
        // The leader is gone; its TERM-proof child must be gone too.
        let mut gone = false;
        for _ in 0..40 {
            // SAFETY: signal 0 only checks that the process exists.
            if unsafe { libc::kill(child, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !gone {
            // SAFETY: do not leak the sleeper when the assertion fails.
            unsafe { libc::kill(child, libc::SIGKILL) };
        }
        assert!(gone, "child {child} survived kill()");
    }

    #[tokio::test]
    async fn a_group_that_is_gone_is_retired_so_a_recycled_pid_is_never_signalled() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec("a", "sh", &["-c", "exit 0"]))
            .await
            .unwrap();
        wait_for_exit(&mut rx).await;
        assert!(mgr.get("a").unwrap().group_retired.load(Ordering::SeqCst));
        // And a group killed through kill() is retired too.
        mgr.spawn(&spec("b", "sh", &["-c", "sleep 300"]))
            .await
            .unwrap();
        mgr.kill("b").await.unwrap();
        wait_for_exit(&mut rx).await;
        assert!(mgr.get("b").unwrap().group_retired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_group_whose_children_outlive_the_leader_is_retired_when_they_exit() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        // The leader exits at once, leaving a child that ends by itself.
        mgr.spawn(&spec(
            "a",
            "sh",
            &["-c", "trap '' HUP; sleep 1 & sleep 0.1; exit 0"],
        ))
        .await
        .unwrap();
        wait_for_exit(&mut rx).await;
        let proc = mgr.get("a").unwrap();
        assert!(
            !proc.group_retired.load(Ordering::SeqCst),
            "child still runs"
        );
        for _ in 0..60 {
            if proc.group_retired.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the group was never retired after its last member exited");
    }

    #[tokio::test]
    async fn spawn_failure_is_an_error_not_a_panic() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        let err = mgr
            .spawn(&spec("a", "/definitely/not/a/binary", &[]))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("/definitely/not/a/binary"),
            "{err}"
        );
        assert!(mgr.screen("missing", 5).await.is_err());
    }

    #[tokio::test]
    async fn attach_replays_the_screen_and_streams_output() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = PtyManager::new(tx);
        mgr.spawn(&spec(
            "a",
            "sh",
            &["-c", "echo before; read -r x; echo after-$x; sleep 5"],
        ))
        .await
        .unwrap();
        wait_for_screen(&mgr, "a", "before").await;
        let mut attachment = mgr.attach("a").unwrap();
        assert!(String::from_utf8_lossy(&attachment.snapshot).contains("before"));
        mgr.resize("a", 30, 100).await.unwrap();
        mgr.write_raw("a", b"typed\n").await.unwrap();
        let mut seen = String::new();
        while !seen.contains("after-typed") {
            let chunk = tokio::time::timeout(Duration::from_secs(5), attachment.output.recv())
                .await
                .expect("no output")
                .unwrap();
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        mgr.kill("a").await.unwrap();
    }

    #[test]
    fn terminal_queries_are_filtered_for_attached_clients() {
        assert_eq!(strip_queries(b"ab\x1b[6ncd\x1b[c"), b"abcd");
        assert_eq!(strip_queries(b"\x1b[31mred"), b"\x1b[31mred");
    }

    pub(super) fn dummy_agent() -> Agent {
        Agent {
            id: "a".into(),
            name: "a".into(),
            role: String::new(),
            task: None,
            status: crew_core::AgentStatus::Idle,
            activity: String::new(),
            runtime: RuntimeKind::Shell,
            model: None,
            workdir: "/".into(),
            branch: None,
            backend: "pty".into(),
            backend_ref: None,
            pid: None,
            session_id: None,
            orchestrator: false,
            parent: None,
            subagents: 0,
            temporary: false,
            delivery: String::new(),
            started_at: None,
            last_activity_at: 0,
            progress: None,
            paused: false,
            block_kind: None,
            limit: None,
            context: None,
            compact_pending: false,
            compactions: 0,
            running_model: None,
            model_name: None,
            effort: None,
            fallback: None,
            exit_code: None,
            restarts: 0,
            inbox_unread: 0,
            inbox_total: 0,
            outbox_total: 0,
            usage: None,
            error: None,
        }
    }
}
