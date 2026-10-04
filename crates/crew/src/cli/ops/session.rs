//! The daemon and the session as a whole: `crew start`, `attach`, `sessions`,
//! `status`, `resume`, `frame`, `stop`.

use std::io::IsTerminal;

use anyhow::{anyhow, bail, Result};
use crew_core::paths::{self};
use crew_core::rpc::{Call, Snapshot};
use crew_core::util::{fmt_elapsed, now_ms, one_line};
use crew_core::{Agent, RuntimeKind, TaskStatus};
use serde_json::Value;

use super::super::daemonize;
use super::team::{dashboard, landing_hint, stale_daemon_note, wait_for_exit};
use super::Target;
use crate::limits::fmt_when;
use crate::mcp::tools::{render_agents, render_tasks};
use crate::models::Catalog;

pub fn start(target: &Target) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let info = daemonize::ensure_daemon(&workspace)?;
    println!("daemon running (pid {}) for {}", info.pid, info.workspace);
    Ok(())
}

pub fn attach(target: &Target) -> Result<()> {
    let info = target.discover()?;
    if !std::io::stdout().is_terminal() {
        bail!("the dashboard needs a terminal; use `crew status` for plain output");
    }
    dashboard(&info).map(|_| ())
}

pub fn sessions() -> Result<()> {
    let sessions = paths::list_sessions();
    if sessions.is_empty() {
        println!("no crew sessions are running");
        return Ok(());
    }
    for s in sessions {
        println!(
            "{:<24} pid {:<8} up {:<8} {}",
            s.project.as_deref().unwrap_or("<no team>"),
            s.pid,
            fmt_elapsed(now_ms() - s.started_at),
            s.workspace
        );
    }
    Ok(())
}

pub fn status(target: &Target, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(0),
        events: Some(0),
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let s = &snapshot.session;
    match &s.project {
        Some(project) => println!(
            "project    {project}  (backend {}, isolation {})",
            s.backend, s.isolation
        ),
        None => println!("project    <no team loaded>"),
    }
    if let Some(style) = &s.style {
        println!("style      {style}");
    }
    println!("workspace  {}", s.workspace);
    println!(
        "daemon     pid {}, up {}",
        s.daemon_pid,
        fmt_elapsed(snapshot.server_time - s.daemon_started_at)
    );
    let done = snapshot
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    println!("tasks      {done}/{} done", snapshot.tasks.len());
    if s.completed_at.is_some() {
        match snapshot.questions.len() {
            0 => println!("state      all tasks done"),
            n => println!("state      all tasks done · waiting for your answer to {n} question(s)"),
        }
    }
    if landing_hint(&snapshot).is_some() {
        println!("next       crew integrate && git merge agents/integration   (lands the agents' branches)");
    }
    if let Some(summary) = &s.summary {
        println!("summary    {}", one_line(summary));
    }
    for line in usage_lines(&snapshot) {
        println!("{line}");
    }
    if let Some(note) = target.discover().ok().as_ref().and_then(stale_daemon_note) {
        println!("note       {note}");
    }
    match snapshot.questions.len() {
        0 => {}
        n => println!("questions  {n} waiting for your answer — see `crew questions`"),
    }
    println!("\nAGENTS\n{}", render_agents(&snapshot.agents).trim_end());
    println!("\nTASKS\n{}", render_tasks(&snapshot.tasks).trim_end());
    Ok(())
}

/// `crew resume`: continue agents paused at a usage limit.
pub fn resume(target: &Target, agents: Vec<String>, restart: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let reply: Value = client.call(Call::Resume { agents, restart })?;
    let resumed: Vec<&str> = reply
        .get("resumed")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if resumed.is_empty() {
        println!("no agent is paused at a usage limit");
    } else if restart {
        println!("restarted, resuming their sessions: {}", resumed.join(", "));
    } else {
        println!("told to continue: {}", resumed.join(", "));
        println!("(after switching account or key, use `crew resume --restart` instead)");
    }
    Ok(())
}

/// A usage figure older than this is said to be old.
const STALE_QUOTA_MS: i64 = 20 * 60_000;

