//! herdr backend: every agent is a named herdr agent in its own pane.
//!
//! herdr (https://herdr.dev) owns the terminals, so the agents appear in the
//! herdr panel next to the user's other agents, survive a crew daemon restart,
//! and can be watched or taken over like any other pane. Crew drives herdr
//! through its socket API (newline-delimited JSON over a Unix socket):
//! `workspace.create` / `tab.create` / `pane.split` for layout, `agent.start`
//! and `agent.prompt` for the agent itself, and `events.subscribe` for
//! lifecycle changes (pushed, not polled).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use crew_core::config::HerdrLayout;
use crew_core::{Agent, RuntimeKind};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{Hint, PaneMeta, ProcEvent, ProcessManager, Spawned};
use crate::agents::{shell_quote, LaunchSpec};

/// Source id crew reports metadata under.
const SOURCE: &str = "crew";
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// What herdr may take per line of scrollback it is asked for (measured:
/// about 35 ms a line once a read reaches past the screen).
const SCROLLBACK_LINE_TIME: Duration = Duration::from_millis(80);
/// How long the unwrapped copy of the screen may take before the wrapped
/// one is used as it is.
const UNWRAP_TIMEOUT: Duration = Duration::from_millis(1500);
const START_TIMEOUT_MS: u64 = 90_000;

#[derive(Clone, Debug)]
struct Pane {
    pane_id: String,
    /// The herdr agent name; agent calls address by name. Normally the crew
    /// agent id, qualified by the team when another team in the same herdr
    /// session already uses that id.
    name: String,
    kind: RuntimeKind,
    alive: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    workspace_id: Option<String>,
    panes: HashMap<String, Pane>,
    /// Tiled layout: the pane the next split hangs off, and how many exist.
    last_pane: Option<String>,
    tiles: usize,
}

pub struct HerdrManager {
    socket: PathBuf,
    label: String,
    layout: HerdrLayout,
    state: Mutex<State>,
    /// Serialises layout changes so panes are created in a stable order.
    layout_lock: tokio::sync::Mutex<()>,
    events: mpsc::UnboundedSender<ProcEvent>,
    seq: AtomicU64,
}

