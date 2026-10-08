//! `crew agent …` and `crew task …`.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use crew_core::config::{AgentSpec, TaskSpec};
use crew_core::rpc::{Call, OutputResult, Snapshot, TranscriptResult};
use crew_core::types::TranscriptKind;
use crew_core::util::fmt_clock;
use crew_core::{Agent, Task};

use super::super::{attach as raw_attach, AgentCommand, TaskCommand};
use super::Target;
use crate::mcp::tools::{render_agents, render_tasks};

/// A session transcript as plain text: each prompt under a rule with its
/// time, replies as written, tool calls one to a line.
fn transcript_text(transcript: &TranscriptResult) -> String {
    let mut out = String::new();
    if transcript.truncated {
        out.push_str("[earlier entries not shown]\n");
    }
    let mut last = None;
    for entry in &transcript.entries {
        // Tool calls of one turn stay together.
        if last.is_some()
            && !(last == Some(TranscriptKind::Tool) && entry.kind == TranscriptKind::Tool)
        {
            out.push('\n');
        }
        match entry.kind {
            TranscriptKind::Prompt => {
                let at = entry
                    .at
                    .map(|at| format!(" · {}", &fmt_clock(at)[..8]))
                    .unwrap_or_default();
                out.push_str(&format!("── prompt{at}\n{}\n", entry.text));
            }
            TranscriptKind::Reply => out.push_str(&format!("{}\n", entry.text)),
            TranscriptKind::Tool => out.push_str(&format!("▸ {}\n", entry.text)),
            TranscriptKind::Note => out.push_str(&format!("── {}\n", entry.text)),
        }
        last = Some(entry.kind);
    }
    out
}

/// The call that restarts agent `id`: resuming its session, or (`fresh`) in a
/// new one. Fresh is a call of its own, so a daemon that predates it refuses
/// rather than resuming.
pub(in crate::cli) fn restart_call(id: &str, fresh: bool) -> Call {
    let id = id.to_string();
    if fresh {
        Call::AgentRestartFresh { id }
    } else {
        Call::AgentRestart { id }
    }
}

