//! A minimal MCP server over stdio that bridges an agent to the daemon.
//!
//! The agent's CLI (Claude Code, Codex) launches `crew mcp --identity <file>`
//! as a stdio MCP server. This process speaks JSON-RPC 2.0, one message per
//! line, and forwards each tool call to the daemon over the Unix socket,
//! authenticated with the agent's token. The protocol surface is small
//! (`initialize`, `tools/list`, `tools/call`, `ping`), so it is implemented
//! directly rather than pulling in an SDK.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use crew_core::client::Client;
use crew_core::paths::Identity;
use crew_core::rpc::Call;
use crew_core::Agent;
use serde_json::{json, Value};

pub mod tools;

const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

struct Bridge {
    identity: Identity,
    client: Option<Client>,
    orchestrator: Option<bool>,
}

impl Bridge {
    fn client(&mut self) -> Result<&mut Client, String> {
        if self.client.is_none() {
            let (client, _) = Client::agent(&self.identity, "mcp").map_err(|e| e.to_string())?;
            client
                .set_timeout(Some(Duration::from_secs(120)))
                .map_err(|e| e.to_string())?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("client was just set"))
    }

    /// Whether this agent is the orchestrator (decides which tools it sees).
    fn is_orchestrator(&mut self) -> bool {
        if let Some(known) = self.orchestrator {
            return known;
        }
        let me = self.identity.agent_id.clone();
        let answer = self
            .client()
            .ok()
            .and_then(|c| c.call::<Vec<Agent>>(Call::AgentsList).ok())
            .map(|agents| agents.iter().any(|a| a.id == me && a.orchestrator));
        match answer {
            Some(value) => {
                self.orchestrator = Some(value);
                value
            }
            None => {
                self.client = None;
                false
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<String, String> {
        // One reconnect covers a daemon restart between two calls.
        for attempt in 0..2 {
            let client = self.client()?;
            match tools::call(client, name, args) {
                Ok(output) => return Ok(output.text),
                Err(e) if attempt == 0 && looks_disconnected(&e) => {
                    self.client = None;
                }
                Err(e) => return Err(e),
            }
        }
        Err("the crew daemon is not reachable".into())
    }
}

fn looks_disconnected(error: &str) -> bool {
    error.contains("closed the connection")
        || error.contains("connection to the crew daemon failed")
        || error.contains("cannot reach the crew daemon")
}

fn tool_list(orchestrator: bool) -> Value {
    let tools: Vec<Value> = tools::definitions()
        .into_iter()
        .filter(|t| orchestrator || !t.orchestrator_only)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
                "annotations": { "readOnlyHint": t.read_only, "openWorldHint": false },
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn handle(bridge: &mut Bridge, message: &Value) -> Option<Value> {
    let method = message.get("method")?.as_str()?;
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(LATEST_PROTOCOL);
            let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
                requested
            } else {
                LATEST_PROTOCOL
            };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "crew", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Team coordination for crew agents: message teammates directly, \
            report progress, exchange commits and complete tasks.",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_list(bridge.is_orchestrator())),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Ok(match bridge.call_tool(name, &args) {
                Ok(text) => {
                    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                }
                // Tool failures are results the model can react to, not
                // protocol errors.
                Err(text) => {
                    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
                }
            })
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ if id.is_none() => return None, // notification we do not care about
        other => Err((-32601, format!("method not found: {other}"))),
    };
    // Notifications never get a response.
    let id = id?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    })
}

/// Serve MCP on stdin/stdout until the client closes the pipe.
pub fn serve(identity_path: Option<&Path>) -> Result<()> {
    let identity = match identity_path {
        Some(path) => Identity::load(path)?,
        None => Identity::from_env().context(
            "no agent identity: pass --identity <file> or set CREW_IDENTITY (crew sets this for its agents)",
        )?,
    };
    let mut bridge = Bridge {
        identity,
        client: None,
        orchestrator: None,
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut bridge, &message),
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {e}") },
            })),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response)?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> Bridge {
        Bridge {
            identity: Identity {
                socket: "/nonexistent/crew.sock".into(),
                agent_id: "claude-1".into(),
                token: "t".into(),
            },
            client: None,
            orchestrator: Some(false),
        }
    }

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        let mut b = bridge();
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "crew");
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], LATEST_PROTOCOL);
    }

    #[test]
    fn notifications_get_no_response_and_unknown_methods_get_an_error() {
        let mut b = bridge();
        assert!(handle(
            &mut b,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
        let r = handle(&mut b, &json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        let r = handle(&mut b, &json!({"jsonrpc":"2.0","id":4,"method":"ping"})).unwrap();
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn workers_do_not_see_orchestrator_tools() {
        let mut b = bridge();
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"agent_send"));
        assert!(!names.contains(&"task_create"));
        b.orchestrator = Some(true);
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":6,"method":"tools/list"}),
        )
        .unwrap();
        assert!(r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "task_create"));
    }

    #[test]
    fn an_unreachable_daemon_is_a_tool_error_not_a_crash() {
        let mut b = bridge();
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"agents_list","arguments":{}}}),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot reach the crew daemon"));
    }
}
