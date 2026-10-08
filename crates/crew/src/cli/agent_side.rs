//! Commands that run on behalf of an agent: `crew ctl`, hooks, and the
//! delivery bridge. Identity always comes from the agent's identity file.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::client::{Client, ClientError};
use crew_core::paths::Identity;
use crew_core::rpc::{Call, Push};
use serde_json::{json, Value};

use super::CtlCommand;
use crate::mcp::tools;

fn identity(path: Option<&Path>) -> Result<Identity> {
    match path {
        Some(path) => Identity::load(path),
        None => Identity::from_env()?.context(
            "this command runs inside a crew agent: CREW_IDENTITY is not set (or pass --identity)",
        ),
    }
}

pub fn ctl(cmd: CtlCommand) -> Result<()> {
    let identity = identity(None)?;
    // What a runtime without an MCP client registers as its own tools: by
    // the role its launch recorded, without asking the daemon, so the list
    // is the same whether or not the daemon answers.
    if let CtlCommand::Tools = cmd {
        let listed: Vec<Value> = tools::definitions()
            .into_iter()
            .filter(|t| identity.orchestrator || !t.orchestrator_only)
            .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema }))
            .collect();
        println!("{}", serde_json::to_string(&listed)?);
        return Ok(());
    }
    let (mut client, _) = Client::agent(&identity, "ctl")?;
    client.set_timeout(Some(Duration::from_secs(120)))?;
    let (name, args): (&str, Value) = match cmd {
        CtlCommand::List => ("agents_list", json!({})),
        CtlCommand::Send { to, body, kind } => (
            "agent_send",
            json!({ "to": to, "body": body, "type": kind }),
        ),
        CtlCommand::Broadcast { body } => ("agent_broadcast", json!({ "body": body })),
        CtlCommand::Inbox { all } => ("agent_inbox", json!({ "all": all })),
        CtlCommand::Status { status, note } => {
            ("agent_status", json!({ "status": status, "note": note }))
        }
        CtlCommand::Progress { percent, message } => (
            "task_update",
            json!({ "percent": percent, "message": message }),
        ),
        CtlCommand::Tasks => ("tasks_list", json!({})),
        CtlCommand::Task { id } => ("task_get", json!({ "id": id })),
        CtlCommand::Done { result } => ("task_complete", json!({ "result": result })),
        CtlCommand::Fail { reason } => ("task_fail", json!({ "reason": reason })),
        CtlCommand::Commit { message } => ("git_commit", json!({ "message": message })),
        CtlCommand::Diff { against } => ("git_diff", json!({ "against": against })),
        CtlCommand::GitStatus => ("git_status", json!({})),
        CtlCommand::CherryPick { commit } => ("git_cherry_pick", json!({ "commit": commit })),
        CtlCommand::Merge { agent } => ("git_merge", json!({ "from_agent": agent })),
        CtlCommand::Spawn { task, role, id } => (
            "agent_spawn",
            json!({ "task": task, "role": role, "id": id }),
        ),
        CtlCommand::ReviewRequest { agent, artifact } => (
            "review_request",
            json!({ "agent_id": agent, "artifact": artifact }),
        ),
        CtlCommand::Review {
            task,
            approve,
            reject,
            comments,
        } => {
            if approve == reject {
                bail!("pass exactly one of --approve or --reject");
            }
            (
                "review_submit",
                json!({ "task_id": task, "approve": approve, "comments": comments }),
            )
        }
        CtlCommand::Tools => unreachable!("answered before connecting"),
        CtlCommand::Tool { name, args } => {
            let args: Value = serde_json::from_str(&args).context("tool arguments must be JSON")?;
            let output = tools::call(&mut client, &name, &args).map_err(|e| anyhow!(e))?;
            println!("{}", output.text.trim_end());
            return Ok(());
        }
    };
    let output = tools::call(&mut client, name, &args).map_err(|e| anyhow!(e))?;
    println!("{}", output.text.trim_end());
    Ok(())
}

/// Forward a runtime hook event to the daemon. Never prints and fails only
/// for a daemon of another build: hook output can end up in the agent's
/// context, and a failing hook can block the session. That one failure is
/// said on stderr with exit code 1, which no runtime takes as blocking
/// (Claude Code blocks on 2), so the operator sees why the team's tools
/// fail (notes/binary-pinning.md B8).
pub fn hook(event: &str, identity_path: Option<&Path>, payload: Option<String>) -> Result<()> {
    let Ok(identity) = identity(identity_path) else {
        return Ok(());
    };
    let payload = match payload {
        Some(text) => text,
        None => {
            let mut text = String::new();
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                let _ = stdin.lock().take(4 * 1024 * 1024).read_to_string(&mut text);
            }
            text
        }
    };
    let payload: Value = serde_json::from_str(payload.trim()).unwrap_or(Value::Null);
    let mut client = match Client::agent(&identity, "hook") {
        Ok((client, _)) => client,
        Err(error) if error.is_protocol_mismatch() => return Err(error.into()),
        Err(_) => return Ok(()),
    };
    let _ = client.set_timeout(Some(Duration::from_secs(5)));
    let _ = client.call_value(Call::Hook {
        event: event.to_string(),
        payload,
        // The launch whose process ran the hook, from its environment: a
        // late hook of a replaced launch is then told apart.
        launch: std::env::var("CREW_LAUNCH").ok().filter(|l| !l.is_empty()),
    });
    Ok(())
}