impl HerdrManager {
    pub fn new(
        socket: &Path,
        label: &str,
        layout: HerdrLayout,
        workspace_id: Option<String>,
        events: mpsc::UnboundedSender<ProcEvent>,
    ) -> Self {
        Self {
            socket: socket.to_path_buf(),
            label: label.to_string(),
            layout,
            state: Mutex::new(State {
                workspace_id,
                ..State::default()
            }),
            layout_lock: tokio::sync::Mutex::new(()),
            events,
            seq: AtomicU64::new(1),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pane(&self, agent_id: &str) -> Result<Pane> {
        self.state()
            .panes
            .get(agent_id)
            .cloned()
            .ok_or_else(|| anyhow!("agent '{agent_id}' has no herdr pane"))
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(method, params, CALL_TIMEOUT).await
    }

    /// The text of a pane, from one of herdr's snapshot sources.
    async fn read_pane(
        &self,
        pane_id: &str,
        source: &str,
        lines: Option<usize>,
        ansi: bool,
        timeout: Duration,
    ) -> Result<String> {
        let mut params = json!({ "pane_id": pane_id, "source": source });
        if let Some(lines) = lines {
            params["lines"] = json!(lines);
        }
        if ansi {
            params["format"] = json!("ansi");
        }
        let result = self.call_with_timeout("pane.read", params, timeout).await?;
        Ok(str_at(&result, &["/read/text", "/text"])
            .unwrap_or_default()
            .to_string())
    }

    /// What the pane shows now, plain or with its colours. The same rows
    /// with soft-wrapped lines joined, as `screen` gives them: asking for
    /// fewer lines than the screen has keeps herdr out of the scrollback,
    /// which is what makes this read quick.
    async fn visible(&self, agent_id: &str, lines: usize, ansi: bool) -> Result<String> {
        let pane = self.pane(agent_id)?;
        let shown = self
            .read_pane(&pane.pane_id, "visible", None, ansi, CALL_TIMEOUT)
            .await?;
        let tidy = |text: String| text.replace('\r', "").trim_end().to_string();
        let rows = shown.lines().count();
        if rows > 2 {
            let within = (rows - 1).min(lines.max(1));
            let joined = self
                .read_pane(
                    &pane.pane_id,
                    "recent_unwrapped",
                    Some(within),
                    ansi,
                    UNWRAP_TIMEOUT,
                )
                .await;
            // herdr counts those lines up from the bottom row of the screen,
            // blank rows included: a screen with little on it comes back
            // empty, and the wrapped text is used as it is.
            if let Some(joined) = joined.ok().filter(|text| !text.trim().is_empty()) {
                return Ok(tidy(joined));
            }
        }
        Ok(tidy(shown))
    }

    async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = format!("crew:{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let request = json!({ "id": id, "method": method, "params": params });
        let exchange = async {
            let stream = UnixStream::connect(&self.socket)
                .await
                .with_context(|| format!("connecting to herdr at {}", self.socket.display()))?;
            let (read, mut write) = stream.into_split();
            let mut line = serde_json::to_vec(&request)?;
            line.push(b'\n');
            write.write_all(&line).await?;
            let mut reader = BufReader::new(read);
            let mut response = String::new();
            reader.read_line(&mut response).await?;
            if response.trim().is_empty() {
                bail!("herdr closed the connection without answering {method}");
            }
            Ok(serde_json::from_str::<Value>(&response)?)
        };
        let response: Value = tokio::time::timeout(timeout, exchange)
            .await
            .map_err(|_| anyhow!("herdr did not answer {method} within {timeout:?}"))??;
        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("error");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            bail!("herdr {method} failed: {code}: {message}");
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Create the pane an agent will live in and return its id.
    async fn create_pane(&self, spec: &LaunchSpec) -> Result<String> {
        let _guard = self.layout_lock.lock().await;
        let env: serde_json::Map<String, Value> = spec
            .env
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        let cwd = spec.cwd.to_string_lossy().into_owned();

        // Reuse the team's workspace if it still exists.
        let mut workspace = self.state().workspace_id.clone();
        if let Some(id) = &workspace {
            if self
                .call("workspace.get", json!({ "workspace_id": id }))
                .await
                .is_err()
            {
                workspace = None;
                let mut state = self.state();
                state.workspace_id = None;
                state.last_pane = None;
                state.tiles = 0;
            }
        }

        let Some(workspace_id) = workspace else {
            let result = self
                .call(
                    "workspace.create",
                    json!({ "label": self.label, "cwd": cwd, "env": env, "focus": false }),
                )
                .await?;
            let workspace_id = str_at(&result, &["/workspace/workspace_id"])
                .ok_or_else(|| anyhow!("herdr workspace.create returned no workspace id"))?;
            let pane_id = str_at(&result, &["/root_pane/pane_id"])
                .ok_or_else(|| anyhow!("herdr workspace.create returned no pane id"))?;
            if let Some(tab_id) = str_at(&result, &["/tab/tab_id"]) {
                let label = match self.layout {
                    HerdrLayout::Tabs => spec.agent_id.as_str(),
                    HerdrLayout::Tiled => "team",
                };
                let _ = self
                    .call("tab.rename", json!({ "tab_id": tab_id, "label": label }))
                    .await;
            }
            let mut state = self.state();
            state.workspace_id = Some(workspace_id);
            state.last_pane = Some(pane_id.clone());
            state.tiles = 1;
            return Ok(pane_id);
        };

        match self.layout {
            HerdrLayout::Tabs => {
                let result = self
                    .call(
                        "tab.create",
                        json!({
                            "workspace_id": workspace_id,
                            "cwd": cwd,
                            "env": env,
                            "label": spec.agent_id,
                            "focus": false,
                        }),
                    )
                    .await?;
                str_at(&result, &["/root_pane/pane_id"])
                    .ok_or_else(|| anyhow!("herdr tab.create returned no pane id"))
            }
            HerdrLayout::Tiled => {
                let (target, tiles) = {
                    let state = self.state();
                    (state.last_pane.clone(), state.tiles)
                };
                let Some(target) = target else {
                    bail!("no pane to split in the team tab");
                };
                // Alternate directions so panes stay roughly square.
                let direction = if tiles % 2 == 1 { "right" } else { "down" };
                let result = self
                    .call(
                        "pane.split",
                        json!({
                            "target_pane_id": target,
                            "direction": direction,
                            "cwd": cwd,
                            "env": env,
                            "focus": false,
                        }),
                    )
                    .await?;
                let pane_id = str_at(&result, &["/pane/pane_id"])
                    .ok_or_else(|| anyhow!("herdr pane.split returned no pane id"))?;
                let mut state = self.state();
                state.last_pane = Some(pane_id.clone());
                state.tiles += 1;
                Ok(pane_id)
            }
        }
    }

    /// Follow one pane's lifecycle over a subscription connection.
    fn watch(self: &Arc<Self>, agent_id: String, pane: Pane) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut failures = 0u32;
            while pane.alive.load(Ordering::SeqCst) {
                match this.watch_once(&agent_id, &pane).await {
                    Ok(()) => failures = 0,
                    Err(e) => {
                        failures += 1;
                        tracing::debug!(agent = %agent_id, "herdr watch: {e:#}");
                    }
                }
                if !pane.alive.load(Ordering::SeqCst) {
                    break;
                }
                if failures >= 5 {
                    pane.alive.store(false, Ordering::SeqCst);
                    let _ = this.events.send(ProcEvent::Exited {
                        agent_id: agent_id.clone(),
                        instance: pane.pane_id.clone(),
                        code: None,
                        detail: "lost the connection to herdr".into(),
                    });
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500 * u64::from(failures.max(1)))).await;
                // The pane may have closed while we were not listening.
                if failures > 0 && this.pane_gone(&pane.pane_id).await {
                    pane.alive.store(false, Ordering::SeqCst);
                    let _ = this.events.send(ProcEvent::Exited {
                        agent_id: agent_id.clone(),
                        instance: pane.pane_id.clone(),
                        code: None,
                        detail: "herdr pane closed".into(),
                    });
                    break;
                }
            }
        });
    }

    async fn pane_gone(&self, pane_id: &str) -> bool {
        match self.call("pane.get", json!({ "pane_id": pane_id })).await {
            Ok(_) => false,
            Err(e) => {
                let text = e.to_string();
                text.contains("not_found") || text.contains("not found") || text.contains("unknown")
            }
        }
    }

    async fn watch_once(&self, agent_id: &str, pane: &Pane) -> Result<()> {
        let stream = UnixStream::connect(&self.socket).await?;
        let (read, mut write) = stream.into_split();
        let request = json!({
            "id": format!("crew:watch:{agent_id}"),
            "method": "events.subscribe",
            "params": { "subscriptions": [
                { "type": "pane.agent_status_changed", "pane_id": pane.pane_id },
                { "type": "pane.exited" },
                { "type": "pane.closed" },
            ]},
        });
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        write.write_all(&line).await?;
        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines.next_line().await? {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(error) = value.get("error") {
                bail!("herdr events.subscribe failed: {error}");
            }
            let Some(event) = value.get("event").and_then(Value::as_str) else {
                continue;
            };
            let data = value.get("data").cloned().unwrap_or(Value::Null);
            if str_at(&data, &["/pane_id", "/pane/pane_id"]).as_deref()
                != Some(pane.pane_id.as_str())
            {
                continue;
            }
            match event.replace('.', "_").as_str() {
                "pane_agent_status_changed" => {
                    let status = data
                        .get("agent_status")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    // The pane outlives the agent: when herdr no longer sees
                    // an agent in it, the agent's process has exited and the
                    // pane is back at its shell.
                    if herdr_kind(pane.kind).is_some()
                        && status == "unknown"
                        && self.agent_left(&pane.pane_id).await
                    {
                        pane.alive.store(false, Ordering::SeqCst);
                        let _ = self.events.send(ProcEvent::Exited {
                            agent_id: agent_id.to_string(),
                            instance: pane.pane_id.clone(),
                            code: None,
                            detail: "the agent exited (its herdr pane is still open)".into(),
                        });
                        return Ok(());
                    }
                    let hint = match status {
                        "working" => Some(Hint::Working),
                        "idle" | "done" => Some(Hint::Idle),
                        "blocked" => Some(Hint::Blocked),
                        _ => None,
                    };
                    if let Some(hint) = hint {
                        let _ = self.events.send(ProcEvent::Hint {
                            agent_id: agent_id.to_string(),
                            hint,
                        });
                    }
                    let _ = self.events.send(ProcEvent::Output {
                        agent_id: agent_id.to_string(),
                    });
                }
                "pane_exited" | "pane_closed" => {
                    pane.alive.store(false, Ordering::SeqCst);
                    let code = data
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .map(|c| c as i32);
                    let _ = self.events.send(ProcEvent::Exited {
                        agent_id: agent_id.to_string(),
                        instance: pane.pane_id.clone(),
                        code,
                        detail: format!("herdr {}", event.replace(['_', '.'], " ")),
                    });
                    return Ok(());
                }
                _ => {}
            }
        }
        bail!("herdr closed the event stream")
    }

    /// The pane still exists but no agent occupies it any more.
    async fn agent_left(&self, pane_id: &str) -> bool {
        match self.call("pane.get", json!({ "pane_id": pane_id })).await {
            Ok(result) => result.pointer("/pane/agent").is_none_or(Value::is_null),
            Err(_) => false,
        }
    }

    async fn pane_pids(&self, pane_id: &str) -> Result<(Option<u32>, Option<u32>)> {
        let result = self
            .call("pane.process_info", json!({ "pane_id": pane_id }))
            .await?;
        let info = result.get("process_info").unwrap_or(&result);
        let group = info
            .get("foreground_process_group_id")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let first = info
            .get("foreground_processes")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|p| p.get("pid"))
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        Ok((first.or(group), group))
    }

    /// Fail closed unless the pane's foreground process is `program`.
    async fn verify_program(&self, pane_id: &str, program: &Path) -> Result<()> {
        let mut seen = None;
        for _ in 0..5 {
            seen = self.pane_pids(pane_id).await.ok().and_then(|(pid, _)| pid);
            if seen.is_some_and(|pid| process_runs(pid, program)) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        bail!(
            "herdr started a pi other than the selected {} (foreground process {}); \
use backend: pty or put the selected pi first on herdr's PATH",
            program.display(),
            seen.map_or("unknown".to_string(), |p| p.to_string())
        )
    }

    async fn signal_foreground(&self, agent_id: &str, signal: libc::c_int) -> Result<()> {
        let pane = self.pane(agent_id)?;
        let (_, group) = self.pane_pids(&pane.pane_id).await?;
        let group =
            group.ok_or_else(|| anyhow!("herdr did not report a foreground process group"))?;
        if group <= 1 {
            bail!("refusing to signal process group {group}");
        }
        // SAFETY: plain syscall; a negative pid addresses the process group.
        let rc = unsafe { libc::kill(-(group as libc::pid_t), signal) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("signalling the agent process");
        }
        Ok(())
    }
}