fn usage_lines(snapshot: &Snapshot) -> Vec<String> {
    let now = snapshot.server_time;
    let mut lines = Vec::new();
    let mut label = "usage";
    // The window names make a column: as wide as the widest.
    let name_w = snapshot
        .session
        .quotas
        .iter()
        .map(|q| crate::limits::window_label(&q.window).chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    for runtime in [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi] {
        let mut name = runtime.to_string();
        for q in snapshot
            .session
            .quotas
            .iter()
            .filter(|q| q.runtime == runtime)
        {
            const CELLS: usize = 24;
            let percent = q.percent_used.clamp(0.0, 100.0);
            let mut filled = ((percent / 100.0) * CELLS as f32).round() as usize;
            if percent > 0.0 {
                filled = filled.max(1);
            }
            let window = crate::limits::window_label(&q.window);
            let resets = q
                .resets_at
                .filter(|at| *at > now)
                .map(|at| format!(" · resets {}", fmt_when(at, now)))
                .unwrap_or_default();
            // A figure that is not from a session just now says how old it is.
            let age = if now - q.updated_at > STALE_QUOTA_MS {
                format!(" · as of {} ago", fmt_elapsed(now - q.updated_at))
            } else {
                String::new()
            };
            lines.push(format!(
                "{label:<10} {name:<7} {window:<name_w$} {}{}  {:>3.0}% used{resets}{age}",
                "█".repeat(filled),
                "░".repeat(CELLS - filled),
                q.percent_used.max(0.0),
            ));
            // The label and the runtime are written once.
            label = "";
            name.clear();
        }
    }
    // Sessions moved to another model than their member was given.
    for a in snapshot.agents.iter().filter(|a| a.fallback.is_some()) {
        let fallback = a.fallback.as_ref().unwrap();
        let running = a
            .model_label()
            .unwrap_or_else(|| "another model".to_string());
        let why = if fallback.note.is_empty() {
            String::new()
        } else {
            format!(" — {}", fallback.note)
        };
        lines.push(if fallback.policy {
            format!(
                "fallback   {} runs on {running} instead of {}{why}; it goes back by itself",
                a.id, fallback.from
            )
        } else {
            format!(
                "fallback   {} runs on {running} instead of {}{why} · crew agent model {} <model> to choose",
                a.id, fallback.from, a.id
            )
        });
    }
    let parked: Vec<&Agent> = snapshot
        .agents
        .iter()
        .filter(|a| a.limit.is_some())
        .collect();
    if !parked.is_empty() {
        let ids: Vec<&str> = parked.iter().map(|a| a.id.as_str()).collect();
        let next = parked
            .iter()
            .filter_map(|a| a.limit.as_ref()?.retry_at)
            .min();
        let when = match next {
            Some(at) => format!("next automatic resume {}", fmt_when(at, now)),
            None => "waiting for you".to_string(),
        };
        lines.push(format!(
            "limits     {} paused at a usage limit ({}) — {when}",
            ids.len(),
            ids.join(", ")
        ));
        lines.push(
            "           continue now: crew resume · after switching account or key: crew resume --restart"
                .to_string(),
        );
    }
    lines
}

/// Render the dashboard once, as text: the same view `crew attach` shows,
/// for terminals that cannot host it (logs, CI, documentation).
pub fn frame(target: &Target, size: &str) -> Result<()> {
    let (width, height) = size
        .split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
        .filter(|(w, h)| *w >= 20 && *h >= 8)
        .ok_or_else(|| anyhow!("--frame expects WIDTHxHEIGHT, e.g. 120x40"))?;
    let (mut client, _) = target.connect("cli")?;
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(300),
        events: Some(400),
    })?;
    let now = snapshot.server_time;
    let mut app = crew_tui::App::new();
    app.model_names = Catalog::load().display_names();
    app.apply_snapshot(snapshot, now);
    println!("{}", crew_tui::render_to_string(&app, width, height, now));
    Ok(())
}

pub fn stop(target: &Target, keep_agents: bool) -> Result<()> {
    let (mut client, info) = target.connect("cli")?;
    match client.call_value(Call::Shutdown {
        stop_agents: !keep_agents,
    }) {
        Ok(_) => {}
        // The daemon can be gone before its answer got through; it was
        // told to stop, and whether it did is checked below.
        Err(
            crew_core::client::ClientError::Closed
            | crew_core::client::ClientError::Io(_)
            | crew_core::client::ClientError::Timeout,
        ) => {}
        Err(e) => return Err(e.into()),
    }
    // Wait for the daemon to go away so a following `crew run` starts clean.
    wait_for_exit(info.pid);
    if paths::pid_alive(info.pid) {
        bail!(
            "the crew daemon (pid {}) was told to stop but is still running",
            info.pid
        );
    }
    println!(
        "stopped the crew daemon for {}{}",
        info.workspace,
        if keep_agents {
            " (agents left running)"
        } else {
            ""
        }
    );
    Ok(())
}
