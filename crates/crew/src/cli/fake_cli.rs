//! `crew fake-cli`: stand-ins for the agent CLIs, for tests that launch
//! sandboxed agents without Claude Code, Codex or pi.
//!
//! Tests put small wrapper scripts named `claude`, `codex` and `pi` on `PATH`
//! that `exec` this subcommand. Each fake does what crew relies on its CLI
//! for: it keeps its files where the real one would (the configuration
//! directory the environment names, a transcript or rollout there, pi's
//! `--session-dir`), tells the daemon about its session the way the real
//! CLI's hooks do, and stays up reading its terminal. `codex app-server`
//! binds the socket it is asked to listen on. What the fake saw (arguments
//! and environment) is written next to its files, so a test can check it.

use std::io::{BufRead, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use serde_json::{json, Value};

#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    Claude,
    Codex,
    Pi,
}

#[derive(Args)]
pub struct FakeArgs {
    kind: Kind,
    /// The arguments the CLI was started with
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub fn run(args: FakeArgs) -> Result<()> {
    match args.kind {
        Kind::Claude => claude(&args.args),
        Kind::Codex => codex(&args.args),
        Kind::Pi => pi(&args.args),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir(var: &str, default: &str) -> Result<PathBuf> {
    env_path(var)
        .or_else(|| env_path("HOME").map(|h| h.join(default)))
        .context("neither the CLI's home variable nor HOME is set")
}

/// The value after `flag`, if there is one.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).map(String::as_str)
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// What this process was started with, for the test to read.
fn record(dir: &Path, name: &str, args: &[String]) -> Result<()> {
    let env: serde_json::Map<String, Value> =
        std::env::vars().map(|(k, v)| (k, json!(v))).collect();
    write(
        &dir.join(name),
        &serde_json::to_string_pretty(&json!({
            "args": args,
            "env": env,
            "cwd": std::env::current_dir().ok(),
        }))?,
    )
}

fn hook(event: &str, payload: Value) {
    let identity = env_path("CREW_IDENTITY");
    // Like the real CLIs, a stand-in goes on after a hook that failed.
    let _ = super::agent_side::hook(event, identity.as_deref(), Some(payload.to_string()));
}

/// Keep the terminal session up, echoing what is typed into `reply`.
fn stay_up(mut reply: impl FnMut(&str)) {
    for line in std::io::stdin().lock().lines() {
        match line {
            Ok(line) => reply(&line),
            Err(_) => break,
        }
    }
}

fn claude(args: &[String]) -> Result<()> {
    let dir = home_dir("CLAUDE_CONFIG_DIR", ".claude")?;
    record(&dir, "fake-claude.json", args)?;
    // And one per agent (--name), for agents that share a configuration directory.
    if let Some(name) = flag(args, "--name") {
        record(&dir, &format!("fake-claude-{name}.json"), args)?;
    }
    let session = flag(args, "--session-id")
        .or_else(|| flag(args, "--resume"))
        .unwrap_or("fake-session");
    let transcript = dir.join("projects/-fake").join(format!("{session}.jsonl"));
    let line = |kind: &str, role: &str, text: &str| {
        json!({ "type": kind, "timestamp": "2026-10-03T12:00:00Z",
                "message": { "role": role, "content": [{ "type": "text", "text": text }] } })
        .to_string()
    };
    write(
        &transcript,
        &format!("{}\n", line("user", "user", "hello from the fake")),
    )?;
    // `--fake-report-transcript <path>`: tell the daemon a different path, as a
    // hostile agent could, for tests of what the daemon is willing to open.
    let reported = flag(args, "--fake-report-transcript").map_or(transcript.clone(), PathBuf::from);
    hook(
        "session-start",
        json!({ "session_id": session, "transcript_path": reported, "model": "fake" }),
    );
    println!("fake claude ready");
    let _ = std::io::stdout().flush();
    stay_up(|typed| {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        let _ = writeln!(
            file,
            "{}",
            line("assistant", "assistant", &format!("echo: {typed}"))
        );
        println!("fake claude got: {typed}");
    });
    Ok(())
}

fn codex(args: &[String]) -> Result<()> {
    let dir = home_dir("CODEX_HOME", ".codex")?;
    if args.first().map(String::as_str) == Some("app-server") {
        record(&dir, "fake-codex-server.json", args)?;
        let Some(listen) = flag(args, "--listen").and_then(|l| l.strip_prefix("unix://")) else {
            bail!("codex app-server needs --listen unix://<socket>");
        };
        let listener = UnixListener::bind(listen).with_context(|| format!("binding {listen}"))?;
        // The rollout is written by whoever owns the session: the server.
        write(
            &dir.join("sessions/2026/10/03/rollout-2026-10-03T12-00-00-fake-thread.jsonl"),
            &format!(
                "{}\n",
                json!({ "type": "response_item", "timestamp": "2026-10-03T12:00:00Z",
                        "payload": { "type": "message", "role": "assistant",
                                     "content": [{ "type": "output_text", "text": "hello from the fake" }] } })
            ),
        )?;
        for stream in listener.incoming() {
            drop(stream);
        }
        return Ok(());
    }
    record(&dir, "fake-codex.json", args)?;
    println!("fake codex ready");
    let _ = std::io::stdout().flush();
    stay_up(|typed| println!("fake codex got: {typed}"));
    Ok(())
}

fn pi(args: &[String]) -> Result<()> {
    let dir = home_dir("PI_CODING_AGENT_DIR", ".pi/agent")?;
    record(&dir, "fake-pi.json", args)?;
    let sessions = flag(args, "--session-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("sessions"));
    let session = flag(args, "--session-id").unwrap_or("fake-session");
    let file = sessions.join(format!("2026-10-03T12-00-00_{session}.jsonl"));
    write(
        &file,
        &format!(
            "{}\n",
            json!({ "type": "message", "timestamp": "2026-10-03T12:00:00Z",
                    "message": { "role": "user", "content": "hello from the fake" } })
        ),
    )?;
    hook(
        "session-start",
        json!({ "session_id": session, "transcript_path": file, "model": "fake" }),
    );
    println!("fake pi ready");
    let _ = std::io::stdout().flush();
    stay_up(|typed| println!("fake pi got: {typed}"));
    Ok(())
}