/// First string found at any of the JSON pointers.
fn str_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|p| value.pointer(p).and_then(Value::as_str))
        .map(str::to_string)
}

/// Longest name herdr accepts (`[a-z][a-z0-9_-]{0,31}`).
const NAME_MAX: usize = 32;

/// The names to try for an agent, in order: its id, then the id qualified by
/// the team (taken from the workspace label, `crew:<project>`), then that
/// with a counter.
fn agent_names(agent_id: &str, label: &str) -> Vec<String> {
    let team: String = label
        .strip_prefix("crew:")
        .unwrap_or(label)
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let team = team.trim_matches('-');
    let mut names = vec![agent_id.to_string()];
    let qualified = format!("{agent_id}-{}", if team.is_empty() { "crew" } else { team });
    let fit = |name: &str, room: usize| -> String {
        let cut: String = name.chars().take(NAME_MAX - room).collect();
        cut.trim_end_matches('-').to_string()
    };
    names.push(fit(&qualified, 0));
    for n in 2..=9 {
        names.push(format!("{}-{n}", fit(&qualified, 2)));
    }
    names.dedup();
    names
}

fn herdr_kind(kind: RuntimeKind) -> Option<&'static str> {
    match kind {
        RuntimeKind::Claude => Some("claude"),
        RuntimeKind::Codex => Some("codex"),
        RuntimeKind::Pi => Some("pi"),
        RuntimeKind::Shell => None,
    }
}

