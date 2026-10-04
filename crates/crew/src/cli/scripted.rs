//! `crew scripted-agent`: a deterministic stand-in for a coding agent.
//!
//! It is a real agent process as far as crew is concerned — it runs on its
//! own terminal, authenticates with its own token, and uses the same bus,
//! task and git calls as Claude Code or Codex — but it follows a script
//! instead of a model. That makes whole-team runs reproducible and free: the
//! test suite uses it as its mock runtime, and `examples/dry-run.yaml` uses
//! it to exercise the dashboard without spending tokens.
//!
//! The script is a list of steps per trigger (`--on-start`, `--on-task`,
//! `--on-message`, `--on-review`), each a small command:
//!
//! ```text
//! sleep <ms>                     progress <pct> [note]
//! status <status> [note]         send <to> <type> <body…>
//! broadcast <body…>              reply <body…>
//! write <file> <content…>        commit <message…>
//! publish <to> [text…]           cherry-pick
//! merge <agent>                  wait-message <from>
//! spawn <task…>                  review approve|reject [comments…]
//! done [result…]                 fail <reason…>
//! exit <code>                    crash-once <marker-file>
//! tool <name> <json-args>
//! ```
//!
//! `{commit}`, `{from}`, `{task}`, `{body}` and `{me}` are substituted.

use std::collections::VecDeque;
use std::io::BufRead;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::Args;
use crew_core::client::Client;
use crew_core::paths::Identity;
use crew_core::rpc::{Call, InboxResult};
use crew_core::{Message, MessageType};
use serde_json::{json, Value};

use crate::mcp::tools;

#[derive(Args, Clone, Default)]
pub struct ScriptArgs {
    /// Step to run once at start-up (repeatable)
    #[arg(long = "on-start", value_name = "STEP")]
    pub on_start: Vec<String>,
    /// Step to run when a task arrives (repeatable)
    #[arg(long = "on-task", value_name = "STEP")]
    pub on_task: Vec<String>,
    /// Step to run when any other message arrives (repeatable)
    #[arg(long = "on-message", value_name = "STEP")]
    pub on_message: Vec<String>,
    /// Step to run when a review request arrives (repeatable)
    #[arg(long = "on-review", value_name = "STEP")]
    pub on_review: Vec<String>,
    /// Step to run for a task when this agent is a subagent (repeatable);
    /// subagents inherit their parent's command, so this is how the same
    /// script behaves differently for them
    #[arg(long = "on-subtask", value_name = "STEP")]
    pub on_subtask: Vec<String>,
}

struct Agent {
    me: String,
    client: Client,
    stdin: std::io::Lines<std::io::StdinLock<'static>>,
    /// Messages fetched but not handled yet.
    backlog: VecDeque<Message>,
    last_commit: Option<String>,
    /// This agent was launched by another agent.
    is_subagent: bool,
    args: ScriptArgs,
}

/// What a step's placeholders refer to.
#[derive(Default, Clone)]
struct Trigger {
    from: String,
    body: String,
    task: String,
    commit: Option<String>,
}

impl Trigger {
    fn of(message: &Message) -> Self {
        let meta = |key: &str| {
            message
                .metadata
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Self {
            from: message.from.clone(),
            body: message.body.clone(),
            task: meta("task_id").unwrap_or_default(),
            commit: meta("commit").or_else(|| find_commit(&message.body)),
        }
    }
}

/// The first hex word that looks like a commit id.
fn find_commit(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| (7..=40).contains(&w.len()) && w.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

fn split_step(step: &str) -> (String, String) {
    let step = step.trim();
    match step.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb.to_string(), rest.trim().to_string()),
        None => (step.to_string(), String::new()),
    }
}

/// Split off the first `n` whitespace-separated words; the rest stays intact.
fn take_words(rest: &str, n: usize) -> (Vec<String>, String) {
    let mut words = Vec::new();
    let mut remaining = rest.trim();
    for _ in 0..n {
        match remaining.split_once(char::is_whitespace) {
            Some((word, tail)) => {
                words.push(word.to_string());
                remaining = tail.trim_start();
            }
            None if !remaining.is_empty() => {
                words.push(remaining.to_string());
                remaining = "";
            }
            None => break,
        }
    }
    (words, remaining.to_string())
}

impl Agent {
    fn tool(&mut self, name: &str, args: Value) -> Result<tools::ToolOutput> {
        let output =
            tools::call(&mut self.client, name, &args).map_err(|e| anyhow!("{name}: {e}"))?;
        println!(
            "[{}] {name}: {}",
            self.me,
            output.text.lines().next().unwrap_or("")
        );
        Ok(output)
    }

    fn hook(&mut self, event: &str) {
        let _ = self.client.call_value(Call::Hook {
            event: event.to_string(),
            payload: Value::Null,
        });
    }

    fn fetch(&mut self) -> Result<()> {
        let inbox: InboxResult = self.client.call(Call::Inbox {
            agent: None,
            unread_only: true,
            ack: true,
            limit: Some(100),
        })?;
        self.backlog
            .extend(inbox.messages.into_iter().map(|e| e.message));
        Ok(())
    }

