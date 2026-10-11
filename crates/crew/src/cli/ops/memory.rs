//! `crew memory`: the project memory from the operator's side (see
//! `crate::memory`). The files under the memory directory can be edited
//! by hand just as well; these go through the daemon so that what happens
//! is an event and the limits hold.

use std::io::Read;

use anyhow::{anyhow, Result};
use clap::Subcommand;
use crew_core::rpc::Call;
use crew_core::util::fmt_datetime;
use crew_core::{Memory, MemoryKind};
use serde_json::Value;

use super::Target;

#[derive(Subcommand, Clone, Debug)]
pub enum MemoryCommand {
    /// Every memory, newest first
    List {
        #[arg(long)]
        json: bool,
    },
    /// One memory in full
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// The memories matching words, best first
    Search {
        #[arg(required = true)]
        words: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Remember something for later sessions (the body from `--body`, or
    /// standard input)
    #[command(long_about = "Remember something for later sessions.\n\n\
The name is [a-z0-9-] (at most 64 characters), the description one line for the gist, the \
body what a later session needs to know and why: `crew memory add docs-host-quill \"Docs that \
leave the repo go to Quill\" --kind decision --body \"Chosen over Notion: …\"`. Without --body, \
the body is read from standard input. A memory of the same name is replaced.")]
    Add {
        name: String,
        /// One line, at most 200 characters: the gist, read in every briefing
        description: String,
        /// decision | convention | preference | fact | pitfall
        #[arg(long, default_value = "fact")]
        kind: String,
        /// The note; read from standard input when left out
        #[arg(long)]
        body: Option<String>,
        /// Where it came from: a task, message or commit id
        #[arg(long)]
        source: Option<String>,
    },
    /// Drop a memory
    Forget { name: String },
}

pub fn memory(target: &Target, command: MemoryCommand) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    match command {
        MemoryCommand::List { json } => {
            let memories: Vec<Memory> = client.call(Call::MemoryList)?;
            print_list(&memories, json, "nothing is remembered yet")?;
        }
        MemoryCommand::Show { name, json } => {
            let memory: Memory = client.call(Call::MemoryGet { name })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&memory)?);
            } else {
                print_memory(&memory);
            }
        }
        MemoryCommand::Search { words, limit, json } => {
            let memories: Vec<Memory> = client.call(Call::MemorySearch {
                query: words.join(" "),
                limit: Some(limit),
            })?;
            print_list(&memories, json, "nothing matches")?;
        }
        MemoryCommand::Add {
            name,
            description,
            kind,
            body,
            source,
        } => {
            let kind: MemoryKind = kind.parse().map_err(|e: String| anyhow!(e))?;
            let body = match body {
                Some(body) => body,
                None => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
            };
            let result: Value = client.call(Call::MemoryAdd {
                name: name.clone(),
                description,
                kind: Some(kind),
                body,
                source,
            })?;
            let replaced = result["replaced"].as_bool().unwrap_or(false);
            println!(
                "remembered {name}{}",
                if replaced {
                    " (replacing the earlier one)"
                } else {
                    ""
                }
            );
        }
        MemoryCommand::Forget { name } => {
            let _: Value = client.call(Call::MemoryForget { name: name.clone() })?;
            println!("forgot {name}");
        }
    }
    Ok(())
}

fn print_list(memories: &[Memory], json: bool, none: &str) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(memories)?);
        return Ok(());
    }
    if memories.is_empty() {
        println!("{none}");
        return Ok(());
    }
    let width = memories.iter().map(|m| m.name.len()).max().unwrap_or(0);
    for m in memories {
        println!(
            "{:width$}  {}  [{}, {}, {}]",
            m.name,
            m.description,
            m.kind,
            m.by,
            crate::memory::date_of(m.at),
            width = width
        );
    }
    Ok(())
}

fn print_memory(m: &Memory) {
    println!("{} — {}", m.name, m.description);
    print!("{}, by {}, {}", m.kind, m.by, fmt_datetime(m.at));
    if let Some(source) = &m.source {
        print!(", from {source}");
    }
    println!("\n\n{}", m.body);
}