/// herdr's agent.start takes only a kind (`pi`) and runs that executable as the
/// pane's shell finds it; it cannot be told a program. So the selected pi's
/// directory goes first on the pane's PATH, and after the start the pane's
/// foreground process is checked to be that program (`verify_program`).
fn pinned_to_selected_pi(spec: &LaunchSpec) -> LaunchSpec {
    let mut spec = spec.clone();
    if is_pinned(&spec) {
        if let Some(dir) = Path::new(&spec.program).parent() {
            let rest = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
            spec.env
                .push(("PATH".into(), format!("{}:{rest}", dir.display())));
        }
    }
    spec
}

/// A pi crew selected by path, which herdr has to be steered to.
fn is_pinned(spec: &LaunchSpec) -> bool {
    spec.kind == RuntimeKind::Pi && spec.program != "pi"
}

/// Whether process `pid` runs `program`: its executable is it, or the program
/// is one of its arguments (a script run by its interpreter).
fn process_runs(pid: u32, program: &Path) -> bool {
    let Ok(want) = program.canonicalize() else {
        return false;
    };
    let same = |p: &Path| p.canonicalize().is_ok_and(|p| p == want);
    if crate::os::process_exe(pid).is_some_and(|exe| same(&exe)) {
        return true;
    }
    crate::os::process_args(pid).is_some_and(|args| {
        args.iter()
            .any(|a| same(Path::new(&*String::from_utf8_lossy(a))))
    })
}

