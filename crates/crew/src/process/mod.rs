//! Process management: where agent processes actually run.
//!
//! `ProcessManager` hides the host. Two implementations exist:
//! - `pty::PtyManager` — the daemon owns a pseudo-terminal per agent;
//! - `herdr::HerdrManager` — each agent is a named agent in a herdr pane, so
//!   it shows up in the herdr panel and the user can watch or take over.
//!
//! Either way an agent is a real interactive CLI session on a real terminal.

use anyhow::Result;
use async_trait::async_trait;
use crew_core::Agent;
use tokio::sync::broadcast;

use crate::agents::LaunchSpec;

pub mod herdr;
pub mod pty;

/// Things a backend observes about a process.
#[derive(Clone, Debug, PartialEq)]
pub enum ProcEvent {
    /// The process (or its pane) is gone.
    Exited {
        agent_id: String,
        /// Which incarnation of the agent this was (pid or pane id), so an
        /// exit that arrives after a restart is not mistaken for the new
        /// process dying.
        instance: String,
        code: Option<i32>,
        detail: String,
    },
    /// The terminal produced output (throttled; a liveness tick, not a stream).
    Output { agent_id: String },
    /// The backend's own reading of the agent's state (herdr detects this).
    Hint { agent_id: String, hint: Hint },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Idle,
    Working,
    Blocked,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spawned {
    pub pid: Option<u32>,
    /// The process's start time (clock ticks after boot), read before
    /// anything could reap it, so `pid` and `start` name this process and no
    /// later one with the same number. None where it is not known.
    pub start: Option<u64>,
    /// Backend handle to persist (the herdr pane id).
    pub backend_ref: Option<String>,
    /// The first prompt was passed on the command line.
    pub prompt_delivered: bool,
    /// The session already accepts input.
    pub ready: bool,
}

/// Display metadata a backend may surface in its own UI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneMeta {
    pub role: String,
    pub task: Option<String>,
    pub status: String,
    pub progress: Option<u8>,
}

/// A live view of an agent's terminal for `crew agent attach`.
pub struct Attachment {
    /// Escape sequences that repaint the current screen.
    pub snapshot: Vec<u8>,
    pub output: broadcast::Receiver<Vec<u8>>,
}

#[async_trait]
pub trait ProcessManager: Send + Sync {
    /// `pty` or `herdr`.
    fn name(&self) -> &'static str;

    /// Start the agent's process. Arguments are passed as an array; no shell
    /// is involved unless the backend is itself a shell pane.
    async fn spawn(&self, spec: &LaunchSpec) -> Result<Spawned>;

    /// Submit text as one prompt (paste, then Enter).
    async fn submit_prompt(&self, agent_id: &str, text: &str) -> Result<()>;

    /// Type raw bytes into the terminal.
    async fn write_raw(&self, agent_id: &str, data: &[u8]) -> Result<()>;

    /// Press logical keys (`enter`, `down`, `up`, `esc`, `tab`, or a single
    /// character), one at a time.
    async fn send_keys(&self, agent_id: &str, keys: &[&str]) -> Result<()> {
        for key in keys {
            self.write_raw(agent_id, key_bytes(key).as_bytes()).await?;
        }
        Ok(())
    }

    /// Rendered terminal contents (the last `lines` lines).
    async fn screen(&self, agent_id: &str, lines: usize) -> Result<String>;

    /// What the terminal shows now: at most `lines` lines, and never more
    /// than can be read at once. This is what gets polled (dialogs, limit
    /// notices, the dashboard's live view), so a backend that is slow to
    /// read its scrollback answers from the screen alone.
    async fn visible(&self, agent_id: &str, lines: usize) -> Result<String> {
        self.screen(agent_id, lines).await
    }

    /// Like `visible`, with the terminal's colours and attributes left in
    /// as SGR escape codes. A backend that cannot say gives plain text.
    async fn styled(&self, agent_id: &str, lines: usize) -> Result<String> {
        self.visible(agent_id, lines).await
    }

    /// Stop the process without killing it (SIGSTOP).
    async fn pause(&self, agent_id: &str) -> Result<()>;

    async fn resume(&self, agent_id: &str) -> Result<()>;

    /// Terminate the process and release its terminal.
    async fn kill(&self, agent_id: &str) -> Result<()>;

    fn is_alive(&self, agent_id: &str) -> bool;

    /// After a daemon restart: take over a process that outlived the daemon.
    /// Returns false when it is gone (always, for daemon-owned PTYs).
    async fn adopt(&self, agent: &Agent) -> Result<bool>;

    async fn set_meta(&self, _agent_id: &str, _meta: &PaneMeta) -> Result<()> {
        Ok(())
    }

    /// Bring the agent's terminal to the user's attention.
    async fn focus(&self, _agent_id: &str) -> Result<()> {
        anyhow::bail!(
            "the {} backend has no panes to focus; use `crew agent attach`",
            self.name()
        )
    }

    /// Stream the terminal for a raw attachment.
    fn attach(&self, _agent_id: &str) -> Result<Attachment> {
        anyhow::bail!(
            "attach is not available on the {} backend; focus the agent's pane instead",
            self.name()
        )
    }

    async fn resize(&self, _agent_id: &str, _rows: u16, _cols: u16) -> Result<()> {
        Ok(())
    }

    /// Backend-wide handle worth persisting (the herdr workspace id).
    fn session_ref(&self) -> Option<String> {
        None
    }
}

/// The bytes a terminal sends for a logical key.
pub fn key_bytes(key: &str) -> &str {
    match key {
        "enter" => "\r",
        "down" => "\x1b[B",
        "up" => "\x1b[A",
        "esc" => "\x1b",
        "tab" => "\t",
        other => other,
    }
}

/// Wrap text for submission to an interactive TUI. With bracketed paste the
/// text arrives as one paste (newlines intact); without it, newlines would
/// submit early, so they are flattened.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    // Strip control characters that could act as keystrokes (ESC in
    // particular could close the paste early or trigger shortcuts).
    let clean: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    if bracketed {
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        clean.replace('\n', " ").into_bytes()
    }
}

/// What a `/proc/<pid>/stat` line says about its process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    /// R, S, D, Z (a zombie), X (dead), ...
    pub state: char,
    pub ppid: u32,
    /// Clock ticks after boot.
    pub start: u64,
}

/// Fields 3 (state), 4 (parent) and 22 (start time) of a `/proc/<pid>/stat`
/// line. The command name before them is in parentheses and may hold
/// anything, so the fields are counted from the last `)`.
pub(crate) fn parse_stat(stat: &str) -> Option<Stat> {
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    Some(Stat {
        state: fields.first()?.chars().next()?,
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// What `/proc` says about process `pid` now, if it has one.
pub(crate) fn stat(pid: u32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stat_line_is_read_past_any_command_name() {
        let tail = "S 7 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 20 21";
        let want = Some(Stat {
            state: 'S',
            ppid: 7,
            start: 987654,
        });
        assert_eq!(parse_stat(&format!("42 (sleep) {tail}")), want);
        // A command named `a) b (c`.
        assert_eq!(parse_stat(&format!("42 (a) b (c) {tail}")), want);
        assert_eq!(parse_stat("42 (sleep) S 1"), None);
    }

    #[test]
    fn paste_wraps_and_sanitises() {
        assert_eq!(paste_bytes("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(paste_bytes("a\nb", false), b"a b");
        // An embedded ESC (or end-of-paste marker) cannot escape the paste.
        assert_eq!(
            paste_bytes("x\x1b[201~y\x03", true),
            b"\x1b[200~x[201~y\x1b[201~"
        );
    }
}