    /// Block until the daemon announces new messages on our stdin.
    /// Returns false when stdin closes (the daemon let go of us).
    fn wait_for_nudge(&mut self) -> Result<bool> {
        match self.stdin.next() {
            Some(line) => {
                line?;
                self.fetch()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn expand(&self, text: &str, trigger: &Trigger) -> String {
        text.replace("{me}", &self.me)
            .replace("{from}", &trigger.from)
            .replace("{task}", &trigger.task)
            .replace("{body}", &trigger.body)
            .replace(
                "{commit}",
                self.last_commit
                    .as_deref()
                    .or(trigger.commit.as_deref())
                    .unwrap_or(""),
            )
    }

    fn run_steps(&mut self, steps: &[String], trigger: &Trigger) -> Result<()> {
        let mut trigger = trigger.clone();
        for step in steps {
            let (verb, rest) = split_step(step);
            let rest = self.expand(&rest, &trigger);
            match verb.as_str() {
                "sleep" => {
                    let ms: u64 = rest.parse().context("sleep <ms>")?;
                    std::thread::sleep(Duration::from_millis(ms));
                }
                "progress" => {
                    let (words, note) = take_words(&rest, 1);
                    let percent: u64 = words
                        .first()
                        .ok_or_else(|| anyhow!("progress <pct> [note]"))?
                        .parse()
                        .context("progress <pct>")?;
                    let note = (!note.is_empty()).then_some(note);
                    self.tool(
                        "task_update",
                        json!({ "percent": percent, "message": note }),
                    )?;
                }
                "status" => {
                    let (words, note) = take_words(&rest, 1);
                    let status = words
                        .first()
                        .ok_or_else(|| anyhow!("status <status> [note]"))?;
                    let note = (!note.is_empty()).then_some(note);
                    self.tool("agent_status", json!({ "status": status, "note": note }))?;
                }
                "send" => {
                    let (words, body) = take_words(&rest, 2);
                    let [to, kind] = words.as_slice() else {
                        bail!("send <to> <type> <body>");
                    };
                    self.tool(
                        "agent_send",
                        json!({ "to": to, "type": kind, "body": body }),
                    )?;
                }
                "broadcast" => {
                    self.tool("agent_broadcast", json!({ "body": rest }))?;
                }
                "reply" => {
                    if trigger.from.is_empty() {
                        bail!("reply: there is no message to reply to");
                    }
                    self.tool(
                        "agent_send",
                        json!({ "to": trigger.from, "type": "answer", "body": rest }),
                    )?;
                }
                "write" => {
                    let (words, content) = take_words(&rest, 1);
                    let file = words
                        .first()
                        .ok_or_else(|| anyhow!("write <file> <content>"))?;
                    let path = std::path::Path::new(file);
                    if path.is_absolute() || file.split('/').any(|part| part == "..") {
                        bail!("write: '{file}' must stay inside the working directory");
                    }
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, format!("{content}\n"))?;
                    println!("[{}] wrote {file}", self.me);
                }
                "commit" => {
                    let output = self.tool("git_commit", json!({ "message": rest }))?;
                    self.last_commit = output
                        .raw
                        .get("sha")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                "publish" => {
                    let (words, text) = take_words(&rest, 1);
                    let to = words
                        .first()
                        .ok_or_else(|| anyhow!("publish <to> [text]"))?;
                    let commit = self
                        .last_commit
                        .clone()
                        .ok_or_else(|| anyhow!("publish: nothing has been committed yet"))?;
                    let text = if text.is_empty() {
                        "My work is committed.".to_string()
                    } else {
                        text
                    };
                    self.tool(
                        "agent_send",
                        json!({
                            "to": to,
                            "type": "artifact",
                            "body": format!("{text}\nCommit: {commit}"),
                            "metadata": { "commit": commit },
                        }),
                    )?;
                }
                "cherry-pick" => {
                    let commit = if rest.is_empty() {
                        trigger
                            .commit
                            .clone()
                            .ok_or_else(|| anyhow!("cherry-pick: the message names no commit"))?
                    } else {
                        rest.clone()
                    };
                    self.tool("git_cherry_pick", json!({ "commit": commit }))?;
                }
                "merge" => {
                    self.tool("git_merge", json!({ "from_agent": rest }))?;
                }
                "wait-message" => {
                    let from = rest.clone();
                    loop {
                        let found = self
                            .backlog
                            .iter()
                            .position(|m| from.is_empty() || m.from == from);
                        if let Some(at) = found {
                            let message = self.backlog.remove(at).expect("index is valid");
                            println!("[{}] got the message from {}", self.me, message.from);
                            trigger = Trigger::of(&message);
                            break;
                        }
                        if !self.wait_for_nudge()? {
                            bail!("stdin closed while waiting for a message from '{from}'");
                        }
                    }
                }
                "spawn" => {
                    self.tool("agent_spawn", json!({ "task": rest }))?;
                }
                "review" => {
                    let (words, comments) = take_words(&rest, 1);
                    let approve = match words.first().map(String::as_str) {
                        Some("approve") => true,
                        Some("reject") => false,
                        _ => bail!("review approve|reject [comments]"),
                    };
                    self.tool(
                        "review_submit",
                        json!({ "task_id": trigger.task, "approve": approve, "comments": comments }),
                    )?;
                }
                "done" => {
                    let result = if rest.is_empty() {
                        "done".to_string()
                    } else {
                        rest
                    };
                    self.tool("task_complete", json!({ "result": result }))?;
                }
                "fail" => {
                    self.tool("task_fail", json!({ "reason": rest }))?;
                }
                "exit" => {
                    let code: i32 = rest.parse().context("exit <code>")?;
                    println!("[{}] exiting with code {code}", self.me);
                    std::process::exit(code);
                }
                "crash-once" => {
                    // Simulate a crash the first time only: the marker file
                    // survives the restart, so the second attempt carries on.
                    let marker = std::path::Path::new(&rest);
                    if rest.is_empty() || marker.is_absolute() || rest.contains("..") {
                        bail!("crash-once <marker-file> (relative to the working directory)");
                    }
                    if !marker.exists() {
                        std::fs::write(marker, "crashed once\n")?;
                        println!("[{}] simulating a crash", self.me);
                        std::process::exit(3);
                    }
                }
                "tool" => {
                    let (words, json_args) = take_words(&rest, 1);
                    let name = words
                        .first()
                        .ok_or_else(|| anyhow!("tool <name> <json-args>"))?;
                    let args: Value = if json_args.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json_args).context("tool arguments must be JSON")?
                    };
                    self.tool(name, args)?;
                }
                other => bail!("unknown step '{other}'"),
            }
        }
        Ok(())
    }

    fn handle(&mut self, message: Message) -> Result<()> {
        println!(
            "[{}] {} from {}: {}",
            self.me,
            message.kind,
            message.from,
            crew_core::util::truncate_chars(&crew_core::util::one_line(&message.body), 100)
        );
        let steps = match message.kind {
            MessageType::Task if self.is_subagent && !self.args.on_subtask.is_empty() => {
                self.args.on_subtask.clone()
            }
            MessageType::Task => self.args.on_task.clone(),
            MessageType::ReviewRequest if !self.args.on_review.is_empty() => {
                self.args.on_review.clone()
            }
            _ => self.args.on_message.clone(),
        };
        if steps.is_empty() {
            return Ok(());
        }
        let trigger = Trigger::of(&message);
        self.hook("turn-start");
        let result = self.run_steps(&steps, &trigger);
        self.hook("turn-end");
        result
    }
}

pub fn run(args: ScriptArgs) -> Result<()> {
    let identity = Identity::from_env()
        .context("crew scripted-agent runs as a crew agent: CREW_IDENTITY is not set")?;
    let (mut client, _) = Client::agent(&identity, "scripted-agent")?;
    client.set_timeout(Some(Duration::from_secs(120)))?;
    let team: Vec<crew_core::Agent> = client.call(Call::AgentsList)?;
    let is_subagent = team
        .iter()
        .any(|a| a.id == identity.agent_id && a.parent.is_some());
    let mut agent = Agent {
        me: identity.agent_id.clone(),
        client,
        is_subagent,
        stdin: std::io::stdin().lock().lines(),
        backlog: VecDeque::new(),
        last_commit: None,
        args,
    };
    println!("[{}] scripted agent online", agent.me);
    agent.hook("ready");
    if !agent.args.on_start.is_empty() {
        let steps = agent.args.on_start.clone();
        agent.hook("turn-start");
        let result = agent.run_steps(&steps, &Trigger::default());
        agent.hook("turn-end");
        result?;
    }
    // Messages may already be waiting (sent before we were up).
    agent.fetch()?;
    loop {
        while let Some(message) = agent.backlog.pop_front() {
            if let Err(e) = agent.handle(message) {
                eprintln!("[{}] step failed: {e:#}", agent.me);
                std::process::exit(1);
            }
        }
        if !agent.wait_for_nudge()? {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_parsing() {
        assert_eq!(
            split_step("progress 50 half way"),
            ("progress".into(), "50 half way".into())
        );
        assert_eq!(split_step("  done  "), ("done".into(), String::new()));
        assert_eq!(
            take_words("claude-5 question please add a column", 2),
            (
                vec!["claude-5".into(), "question".into()],
                "please add a column".into()
            )
        );
        assert_eq!(take_words("only", 2), (vec!["only".into()], String::new()));
        assert_eq!(take_words("", 1), (Vec::<String>::new(), String::new()));
    }

    #[test]
    fn commit_ids_are_found_in_message_bodies() {
        assert_eq!(
            find_commit("API implementation complete.\nCommit: a91ec21\nCherry-pick it."),
            Some("a91ec21".into())
        );
        assert_eq!(
            find_commit("see 0123456789abcdef0123456789abcdef01234567 please"),
            Some("0123456789abcdef0123456789abcdef01234567".into())
        );
        // Plain words made of hex letters are not commits.
        assert_eq!(find_commit("a decade of cafe facade"), None);
        assert_eq!(find_commit("nothing here"), None);
    }
}
