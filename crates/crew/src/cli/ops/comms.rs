//! Reading the event log and the message bus, and writing to them: `crew logs`,
//! `messages`, `send`, `questions`, `answer`, `integrate`.

use std::path::Path;

use anyhow::{anyhow, bail, Result};
use crew_core::client::Client;
use crew_core::rpc::{Call, Push, SendParams, SendResult, Snapshot};
use crew_core::util::{fmt_clock, fmt_elapsed, one_line, slash_command, truncate_chars};
use crew_core::{Event, Message};
use serde_json::Value;

use super::Target;

fn print_event(event: &Event, json: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
        return;
    }
    println!(
        "{} {:<5} {:<10} {:<22} {:<14} {}",
        fmt_clock(event.timestamp),
        event.level.as_str(),
        event.source,
        event.event_type,
        event.agent_id.as_deref().unwrap_or("-"),
        event.message
    );
}

pub fn logs(
    target: &Target,
    agent: Option<String>,
    lines: usize,
    follow: bool,
    json: bool,
) -> Result<()> {
    let (mut client, info) = target.connect("cli")?;
    let events: Vec<Event> = client.call(Call::Events {
        agent: agent.clone(),
        limit: Some(lines),
    })?;
    let mut last_seq = 0;
    for event in &events {
        print_event(event, json);
        last_seq = event.seq;
    }
    if !follow {
        return Ok(());
    }
    let (subscriber, _) = Client::operator(Path::new(&info.socket), "cli-logs")?;
    let mut stream = subscriber.subscribe()?;
    loop {
        match stream.next() {
            Ok(Push::Event(event)) => {
                if event.seq <= last_seq {
                    continue;
                }
                if agent.is_some() && event.agent_id != agent {
                    continue;
                }
                print_event(&event, json);
            }
            Ok(Push::Shutdown) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

fn print_message(m: &Message) {
    let arrow = if m.is_broadcast() {
        "⇶"
    } else if m.is_p2p() {
        "→"
    } else {
        "⇉"
    };
    println!(
        "{} {:>13} {arrow} {:<13} [{}] {}",
        fmt_clock(m.timestamp),
        m.from,
        m.to,
        m.kind,
        truncate_chars(&one_line(&m.body), 140)
    );
}

pub fn messages(target: &Target, agent: Option<String>, lines: usize, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let messages: Vec<Message> = client.call(Call::History {
        agent,
        limit: Some(lines),
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else {
        for m in &messages {
            print_message(m);
        }
    }
    Ok(())
}

pub fn send(target: &Target, to: &str, body: &str, kind: &str) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let sent: SendResult = client.call(Call::Send(SendParams {
        to: to.to_string(),
        kind: kind.parse().map_err(|e: String| anyhow!(e))?,
        body: body.to_string(),
        ..SendParams::default()
    }))?;
    println!("sent {} to {}", sent.message.id, sent.recipients.join(", "));
    Ok(())
}

fn open_questions(client: &mut Client) -> Result<(Vec<Message>, i64)> {
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(0),
        events: Some(0),
    })?;
    Ok((snapshot.questions, snapshot.server_time))
}

/// `crew questions`: what the team wants the operator to decide.
pub fn questions(target: &Target, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let (open, now) = open_questions(&mut client)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&open)?);
        return Ok(());
    }
    if open.is_empty() {
        println!("no question is waiting for you");
        return Ok(());
    }
    for question in &open {
        println!(
            "{}  from {} · {} ago",
            question.id,
            question.asker(),
            fmt_elapsed((now - question.timestamp).max(0))
        );
        for line in question.body.trim_end().lines() {
            println!("  {line}");
        }
        for (i, option) in question.options().iter().enumerate() {
            println!("    {}. {option}", i + 1);
        }
        println!();
    }
    println!("answer with: crew answer <id> \"your answer\"   (a number picks a suggested answer)");
    Ok(())
}

/// `crew answer`: answer an open question, in words or by picking one of
/// its suggested answers.
pub fn answer(target: &Target, id: Option<&str>, answer: &str) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let (open, _) = open_questions(&mut client)?;
    let question = match id {
        Some(id) => {
            // The id as shown, or any unambiguous part of it.
            let mut matches = open.iter().filter(|q| q.id == id || q.id.contains(id));
            match (matches.next(), matches.next()) {
                (Some(question), None) => question,
                (Some(_), Some(_)) => bail!("'{id}' matches more than one open question; see `crew questions`"),
                _ => bail!("no open question '{id}'; see `crew questions`"),
            }
        }
        None => match open.as_slice() {
            [] => bail!("no question is waiting for you"),
            [only] => only,
            _ => bail!(
                "{} questions are open; name the one you answer: crew answer <id> \"...\" (ids: {})",
                open.len(),
                open.iter().map(|q| q.id.as_str()).collect::<Vec<_>>().join(", ")
            ),
        },
    };
    let options = question.options();
    let body = match answer.trim().parse::<usize>() {
        Ok(number) if (1..=options.len()).contains(&number) => options[number - 1].clone(),
        _ => answer.trim().to_string(),
    };
    refuse_command_answer(&body)?;
    let sent: SendResult = client.call(Call::Answer {
        id: question.id.clone(),
        body: body.clone(),
    })?;
    println!(
        "answered {}: {}",
        sent.recipients.join(", "),
        truncate_chars(&one_line(&body), 120)
    );
    Ok(())
}

/// An answer that is really a slash command was typed in the wrong place.
fn refuse_command_answer(answer: &str) -> Result<()> {
    match slash_command(answer) {
        Some((name, _)) => bail!("'/{name}' looks like a command, not an answer; nothing was sent"),
        None => Ok(()),
    }
}

pub fn integrate(target: &Target) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let result: Value = client.call(Call::Integrate)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