/// The shared handle is needed to spawn watcher tasks, so the trait is
/// implemented on `Arc<HerdrManager>`.
pub struct Herdr(pub Arc<HerdrManager>);

#[async_trait]
impl ProcessManager for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    async fn spawn(&self, spec: &LaunchSpec) -> Result<Spawned> {
        let mgr = &self.0;
        let pinned = pinned_to_selected_pi(spec);
        let spec = &pinned;
        if let Ok(old) = mgr.pane(&spec.agent_id) {
            if old.alive.load(Ordering::SeqCst) {
                // Restart: replace the old pane.
                old.alive.store(false, Ordering::SeqCst);
                let _ = mgr
                    .call("pane.close", json!({ "pane_id": old.pane_id }))
                    .await;
            }
        }
        let pane_id = mgr.create_pane(spec).await?;

        // herdr agent names are unique across the whole herdr session, and
        // every team has an "orchestrator".
        let mut names = agent_names(&spec.agent_id, &mgr.label).into_iter();
        let mut name = names.next().unwrap_or_else(|| spec.agent_id.clone());
        match herdr_kind(spec.kind) {
            Some(kind) => {
                // A fresh pane needs a moment before its shell is at a prompt.
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    let result = mgr
                        .call_with_timeout(
                            "agent.start",
                            json!({
                                "name": name,
                                "kind": kind,
                                "pane_id": pane_id,
                                "args": spec.args,
                                "timeout_ms": START_TIMEOUT_MS,
                            }),
                            Duration::from_millis(START_TIMEOUT_MS + 5_000),
                        )
                        .await;
                    match result {
                        Ok(_) => break,
                        Err(e) if e.to_string().contains("agent_name_taken") => {
                            match names.next() {
                                Some(next) => name = next,
                                None => {
                                    let _ =
                                        mgr.call("pane.close", json!({ "pane_id": pane_id })).await;
                                    return Err(e);
                                }
                            }
                        }
                        Err(e) if attempt < 8 && is_pane_not_ready(&e.to_string()) => {
                            tokio::time::sleep(Duration::from_millis(400)).await;
                        }
                        Err(e) => {
                            let _ = mgr.call("pane.close", json!({ "pane_id": pane_id })).await;
                            return Err(e);
                        }
                    }
                }
            }
            None => {
                // Not an agent herdr knows: type the command into the pane's
                // shell, quoting every argument.
                let mut line = shell_quote(&spec.program);
                for arg in &spec.args {
                    line.push(' ');
                    line.push_str(&shell_quote(arg));
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
                mgr.call(
                    "pane.send_input",
                    json!({ "pane_id": pane_id, "text": line, "keys": ["enter"] }),
                )
                .await?;
            }
        }

