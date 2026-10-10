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
use serde_json::{json, Value};

pub mod proxy;
pub mod tools;

const LATEST_PROTOCOL: &str = "2025-06-18";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The calls a turn usually ends with. To a client that can end a turn
/// with a tool result (Claude Code: `_meta["claude/endTurn"]`) each takes
/// `end_turn`: the turn ends with the call, instead of with one more model
/// request, the whole conversation sent again, to write a closing line
/// nobody reads.
const END_TURN_TOOLS: &[&str] = &[
    "agent_send",
    "agent_broadcast",
    "agent_status",
    "agent_clear",
    "task_complete",
    "task_fail",
    "review_submit",
];
/// Called, it ends the turn whatever the arguments: what comes of it is
/// decided when the turn is over (decline, in its description).
const ALWAYS_END_TURN: &[&str] = &["decline"];
const END_TURN_META: &str = "claude/endTurn";
/// The client that reads `END_TURN_META`, by the name it gives at `initialize`.
const END_TURN_CLIENT: &str = "claude-code";

struct Bridge {
    identity: Identity,
    client: Option<Client>,
    /// The client ends a turn on a tool result that asks it to.
    ends_turns: bool,
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

    /// Whether this agent is the orchestrator (decides which tools it sees),
    /// as its launch recorded it: the list must not depend on whether the
    /// daemon answers at the moment it is asked for.
    fn is_orchestrator(&self) -> bool {
        self.identity.orchestrator
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<String, String> {
        // One reconnect covers a daemon restart between two calls. A call
        // whose connection failed may have run before the reply was lost,
        // so only one that changes nothing is made again; for any other the
        // agent is told, to check before it calls again.
        let read_only = tools::definitions()
            .iter()
            .any(|t| t.name == name && t.read_only);
        for attempt in 0..2 {
            let client = self.client()?;
            match tools::call(client, name, args) {
                Ok(output) => return Ok(output.text),
                Err(tools::CallError::Daemon(e)) if e.is_disconnect() => {
                    self.client = None;
                    if !read_only {
                        return Err(format!(
                            "{e}: the call was sent, so it may or may not have taken effect; \
check before calling {name} again"
                        ));
                    }
                    if attempt > 0 {
                        return Err(e.to_string());
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("the crew daemon is not reachable".into())
    }
}

/// The tools offered: the orchestrator's or a member's, cut down to
/// `allowed` when the team's style takes some away (the identity file
/// carries the list, so it is the same whether or not the daemon answers).
fn tool_list(orchestrator: bool, ends_turns: bool, allowed: Option<&[String]>) -> Value {
    let tools: Vec<Value> = tools::definitions()
        .into_iter()
        .filter(|t| orchestrator || !t.orchestrator_only)
        .filter(|t| allowed.is_none_or(|allowed| allowed.iter().any(|a| a == t.name)))
        .map(|t| {
            let mut schema = t.schema;
            if ends_turns && END_TURN_TOOLS.contains(&t.name) {
                schema["properties"]["end_turn"] = json!({
                    "type": "boolean",
                    "description": "true when nothing follows this call this turn: the turn ends with \
it, and no closing text is written (every further response sends your whole conversation again)",
                });
            }
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": schema,
                "annotations": { "readOnlyHint": t.read_only, "openWorldHint": false },
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Whether a call that succeeded ends the turn.
fn ends_turn(name: &str, args: &Value) -> bool {
    ALWAYS_END_TURN.contains(&name)
        || (END_TURN_TOOLS.contains(&name)
            && args.get("end_turn").and_then(Value::as_bool) == Some(true))
}

fn handle(bridge: &mut Bridge, message: &Value) -> Option<Value> {
    let method = message.get("method")?.as_str()?;
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            bridge.ends_turns =
                params.pointer("/clientInfo/name").and_then(Value::as_str) == Some(END_TURN_CLIENT);
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
        "tools/list" => Ok(tool_list(
            bridge.is_orchestrator(),
            bridge.ends_turns,
            bridge.identity.tools.as_deref(),
        )),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // A tool the style took away is refused here as well as by the
            // daemon: the model is told plainly, not by a failed call.
            let withheld = bridge
                .identity
                .tools
                .as_ref()
                .is_some_and(|allowed| !allowed.iter().any(|a| a == name));
            let called = if withheld {
                Err(format!(
                    "{name} is not among the crew tools this team's style gives you"
                ))
            } else {
                bridge.call_tool(name, &args)
            };
            Ok(match called {
                Ok(text) if bridge.ends_turns && ends_turn(name, &args) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                    "_meta": { END_TURN_META: true },
                }),
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
        None => Identity::from_env()?.context(
            "no agent identity: pass --identity <file> or set CREW_IDENTITY (crew sets this for its agents)",
        )?,
    };
    let mut bridge = Bridge {
        identity,
        client: None,
        ends_turns: false,
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
                orchestrator: false,
                tools: None,
            },
            client: None,
            ends_turns: false,
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
        b.identity.orchestrator = true;
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
    fn a_member_whose_style_cut_its_crew_tools_sees_and_calls_those_only() {
        let mut b = bridge();
        b.identity.tools = Some(
            ["agents_list", "task_get", "task_complete", "agent_send"]
                .into_iter()
                .map(String::from)
                .collect(),
        );
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":8,"method":"tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["agents_list", "agent_send", "task_get", "task_complete"]
        );
        // Called anyway, a withheld tool is answered, not forwarded.
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"git_merge","arguments":{"from_agent":"x"}}}),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("git_merge is not among the crew tools this team's style gives you"),
            "{text}"
        );
    }

    // T21 (notes/toolboxes.md): the role comes from the launch identity, so
    // an orchestrator whose daemon does not answer at `tools/list` time
    // still gets its own list, byte for byte the same each time.
    #[test]
    fn the_tool_list_does_not_depend_on_the_daemon_answering() {
        let mut b = bridge();
        b.identity.orchestrator = true;
        let list = |b: &mut Bridge| {
            let r = handle(b, &json!({"jsonrpc":"2.0","id":7,"method":"tools/list"})).unwrap();
            serde_json::to_vec(&r["result"]).unwrap()
        };
        let first = list(&mut b);
        assert!(b.client.is_none(), "listing tools asks the daemon nothing");
        assert_eq!(list(&mut b), first);
        let tools: Value = serde_json::from_slice(&first).unwrap();
        assert!(tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "task_create"));
    }

    /// A daemon that answers the hello, takes one request and drops the
    /// connection without a reply (it may or may not have run it), on every
    /// connection; with how many requests it took.
    fn daemon_that_drops_requests() -> (
        tempfile::TempDir,
        Bridge,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::io::{BufRead, BufReader, Write};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crew.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let taken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = taken.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let hello: Value = serde_json::from_str(&line).unwrap();
                let info = crew_core::rpc::HelloInfo {
                    role: crew_core::rpc::Role::Agent,
                    agent_id: Some("claude-1".into()),
                    project: None,
                    version: "test".into(),
                    protocol: crew_core::rpc::PROTOCOL_VERSION,
                    pid: 1,
                    web_proof: None,
                };
                let reply = json!({ "id": hello["id"], "result": info });
                writeln!(stream, "{reply}").unwrap();
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) > 0 {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
        let bridge = Bridge {
            identity: Identity {
                socket: socket.to_string_lossy().into_owned(),
                agent_id: "claude-1".into(),
                token: "t".into(),
                orchestrator: false,
                tools: None,
            },
            client: None,
            ends_turns: false,
        };
        (dir, bridge, taken)
    }

    /// A daemon that answers the hello and then every request: a status
    /// call with the agent, anything else with an error.
    fn daemon_that_answers() -> (tempfile::TempDir, Bridge) {
        use std::io::{BufRead, BufReader, Write};
        let (dir, bridge, _) = daemon_that_drops_requests();
        let socket = dir.path().join("answers.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let mut hello = true;
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    line.clear();
                    let reply = if std::mem::take(&mut hello) {
                        let info = crew_core::rpc::HelloInfo {
                            role: crew_core::rpc::Role::Agent,
                            agent_id: Some("claude-1".into()),
                            project: None,
                            version: "test".into(),
                            protocol: crew_core::rpc::PROTOCOL_VERSION,
                            pid: 1,
                            web_proof: None,
                        };
                        json!({ "id": request["id"], "result": info })
                    } else if request["method"] == "status_update" {
                        let agent = json!({
                            "id": "claude-1", "name": "claude-1", "role": "work",
                            "status": request["params"]["status"], "runtime": "claude",
                            "workdir": "/w", "backend": "pty", "last_activity_at": 0
                        });
                        json!({ "id": request["id"], "result": agent })
                    } else {
                        json!({ "id": request["id"], "error": { "code": "invalid", "message": "no" } })
                    };
                    writeln!(stream, "{reply}").unwrap();
                }
            }
        });
        let bridge = Bridge {
            identity: Identity {
                socket: socket.to_string_lossy().into_owned(),
                ..bridge.identity
            },
            ..bridge
        };
        (dir, bridge)
    }

    #[test]
    fn claude_code_ends_its_turn_with_the_call_that_closes_it() {
        let send = |b: &mut Bridge, method: &str, params: Value| {
            handle(
                b,
                &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
            )
            .unwrap()["result"]
                .clone()
        };
        let schema = |list: &Value, name: &str| {
            list["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap()["inputSchema"]
                .clone()
        };
        for (client, ends) in [("claude-code", true), ("codex-mcp-client", false)] {
            let (_dir, mut b) = daemon_that_answers();
            send(
                &mut b,
                "initialize",
                json!({ "protocolVersion": LATEST_PROTOCOL, "clientInfo": { "name": client, "version": "1" } }),
            );
            let list = send(&mut b, "tools/list", json!({}));
            for name in [
                "agent_send",
                "agent_status",
                "task_complete",
                "review_submit",
            ] {
                assert_eq!(
                    schema(&list, name)["properties"]["end_turn"].is_object(),
                    ends,
                    "{client}: {name}"
                );
            }
            // A call that closes nothing, or a read, never offers it.
            for name in ["tasks_list", "task_get", "git_commit", "decline"] {
                assert!(
                    schema(&list, name)["properties"]["end_turn"].is_null(),
                    "{name}"
                );
            }
            let call = |b: &mut Bridge, name: &str, args: Value| {
                send(b, "tools/call", json!({ "name": name, "arguments": args }))
            };
            let ended =
                |result: &Value| result["_meta"]["claude/endTurn"].as_bool().unwrap_or(false);
            let parked = call(
                &mut b,
                "agent_status",
                json!({ "status": "waiting", "end_turn": true }),
            );
            assert_eq!(parked["isError"], false, "{parked}");
            assert_eq!(ended(&parked), ends, "{client}: {parked}");
            // Asked for, or not: the turn goes on without it.
            let working = call(&mut b, "agent_status", json!({ "status": "working" }));
            assert!(!ended(&working), "{working}");
            // A call that failed never ends the turn: the model sees the error.
            let failed = call(
                &mut b,
                "task_complete",
                json!({ "result": "done", "end_turn": true }),
            );
            assert_eq!(failed["isError"], true, "{failed}");
            assert!(!ended(&failed), "{failed}");
        }
        assert!(ends_turn("decline", &json!({ "reason": "no" })));
        assert!(!ends_turn("git_commit", &json!({ "end_turn": true })));
    }

    #[test]
    fn a_call_that_changes_something_is_not_sent_again_after_the_connection_drops() {
        let call = |b: &mut Bridge, name: &str, args: Value| {
            handle(
                b,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}),
            )
            .unwrap()
        };
        // A change: sent once, and the agent is told it may have happened.
        let (_dir, mut b, taken) = daemon_that_drops_requests();
        let r = call(&mut b, "agent_status", json!({"status": "working"}));
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(
            taken.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a call that changes something was sent again: {text}"
        );
        assert!(text.contains("may or may not have taken effect"), "{text}");
        // A read: asked again on a new connection.
        let (_dir, mut b, taken) = daemon_that_drops_requests();
        call(&mut b, "tasks_list", json!({}));
        assert_eq!(taken.load(std::sync::atomic::Ordering::SeqCst), 2);
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

    #[test]
    fn a_daemon_of_another_build_is_said_in_every_tool_call() {
        use std::io::{BufRead, BufReader, Write};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("crew.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // A daemon one protocol ahead, refusing every hello as daemons do.
        let other = crew_core::rpc::PROTOCOL_VERSION + 1;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut line = String::new();
                if BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap_or(0)
                    == 0
                {
                    continue;
                }
                let hello: Value = serde_json::from_str(&line).unwrap();
                let refusal = crew_core::rpc::RpcError::new(
                    "protocol",
                    format!(
                        "client speaks protocol {}, daemon speaks {other}",
                        hello["params"]["protocol"]
                    ),
                )
                .with_data(
                    json!({"version": "9.9.9", "protocol": other, "workspace": "/w/payments"}),
                );
                let reply =
                    crew_core::rpc::ServerFrame::err(hello["id"].as_u64().unwrap(), refusal);
                writeln!(stream, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
            }
        });
        let mut b = Bridge {
            identity: Identity {
                socket: socket.to_string_lossy().into_owned(),
                agent_id: "claude-1".into(),
                token: "t".into(),
                orchestrator: false,
                tools: None,
            },
            client: None,
            ends_turns: false,
        };
        let sentence = format!(
            "crew {} (protocol {}) cannot talk to the daemon of /w/payments, which is crew 9.9.9 \
(protocol {other}): restart the team (`crew stop`, then `crew run`)",
            env!("CARGO_PKG_VERSION"),
            crew_core::rpc::PROTOCOL_VERSION
        );
        // The handshake completes whatever the daemon says.
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":LATEST_PROTOCOL}}),
        )
        .unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "crew");
        let r = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
        assert!(!r["result"]["tools"].as_array().unwrap().is_empty());
        // And every call answers with the sentence.
        for (id, name) in [(3, "agents_list"), (4, "task_complete")] {
            let r = handle(
                &mut b,
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}}),
            )
            .unwrap();
            assert_eq!(r["result"]["isError"], true);
            assert_eq!(r["result"]["content"][0]["text"], sentence.as_str());
        }
    }
}