pub fn agent(target: &Target, cmd: AgentCommand) -> Result<()> {
    let (mut client, info) = target.connect("cli")?;
    match cmd {
        AgentCommand::List { json } => {
            let agents: Vec<Agent> = client.call(Call::AgentsList)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&agents)?);
            } else {
                print!("{}", render_agents(&agents));
            }
        }
        AgentCommand::Add {
            id,
            role,
            runtime,
            model,
            task,
        } => {
            let runtime = match runtime {
                Some(r) => Some(r.parse().map_err(|e: String| anyhow!(e))?),
                None => None,
            };
            let agent: Agent = client.call(Call::AgentAdd {
                spec: Box::new(AgentSpec {
                    id,
                    role,
                    runtime,
                    model,
                    task,
                    ..AgentSpec::default()
                }),
            })?;
            println!("added {} ({})", agent.id, agent.runtime);
        }
        AgentCommand::Kill { id } => {
            client.call_value(Call::AgentKill { id: id.clone() })?;
            println!("stopped {id}");
        }
        AgentCommand::Remove { id } => {
            client.call_value(Call::AgentRemove { id: id.clone() })?;
            println!("{id} left the team");
        }
        AgentCommand::Restart { id, fresh } => {
            client.call_value(restart_call(&id, fresh))?;
            println!(
                "restarting {id}{}",
                if fresh { " in a new session" } else { "" }
            );
        }
        AgentCommand::Model {
            id,
            model,
            runtime,
            note,
        } => {
            let runtime = match runtime {
                Some(r) => Some(r.parse().map_err(|e: String| anyhow!(e))?),
                None => None,
            };
            if model.is_none() && runtime.is_none() {
                bail!("name a model, or a --runtime for its default model");
            }
            let before: Vec<Agent> = client.call(Call::AgentsList)?;
            let before = before.iter().find(|a| a.id == id).map(|a| a.runtime);
            let agent: Agent = client.call(Call::AgentSetModel {
                id: id.clone(),
                model: model.unwrap_or_default(),
                runtime,
                note,
            })?;
            let on = agent
                .model_name
                .clone()
                .or(agent.model.clone())
                .unwrap_or_else(|| "its default model".to_string());
            if before.is_some_and(|r| r != agent.runtime) {
                println!(
                    "moving {id} to {} on {on}: a fresh session there, handed the work",
                    crate::models::runtime_label(agent.runtime)
                );
                // The toolbox is mapped again there: say what will not load.
                // The move is made either way, so this is shown only when known.
                let mapped: Result<Vec<crew_core::rpc::MemberToolbox>, _> =
                    client.call(Call::Toolbox {
                        agent: Some(id.clone()),
                        runtime: Some(agent.runtime),
                    });
                if let Some(member) = mapped.ok().as_ref().and_then(|m| m.first()) {
                    for line in super::toolbox::not_loading(&member.toolbox) {
                        println!("  {line}");
                    }
                }
            } else {
                println!("restarting {id} on {on}");
            }
        }
        AgentCommand::Compact { id, keep } => {
            client.call_value(Call::AgentCompact {
                id: Some(id.clone()),
                keep,
            })?;
            println!("queued a compaction of {id}'s context; it runs when its current turn ends");
        }
        AgentCommand::Pause { id } => {
            client.call_value(Call::AgentPause { id: id.clone() })?;
            println!("paused {id}");
        }
        AgentCommand::Resume { id } => {
            client.call_value(Call::AgentResume { id: id.clone() })?;
            println!("resumed {id}");
        }
        AgentCommand::Output {
            id,
            lines,
            screen,
            ansi,
        } => {
            let output: OutputResult = client.call(Call::AgentOutput {
                id,
                lines: Some(lines),
                screen: screen || ansi,
                ansi,
            })?;
            println!("{}", output.text);
        }
        AgentCommand::Transcript { id, entries } => {
            let transcript: TranscriptResult = client.call(Call::AgentTranscript {
                id: id.clone(),
                limit: Some(entries),
            })?;
            if !transcript.available {
                bail!("{id} has no session transcript (yet)");
            }
            print!("{}", transcript_text(&transcript));
        }
        AgentCommand::Prompt { id, text } => {
            client.call_value(Call::AgentInput {
                id: id.clone(),
                text: Some(text),
                submit: true,
            })?;
            println!("prompt submitted to {id}");
        }
        AgentCommand::Attach { id } => {
            let snapshot: Snapshot = client.call(Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            })?;
            if snapshot.session.backend == "herdr" {
                // herdr owns the terminal: bring its pane forward instead.
                client.call_value(Call::AgentFocus { id: id.clone() })?;
                println!("{id} runs in a herdr pane; focused it");
                return Ok(());
            }
            drop(client);
            raw_attach::run(Path::new(&info.socket), &id)?;
        }
    }
    Ok(())
}

pub fn task(target: &Target, cmd: TaskCommand) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    match cmd {
        TaskCommand::List { json } => {
            let tasks: Vec<Task> = client.call(Call::TasksList)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&tasks)?);
            } else {
                print!("{}", render_tasks(&tasks));
            }
        }
        TaskCommand::Add {
            id,
            title,
            description,
            agent,
            depends_on,
            reviewer,
        } => {
            let task: Task = client.call(Call::TaskCreate {
                spec: TaskSpec {
                    id,
                    title,
                    description,
                    agent,
                    depends_on,
                    reviewer,
                    timeout_secs: None,
                },
            })?;
            println!("created task {} ({})", task.id, task.status);
        }
        TaskCommand::Show { id, json } => {
            let card: crew_core::rpc::TaskCard = client.call(Call::TaskGet { id: Some(id) })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&card)?);
            } else {
                print!("{}", crate::mcp::tools::render_card(&card));
            }
        }
        TaskCommand::Edit {
            id,
            title,
            description,
        } => {
            let task: Task = client.call(Call::TaskEdit {
                id,
                title,
                description,
            })?;
            println!("task {} revised", task.id);
        }
        TaskCommand::Assign { id, agent } => {
            let task: Task = client.call(Call::TaskAssign { id, agent })?;
            println!(
                "task {} assigned to {}",
                task.id,
                task.assigned_agent.as_deref().unwrap_or("?")
            );
        }
        TaskCommand::Retry { id } => {
            let task: Task = client.call(Call::TaskRetry { id })?;
            println!("task {} queued again", task.id);
        }
        TaskCommand::Done { id, result } => {
            let task: Task = client.call(Call::TaskComplete {
                task_id: Some(id),
                result,
                artifacts: Vec::new(),
            })?;
            println!("task {} is {}", task.id, task.status);
        }
    }
    Ok(())
}