        if is_pinned(spec) {
            if let Err(e) = mgr.verify_program(&pane_id, Path::new(&spec.program)).await {
                let _ = mgr.call("pane.close", json!({ "pane_id": pane_id })).await;
                return Err(e);
            }
        }
        let pid = mgr.pane_pids(&pane_id).await.ok().and_then(|(pid, _)| pid);
        let pane = Pane {
            pane_id: pane_id.clone(),
            name,
            kind: spec.kind,
            alive: Arc::new(AtomicBool::new(true)),
        };
        mgr.state()
            .panes
            .insert(spec.agent_id.clone(), pane.clone());
        mgr.watch(spec.agent_id.clone(), pane);
        Ok(Spawned {
            pid,
            start: None,
            backend_ref: Some(pane_id),
            prompt_delivered: false,
            // herdr considers the agent started, but a start-up dialog (folder
            // trust) may still be in front of it; the daemon decides when the
            // session can take its first prompt.
            ready: false,
        })
    }

    async fn submit_prompt(&self, agent_id: &str, text: &str) -> Result<()> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        if !pane.alive.load(Ordering::SeqCst) {
            bail!("agent '{agent_id}' is not running");
        }
        if herdr_kind(pane.kind).is_some() {
            // `agent.prompt` submits text and Enter atomically, honouring the
            // pane's bracketed-paste mode.
            // Right after start-up herdr may not consider the agent ready
            // for input yet; give it a few moments.
            let mut attempt = 0;
            loop {
                attempt += 1;
                match mgr
                    .call("agent.prompt", json!({ "target": pane.name, "text": text }))
                    .await
                {
                    Ok(_) => break,
                    Err(e) if attempt < 12 && e.to_string().contains("agent_not_ready") => {
                        if !pane.alive.load(Ordering::SeqCst) {
                            bail!("agent '{agent_id}' stopped before the prompt was delivered");
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        } else {
            let line: String = text
                .chars()
                .map(|c| if c == '\n' || c.is_control() { ' ' } else { c })
                .collect();
            mgr.call(
                "pane.send_input",
                json!({ "pane_id": pane.pane_id, "text": line, "keys": ["enter"] }),
            )
            .await?;
        }
        Ok(())
    }

    async fn write_raw(&self, agent_id: &str, data: &[u8]) -> Result<()> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        mgr.call(
            "pane.send_text",
            json!({ "pane_id": pane.pane_id, "text": String::from_utf8_lossy(data) }),
        )
        .await?;
        Ok(())
    }

    async fn send_keys(&self, agent_id: &str, keys: &[&str]) -> Result<()> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        // herdr encodes named keys for the pane's terminal mode itself.
        mgr.call(
            "pane.send_keys",
            json!({ "pane_id": pane.pane_id, "keys": keys }),
        )
        .await?;
        Ok(())
    }

    async fn screen(&self, agent_id: &str, lines: usize) -> Result<String> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        // Past the screen herdr takes tens of milliseconds a line.
        let timeout = CALL_TIMEOUT + SCROLLBACK_LINE_TIME * lines as u32;
        let text = mgr
            .read_pane(
                &pane.pane_id,
                "recent_unwrapped",
                Some(lines),
                false,
                timeout,
            )
            .await?;
        Ok(text.trim_end().to_string())
    }

    async fn visible(&self, agent_id: &str, lines: usize) -> Result<String> {
        self.0.visible(agent_id, lines, false).await
    }

    async fn styled(&self, agent_id: &str, _lines: usize) -> Result<String> {
        // The dashboard's live view: the screen as it is, in one call (each
        // call to herdr takes about a tenth of a second).
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        let shown = mgr
            .read_pane(&pane.pane_id, "visible", None, true, CALL_TIMEOUT)
            .await?;
        Ok(shown.replace('\r', "").trim_end().to_string())
    }

    async fn pause(&self, agent_id: &str) -> Result<()> {
        self.0.signal_foreground(agent_id, libc::SIGSTOP).await
    }

    async fn resume(&self, agent_id: &str) -> Result<()> {
        self.0.signal_foreground(agent_id, libc::SIGCONT).await
    }

    async fn kill(&self, agent_id: &str) -> Result<()> {
        let mgr = &self.0;
        let Ok(pane) = mgr.pane(agent_id) else {
            return Ok(());
        };
        pane.alive.store(false, Ordering::SeqCst);
        match mgr
            .call("pane.close", json!({ "pane_id": pane.pane_id }))
            .await
        {
            Ok(_) => Ok(()),
            // Already gone is what we wanted.
            Err(e) if mgr.pane_gone(&pane.pane_id).await => {
                tracing::debug!("pane.close: {e:#}");
                Ok(())
            }
            Err(e) => {
                // Still there: what is asked of it next must not take it
                // for gone.
                pane.alive.store(true, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    fn is_alive(&self, agent_id: &str) -> bool {
        self.0
            .pane(agent_id)
            .map(|p| p.alive.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    async fn adopt(&self, agent: &Agent) -> Result<bool> {
        let mgr = &self.0;
        let Some(pane_id) = &agent.backend_ref else {
            return Ok(false);
        };
        let Ok(result) = mgr.call("pane.get", json!({ "pane_id": pane_id })).await else {
            return Ok(false);
        };
        // The pane exists; make sure the agent is still in it (and not just
        // the shell it was started from).
        let occupant = str_at(&result, &["/pane/agent"]);
        if herdr_kind(agent.runtime).is_some() && occupant.is_none() {
            return Ok(false);
        }
        // The name it was started under (see `agent_names`).
        let name = match mgr.call("agent.get", json!({ "target": pane_id })).await {
            Ok(info) => str_at(&info, &["/agent/name"]),
            Err(_) => None,
        };
        let pane = Pane {
            pane_id: pane_id.clone(),
            name: name.unwrap_or_else(|| agent.id.clone()),
            kind: agent.runtime,
            alive: Arc::new(AtomicBool::new(true)),
        };
        {
            let mut state = mgr.state();
            state.panes.insert(agent.id.clone(), pane.clone());
            state.last_pane = Some(pane_id.clone());
            state.tiles += 1;
        }
        mgr.watch(agent.id.clone(), pane);
        Ok(true)
    }

    async fn set_meta(&self, agent_id: &str, meta: &PaneMeta) -> Result<()> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        if !pane.alive.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut title = format!("{agent_id} · {}", meta.role);
        if let Some(task) = &meta.task {
            title.push_str(" · ");
            title.push_str(task);
        }
        let mut tokens = serde_json::Map::new();
        tokens.insert("role".into(), json!(meta.role));
        tokens.insert("crew".into(), json!(meta.status));
        tokens.insert(
            "progress".into(),
            match meta.progress {
                Some(p) => json!(format!("{p}%")),
                None => Value::Null,
            },
        );
        mgr.call(
            "pane.report_metadata",
            json!({
                "pane_id": pane.pane_id,
                "source": SOURCE,
                "title": title,
                "tokens": tokens,
            }),
        )
        .await?;
        Ok(())
    }

    async fn focus(&self, agent_id: &str) -> Result<()> {
        let mgr = &self.0;
        let pane = mgr.pane(agent_id)?;
        mgr.call("pane.focus", json!({ "pane_id": pane.pane_id }))
            .await?;
        Ok(())
    }

    fn session_ref(&self) -> Option<String> {
        self.0.state().workspace_id.clone()
    }
}

fn is_pane_not_ready(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("not available")
        || e.contains("not_available")
        || e.contains("not ready")
        || e.contains("not_ready")
        || e.contains("busy")
        || e.contains("prompt")
}

/// Is a herdr server listening on this socket?
pub async fn reachable(socket: &Path) -> bool {
    let probe = async {
        let stream = UnixStream::connect(socket).await.ok()?;
        let (read, mut write) = stream.into_split();
        write
            .write_all(b"{\"id\":\"crew:ping\",\"method\":\"ping\",\"params\":{}}\n")
            .await
            .ok()?;
        let mut line = String::new();
        BufReader::new(read).read_line(&mut line).await.ok()?;
        Some(!line.trim().is_empty())
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(3), probe).await,
        Ok(Some(true))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_names_stay_unique_across_teams_and_valid_for_herdr() {
        let valid = |name: &str| {
            let mut chars = name.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && name.len() <= NAME_MAX
                && chars
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        };
        let names = agent_names("orchestrator", "crew:hello-crew");
        assert_eq!(
            names[..3],
            [
                "orchestrator",
                "orchestrator-hello-crew",
                "orchestrator-hello-crew-2"
            ]
        );
        assert!(names.iter().all(|n| valid(n)), "{names:?}");
        // Long ids, long or odd project names and custom labels still fit.
        let long = agent_names(&"a".repeat(32), "crew:My.Project With Spaces");
        assert_eq!(long[0].len(), 32);
        assert!(long.iter().all(|n| valid(n)), "{long:?}");
        let unique: std::collections::HashSet<_> = long.iter().collect();
        assert!(unique.len() >= 9, "{long:?}");
        let odd = agent_names("dev", "crew:--");
        assert_eq!(odd[1], "dev-crew");
        assert!(agent_names("dev", "Payments Team").iter().all(|n| valid(n)));
    }

    #[test]
    fn json_helpers() {
        let v = json!({"root_pane": {"pane_id": "w1:p2"}, "text": "x"});
        assert_eq!(
            str_at(&v, &["/pane/pane_id", "/root_pane/pane_id"]).as_deref(),
            Some("w1:p2")
        );
        assert_eq!(str_at(&v, &["/missing"]), None);
        assert_eq!(herdr_kind(RuntimeKind::Codex), Some("codex"));
        assert_eq!(herdr_kind(RuntimeKind::Shell), None);
        assert!(is_pane_not_ready("pane_not_available: shell is busy"));
        assert!(!is_pane_not_ready("unknown agent kind"));
    }

    #[tokio::test]
    async fn unreachable_socket_is_reported_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nope.sock");
        assert!(!reachable(&socket).await);
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = HerdrManager::new(&socket, "crew:test", HerdrLayout::Tabs, None, tx);
        let err = mgr.call("ping", json!({})).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("connecting to herdr"),
            "{err:#}"
        );
    }

    /// A herdr that logs every method it is called with and answers
    /// `agent.prompt` with `prompt_error` (an error code), or with success.
    async fn fake_herdr(
        socket: &Path,
        prompt_error: Option<&'static str>,
    ) -> Arc<Mutex<Vec<String>>> {
        fake_herdr_with(socket, prompt_error, vec![]).await
    }

    /// As `fake_herdr`, answering the listed methods with the given results.
    async fn fake_herdr_with(
        socket: &Path,
        prompt_error: Option<&'static str>,
        canned: Vec<(&'static str, Value)>,
    ) -> Arc<Mutex<Vec<String>>> {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                if BufReader::new(read).read_line(&mut line).await.is_err() {
                    continue;
                }
                let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                let method = request["method"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push(method.clone());
                let body = match (method.as_str(), prompt_error) {
                    ("agent.prompt", Some(code)) => {
                        json!({ "id": request["id"], "error": { "code": code, "message": "x" } })
                    }
                    (method, _) => {
                        let result = canned
                            .iter()
                            .find(|(m, _)| *m == method)
                            .map_or(json!({}), |(_, r)| r.clone());
                        json!({ "id": request["id"], "result": result })
                    }
                };
                let mut out = serde_json::to_vec(&body).unwrap();
                out.push(b'\n');
                let _ = write.write_all(&out).await;
            }
        });
        log
    }

    fn herdr_with_pane(socket: &Path, kind: RuntimeKind) -> Herdr {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = Arc::new(HerdrManager::new(
            socket,
            "crew:test",
            HerdrLayout::Tabs,
            None,
            tx,
        ));
        mgr.state().panes.insert(
            "a".into(),
            Pane {
                pane_id: "w1:p1".into(),
                name: "a".into(),
                kind,
                alive: Arc::new(AtomicBool::new(true)),
            },
        );
        Herdr(mgr)
    }

    fn pi_spec(program: &str) -> LaunchSpec {
        LaunchSpec {
            agent_id: "a".into(),
            kind: RuntimeKind::Pi,
            program: program.into(),
            args: vec![],
            env: vec![],
            cwd: "/".into(),
            prompt: None,
            prompt_as_arg: true,
            sidecar: None,
            sandbox: None,
        }
    }

    /// A fake herdr that starts a pane whose foreground process is `pid`.
    async fn fake_herdr_running(socket: &Path, pid: u32) -> Arc<Mutex<Vec<String>>> {
        fake_herdr_with(
            socket,
            None,
            vec![
                (
                    "workspace.create",
                    json!({ "workspace": { "workspace_id": "w1" }, "root_pane": { "pane_id": "w1:p1" } }),
                ),
                (
                    "pane.process_info",
                    json!({ "foreground_processes": [{ "pid": pid }], "foreground_process_group_id": pid }),
                ),
            ],
        )
        .await
    }

    #[tokio::test]
    async fn herdr_fails_closed_when_the_pane_runs_another_pi() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("h.sock");
        // The pane's foreground process is this test binary, not the selected pi.
        let log = fake_herdr_running(&socket, std::process::id()).await;
        let herdr = herdr_with_pane(&socket, RuntimeKind::Pi);
        let selected = dir.path().join("pi");
        std::fs::write(&selected, "#!/bin/sh\n").unwrap();
        let err = herdr
            .spawn(&pi_spec(&selected.to_string_lossy()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("a pi other than"), "{err}");
        assert!(log.lock().unwrap().iter().any(|m| m == "pane.close"));
    }

    #[tokio::test]
    async fn herdr_accepts_the_pane_running_the_selected_pi_and_puts_its_dir_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("h.sock");
        let selected = dir.path().join("pi");
        crate::agents::write_executable(&selected, "#!/bin/sh\nsleep 30\n");
        let mut child = std::process::Command::new(&selected).spawn().unwrap();
        let _log = fake_herdr_running(&socket, child.id()).await;
        let herdr = herdr_with_pane(&socket, RuntimeKind::Pi);
        let spec = pi_spec(&selected.to_string_lossy());
        let ok = herdr.spawn(&spec).await;
        let _ = child.kill();
        let _ = child.wait();
        assert!(ok.is_ok(), "{:?}", ok.err());
        // The pane is told to find the selected pi first.
        let pinned = pinned_to_selected_pi(&spec);
        let path = pinned.env.iter().find(|(k, _)| k == "PATH").unwrap();
        assert!(path.1.starts_with(&dir.path().display().to_string()));
        // A bare `pi` is left to the pane's own PATH.
        assert!(pinned_to_selected_pi(&pi_spec("pi")).env.is_empty());
    }

    #[tokio::test]
    async fn an_agent_that_is_never_ready_does_not_get_text_typed_into_its_shell() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("h.sock");
        let log = fake_herdr(&socket, Some("agent_not_ready")).await;
        let herdr = herdr_with_pane(&socket, RuntimeKind::Claude);
        let result = herdr.submit_prompt("a", "x; touch /tmp/pwned").await;
        assert!(result.is_err(), "delivery must fail, not fall back");
        let calls = log.lock().unwrap().clone();
        assert_eq!(calls.iter().filter(|m| *m == "agent.prompt").count(), 12);
        assert!(!calls.iter().any(|m| m == "pane.send_input"), "{calls:?}");
    }
}