/// Hold the agent's delivery channel open. Each delivery is printed as one
/// JSON line (`{"ids":[..],"text":".."}`) for the runtime-side mod to inject.
pub fn bridge(identity_path: Option<&Path>) -> Result<()> {
    let identity = identity(identity_path)?;
    let (mut client, _) = Client::agent(&identity, "bridge")?;
    client.call_value(Call::Listen)?;
    let stdout = std::io::stdout();
    loop {
        let frame = match client.next_frame() {
            Ok(frame) => frame,
            Err(ClientError::Closed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match frame.push {
            Some(Push::Deliver { ids, text }) => {
                let mut out = stdout.lock();
                // A closed pipe means the session is gone; so are we.
                if writeln!(out, "{}", json!({ "ids": ids, "text": text })).is_err()
                    || out.flush().is_err()
                {
                    return Ok(());
                }
            }
            // A compaction to run at the next turn boundary.
            Some(Push::Compact { instructions }) => {
                let mut out = stdout.lock();
                let order = json!({ "compact": { "instructions": instructions } });
                if writeln!(out, "{order}").is_err() || out.flush().is_err() {
                    return Ok(());
                }
            }
            Some(Push::Shutdown) => return Ok(()),
            _ => {}
        }
    }
}

/// Claude Code's `apiKeyHelper`: print this agent's key and nothing else.
pub fn credential(identity_path: Option<&Path>) -> Result<()> {
    let identity = identity(identity_path)?;
    let (mut client, _) = Client::agent(&identity, "credential")?;
    client.set_timeout(Some(Duration::from_secs(10)))?;
    let reply = client.call_value(Call::Credential)?;
    let key = reply
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("the daemon returned no key"))?;
    println!("{key}");
    Ok(())
}

pub fn delivered(identity_path: Option<&Path>, ids: Vec<String>) -> Result<()> {
    let identity = identity(identity_path)?;
    let (mut client, _) = Client::agent(&identity, "delivered")?;
    client.set_timeout(Some(Duration::from_secs(10)))?;
    client.call_value(Call::Delivered { ids })?;
    Ok(())
}

/// `crew toolbox mcp <name>`: one toolbox MCP server for the agent's
/// session, through crew's proxy (`crate::mcp::proxy`); with `--preflight`,
/// its listing for the daemon to pin. Nothing it prints or reports carries a
/// resolved secret.
pub fn toolbox_mcp(args: super::ToolboxMcpArgs) -> Result<()> {
    use crate::mcp::proxy;
    use crew_core::rpc::{ToolboxReportKind, ToolboxServerLaunch};

    let mut client: Option<Client> = None;
    let launch: ToolboxServerLaunch = match &args.launch_file {
        Some(path) => serde_json::from_slice(
            &std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        )
        .with_context(|| format!("parsing {}", path.display()))?,
        None => {
            let identity = identity(args.identity.identity.as_deref())?;
            let (mut c, _) = Client::agent(&identity, "toolbox")?;
            c.set_timeout(Some(Duration::from_secs(30)))?;
            let launch = c.call(Call::ToolboxServer {
                name: args.name.clone(),
                preflight: args.preflight,
            });
            client = Some(c);
            match launch {
                Ok(launch) => launch,
                Err(e) if args.preflight => {
                    println!("{}", json!({ "ok": false, "reason": e.to_string() }));
                    std::process::exit(1);
                }
                Err(e) => return Err(e.into()),
            }
        }
    };
    if args.preflight {
        match proxy::preflight(&launch, proxy::PREFLIGHT_WITHIN) {
            Ok(listing) => println!("{}", json!({ "ok": true, "listing": listing.to_json() })),
            Err(e) => {
                let reason = proxy::redacted(&launch, &format!("{e:#}"));
                println!("{}", json!({ "ok": false, "reason": reason }));
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let snapshot = proxy::Snapshot::load(std::path::Path::new(&launch.snapshot))?;
    let name = launch.name.clone();
    let mut report = |kind: ToolboxReportKind, detail: String| match client.as_mut() {
        Some(c) => {
            let _ = c.call_value(Call::ToolboxReport {
                name: name.clone(),
                kind,
                detail: proxy::redacted(&launch, &detail),
            });
        }
        None => eprintln!(
            "crew toolbox mcp {name}: {kind:?}: {}",
            proxy::redacted(&launch, &detail)
        ),
    };
    proxy::serve(
        &launch,
        snapshot,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
        &mut report,
    )
}
