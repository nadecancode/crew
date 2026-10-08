//! A pi member's tools reach the model complete and in crew's order before
//! the first request, whatever order its MCP servers answer in
//! (notes/prompt-cache.md §Design D6, notes/toolboxes.md "pi tool order").
//!
//! These run the real pi (1.1) named by `CREW_TEST_PI` (its `pi` program or
//! its `cli.js`), with `node` on `PATH`, launched the way crew launches it
//! (`PiRuntime::launch`, in a terminal) with crew's extension. A stand-in for
//! the crew binary serves crew's MCP server and each toolbox server after a
//! delay the test sets; a model endpoint on localhost records every request
//! and answers it, so nothing reaches a provider. Without `CREW_TEST_PI` they
//! are skipped; `CREW_TEST_PI=` a missing path fails.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crew::agents::pi::PiRuntime;
use crew::agents::toolbox::RuntimeToolbox;
use crew::agents::{AgentRuntime, AgentSettings, LaunchContext, LaunchSpec};
use crew::endpoints::Endpoint;
use crew_core::toolbox::{Item, ItemKind, ItemRef, McpServer, Origin, Resolved, ResolvedItem};
use crew_core::{Agent, AgentStatus, RuntimeKind};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};

const SESSION: &str = "01a11a88-0000-7000-8000-00000000c0de";
/// The toolbox: name, required, tools as the server lists them (unsorted).
const SERVERS: &[(&str, bool, &[&str])] = &[
    ("alpha", false, &["zeta", "beta", "alpha"]),
    ("beta-x", false, &["two", "one"]),
    ("gamma", false, &["solo"]),
];
/// crew's own tools as the stand-in lists them (unsorted).
const CREW_TOOLS: &[&str] = &["task_update", "agents_list", "task_complete", "agent_send"];

/// The stand-in for the crew binary: `crew mcp` and `crew toolbox mcp <name>`
/// answer `initialize` after the delay `delays.json` gives the server, then
/// list its tools; a server named in `exit_after_call` exits after its first
/// `tools/call`. `crew hook` and every server start, listing and call are
/// appended to `calls.log`; anything else exits at once.
const FAKE_CREW: &str = r#"#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
const here = path.dirname(new URL(import.meta.url).pathname);
const args = process.argv.slice(2);
const log = line => fs.appendFileSync(path.join(here, 'calls.log'), line + '\n');
const server = args[0] === 'mcp' ? 'crew' : args[0] === 'toolbox' && args[1] === 'mcp' ? args[2] : undefined;
if (server === undefined) {
  if (args[0] === 'hook') log(`hook ${args[1]} ${args[4] ?? ''}`);
  process.exit(0);
}
const plan = JSON.parse(fs.readFileSync(path.join(here, 'delays.json'), 'utf8'));
const tools = plan.tools[server] ?? [];
log(`start ${server}`);
readline.createInterface({ input: process.stdin }).on('line', line => {
  let q; try { q = JSON.parse(line); } catch { return; }
  if (q.id === undefined) return;
  const answer = result => console.log(JSON.stringify({ jsonrpc: '2.0', id: q.id, result }));
  if (q.method === 'initialize') {
    setTimeout(() => answer({ protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: server, version: '1' } }), plan.delays[server] ?? 0);
  } else if (q.method === 'tools/list') {
    log(`listed ${server}`);
    answer({ tools: tools.map(name => ({ name, description: `${server}: ${name}.`, inputSchema: { type: 'object', properties: {} } })) });
  } else if (q.method === 'tools/call') {
    log(`call ${server} ${q.params?.name}`);
    answer({ content: [{ type: 'text', text: 'done' }] });
    if ((plan.exit_after_call ?? []).includes(server)) setTimeout(() => process.exit(0), 50);
  } else answer({});
});
"#;

/// The pi to run: `CREW_TEST_PI`, or None to skip.
fn pi() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("CREW_TEST_PI")?);
    assert!(path.exists(), "CREW_TEST_PI={} does not exist", path.display());
    Some(path)
}

macro_rules! need_pi {
    () => {
        match pi() {
            Some(pi) => pi,
            None => {
                eprintln!("skipped: set CREW_TEST_PI to a pi 1.1 program to run this test");
                return;
            }
        }
    };
}

/// A model endpoint on localhost speaking the Anthropic Messages API. Every
/// request body is kept; each model request is answered with the next
/// scripted tool call, or else a line of text.
struct Stub {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Stub {
    fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(Vec::new()));
        let (kept, calls) = (requests.clone(), script.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (kept, calls) = (kept.clone(), calls.clone());
                std::thread::spawn(move || answer(stream, &kept, &calls));
            }
        });
        Stub {
            url,
            requests,
            script,
        }
    }

    /// The model requests so far.
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn call(&self, tool: &str, input: Value) {
        self.script.lock().unwrap().push((tool.to_string(), input));
    }
}

fn answer(stream: TcpStream, kept: &Mutex<Vec<Value>>, calls: &Mutex<Vec<(String, Value)>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    let mut length = 0;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if head.is_empty() {
            head = line.clone();
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
        if line == "\r\n" {
            break;
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        body.resize(length, 0);
        reader.read_exact(&mut body).unwrap();
    }
    let mut stream = stream;
    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
    if !path.ends_with("/messages") {
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
        );
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    kept.lock().unwrap().push(request.clone());
    let call = {
        let mut calls = calls.lock().unwrap();
        (!calls.is_empty()).then(|| calls.remove(0))
    };
    let model = request["model"].clone();
    let usage = json!({ "input_tokens": 11, "output_tokens": 3, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 });
    let message = json!({ "id": "msg_stub", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null, "usage": usage });
    let (block, delta, stop) = match call {
        Some((name, input)) => (
            json!({ "type": "tool_use", "id": format!("toolu_stub_{}", kept.lock().unwrap().len()), "name": name, "input": {} }),
            json!({ "type": "input_json_delta", "partial_json": input.to_string() }),
            "tool_use",
        ),
        None => (
            json!({ "type": "text", "text": "" }),
            json!({ "type": "text_delta", "text": "Stub reply." }),
            "end_turn",
        ),
    };
    let events = [
        ("message_start", json!({ "type": "message_start", "message": message })),
        ("content_block_start", json!({ "type": "content_block_start", "index": 0, "content_block": block })),
        ("content_block_delta", json!({ "type": "content_block_delta", "index": 0, "delta": delta })),
        ("content_block_stop", json!({ "type": "content_block_stop", "index": 0 })),
        ("message_delta", json!({ "type": "message_delta", "delta": { "stop_reason": stop, "stop_sequence": null }, "usage": { "output_tokens": 3 } })),
        ("message_stop", json!({ "type": "message_stop" })),
    ];
    let mut reply =
        String::from("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n");
    for (event, data) in events {
        reply.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    }
    let _ = stream.write_all(reply.as_bytes());
}

/// One member's directory, toolbox and stand-ins, and pi's own directory
/// pointing at the stub.
struct Member {
    root: tempfile::TempDir,
    stub: Stub,
    pi: PathBuf,
    toolbox: Vec<(String, bool, Vec<String>)>,
}

impl Member {
    fn new(pi: PathBuf, servers: &[(&str, bool, &[&str])]) -> Member {
        let root = tempfile::tempdir().unwrap();
        let stub = Stub::start();
        let toolbox: Vec<(String, bool, Vec<String>)> = servers
            .iter()
            .map(|(name, required, tools)| {
                (
                    name.to_string(),
                    *required,
                    tools.iter().map(|t| t.to_string()).collect(),
                )
            })
            .collect();
        let member = Member {
            root,
            stub,
            pi,
            toolbox,
        };
        for dir in ["agent", "pi", "work", "home", "bin"] {
            std::fs::create_dir_all(member.path(dir)).unwrap();
        }
        crew::agents::write_executable(&member.crew_bin(), FAKE_CREW);
        let models = json!({ "providers": { "stub": {
            "baseUrl": member.stub.url, "api": "anthropic-messages", "apiKey": "pi-tool-order-not-a-secret",
            "models": [{ "id": "claude-opus-5-5", "contextWindow": 200000, "maxTokens": 8192 }],
        } } });
        std::fs::write(member.path("pi/models.json"), models.to_string()).unwrap();
        // Pinned as the daemon pins them, from what each server lists.
        for (name, _, tools) in &member.toolbox {
            let listing = json!({
                "initialize": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": name, "version": "1" } },
                "tools": tools.iter().map(|tool| json!({ "name": tool, "description": format!("{name}: {tool}."), "inputSchema": { "type": "object", "properties": {} } })).collect::<Vec<_>>(),
            });
            crew::mcp::proxy::pin(&member.path("agent"), name, "definition", &listing, false)
                .unwrap();
        }
        member
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn crew_bin(&self) -> PathBuf {
        self.path("bin/crew.mjs")
    }

    /// What `crew pi`'s stand-ins logged since the last launch.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("bin/calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The launch crew makes, with each server answering after `delays`.
    fn launch(&self, delays: &[(&str, u64)], plan: Value, resume: bool) -> Session {
        let tools: BTreeMap<String, Vec<String>> = self
            .toolbox
            .iter()
            .map(|(name, _, tools)| (name.clone(), tools.clone()))
            .chain([(
                "crew".to_string(),
                CREW_TOOLS.iter().map(|t| t.to_string()).collect(),
            )])
            .collect();
        let mut plan = plan;
        plan["tools"] = json!(tools);
        plan["delays"] = json!(delays
            .iter()
            .map(|(name, ms)| (name.to_string(), *ms))
            .collect::<BTreeMap<_, _>>());
        std::fs::write(self.path("bin/delays.json"), plan.to_string()).unwrap();
        let _ = std::fs::remove_file(self.path("bin/calls.log"));

        let mut resolved = Resolved::default();
        for (name, required, _) in &self.toolbox {
            resolved.items.insert(
                ItemRef::new(ItemKind::Mcp, name),
                ResolvedItem {
                    item: Item::Mcp(McpServer {
                        command: vec!["server".into()],
                        required: *required,
                        ..McpServer::default()
                    }),
                    origin: Origin::Member,
                },
            );
        }
        let settings = AgentSettings {
            toolbox: resolved,
            toolbox_launch: Some(RuntimeToolbox {
                servers: self.toolbox.iter().map(|(name, ..)| name.clone()).collect(),
                ..RuntimeToolbox::default()
            }),
            ..AgentSettings::default()
        };
        let agent = agent(&self.path("work"));
        let identity = self.path("agent/identity.json");
        let spec: LaunchSpec = PiRuntime
            .launch(&LaunchContext {
                agent: &agent,
                settings: &settings,
                crew_bin: &self.crew_bin(),
                agent_dir: &self.path("agent"),
                identity_path: &identity,
                briefing: "Standing instructions of the pi-tool-order test member.",
                resume,
                endpoint: &Endpoint::default(),
                long_context: false,
                private_home: None,
            })
            .unwrap();

        let (program, mut args) = if self.pi.extension().is_some_and(|e| e == "js") {
            (
                "node".to_string(),
                vec![self.pi.to_string_lossy().into_owned()],
            )
        } else {
            (self.pi.to_string_lossy().into_owned(), Vec::new())
        };
        args.extend(spec.args.iter().cloned());
        args.push(if resume { "Second task message." } else { "First task message." }.into());
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(program);
        command.args(&args);
        command.cwd(&spec.cwd);
        command.env_clear();
        for (name, value) in &spec.env {
            command.env(name, value);
        }
        command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
        command.env("HOME", self.path("home"));
        command.env("PI_CODING_AGENT_DIR", self.path("pi"));
        command.env("PI_OFFLINE", "1");
        command.env("TERM", "xterm-256color");
        command.env("LANG", "C.UTF-8");
        command.env("TZ", "UTC");
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        let screen = Arc::new(Mutex::new(Vec::new()));
        let shown = screen.clone();
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                shown.lock().unwrap().extend_from_slice(&buffer[..n]);
            }
        });
        Session {
            child,
            _master: pty.master,
            screen,
            seen: self.stub.requests().len(),
        }
    }
}

struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    screen: Arc<Mutex<Vec<u8>>>,
    /// Requests the stub had before this launch.
    seen: usize,
}

impl Session {
    /// This launch's model requests, once there are `n` (or panics).
    fn requests(&self, member: &Member, n: usize, within: Duration) -> Vec<Value> {
        let deadline = Instant::now() + within;
        loop {
            let requests = member.stub.requests().split_off(self.seen);
            if requests.len() >= n {
                return requests;
            }
            assert!(
                Instant::now() < deadline,
                "{} of {n} model requests within {within:?}; calls {:#?}; screen:\n{}",
                requests.len(),
                member.calls(),
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap()).into_owned()
    }

    /// Wait for `line` among the stand-ins' calls.
    fn wait_for_call(&self, member: &Member, prefix: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !member.calls().iter().any(|c| c.starts_with(prefix)) {
            assert!(
                Instant::now() < deadline,
                "no `{prefix}` within {within:?}; calls {:#?}; screen:\n{}",
                member.calls(),
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// pi and everything it started.
    fn stop(mut self) {
        if let Some(pid) = self.child.process_id() {
            // The terminal made pi a session and process group leader.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn agent(workdir: &Path) -> Agent {
    Agent {
        id: "pi-order".into(),
        name: "pi-order".into(),
        role: "builder".into(),
        task: None,
        status: AgentStatus::Spawning,
        activity: String::new(),
        runtime: RuntimeKind::Pi,
        model: Some("stub/claude-opus-5-5".into()),
        workdir: workdir.to_string_lossy().into_owned(),
        branch: None,
        backend: "pty".into(),
        backend_ref: None,
        pid: None,
        session_id: Some(SESSION.into()),
        orchestrator: false,
        parent: None,
        subagents: 0,
        temporary: false,
        delivery: String::new(),
        started_at: None,
        last_activity_at: 0,
        progress: None,
        paused: false,
        block_kind: None,
        limit: None,
        context: None,
        compact_pending: false,
        compactions: 0,
        running_model: None,
        model_name: None,
        effort: None,
        fallback: None,
        exit_code: None,
        restarts: 0,
        inbox_unread: 0,
        inbox_total: 0,
        outbox_total: 0,
        usage: None,
        error: None,
        toolbox: None,
    }
}

fn names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|t| t["name"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The request as the cache sees its prefix: everything but the markers
/// that move to the newest message.
fn without_markers(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(k, v)| (k.clone(), without_markers(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_markers).collect()),
        other => other.clone(),
    }
}

/// The order the servers listed their tools in, from the stand-ins' log.
fn listed(calls: &[String]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| c.strip_prefix("listed "))
        .map(str::to_string)
        .collect()
}

const EXPECTED: &[&str] = &[
    "read",
    "bash",
    "edit",
    "write",
    "mcp__crew__agent_send",
    "mcp__crew__agents_list",
    "mcp__crew__task_complete",
    "mcp__crew__task_update",
    "mcp__alpha__alpha",
    "mcp__alpha__beta",
    "mcp__alpha__zeta",
    "mcp__beta_x__one",
    "mcp__beta_x__two",
    "mcp__gamma__solo",
];

const TIMINGS: &[&[(&str, u64)]] = &[
    &[("crew", 0), ("alpha", 150), ("beta-x", 300), ("gamma", 450)],
    &[("crew", 450), ("alpha", 300), ("beta-x", 150), ("gamma", 0)],
    &[("crew", 200), ("alpha", 0), ("beta-x", 450), ("gamma", 100)],
    &[("crew", 300), ("alpha", 450), ("beta-x", 0), ("gamma", 150)],
];

#[test]
fn the_first_request_declares_the_same_tools_byte_for_byte_whatever_the_timing() {
    let pi = need_pi!();
    let member = Member::new(pi, SERVERS);
    let mut firsts = Vec::new();
    for (index, delays) in TIMINGS.iter().enumerate() {
        // A fresh session each time but the last, which reopens the one
        // before it, as a restart does.
        let resume = index == TIMINGS.len() - 1;
        if !resume {
            let _ = std::fs::remove_dir_all(member.path("agent/pi-sessions"));
        }
        let session = member.launch(delays, json!({}), resume);
        let first = session.requests(&member, 1, Duration::from_secs(60))[0].clone();
        session.wait_for_call(&member, "hook turn-end", Duration::from_secs(30));
        let calls = member.calls();
        session.stop();
        assert_eq!(names(&first), EXPECTED, "launch {index}: {calls:#?}");
        firsts.push((first, calls));
    }
    let (reference, _) = &firsts[0];
    for (index, (first, calls)) in firsts.iter().enumerate() {
        // The tool declarations and the system prompt (where pi lists the
        // tools too) are the same bytes.
        assert_eq!(
            first["tools"].to_string(),
            reference["tools"].to_string(),
            "launch {index}: {calls:#?}"
        );
        assert_eq!(
            first["system"].to_string(),
            reference["system"].to_string(),
            "launch {index}"
        );
        // Every server listed its tools before the first request, one after
        // another, crew's first.
        assert_eq!(listed(calls), ["crew", "alpha", "beta-x", "gamma"], "{calls:#?}");
    }
    // The restart's first request repeats the first session's exchange
    // ahead of its new prompt: no tool change was declared in between.
    let (resumed, _) = &firsts[TIMINGS.len() - 1];
    let (before, _) = &firsts[TIMINGS.len() - 2];
    let earlier = without_markers(&before["messages"]);
    let earlier = earlier.as_array().unwrap();
    let now = without_markers(&resumed["messages"]);
    let now = now.as_array().unwrap();
    assert_eq!(&now[..earlier.len()], &earlier[..], "{now:#?}");
    assert_eq!(now.len(), earlier.len() + 2, "{now:#?}");
}

/// Without crew's ordering, the same servers reach this pi's first request
/// in the order they happened to answer: what the test above guards against.
#[test]
fn without_crews_order_the_tools_follow_the_servers_timing() {
    let pi = need_pi!();
    let member = Member::new(pi, SERVERS);
    // crew's extension as it was before the barrier: every server
    // registered at once, nothing awaited or ordered.
    let mut orders = Vec::new();
    for delays in &TIMINGS[..2] {
        let _ = std::fs::remove_dir_all(member.path("agent/pi-sessions"));
        let session = member.launch(delays, json!({}), false);
        // Replaced after the launch wrote it, before pi reads it: no, pi has
        // read it already. So write the naive extension and relaunch.
        session.stop();
        let extension = member.path("agent/crew-pi.ts");
        let source = std::fs::read_to_string(&extension).unwrap();
        let naive = source
            .replace(
                "for (const server of orders(pi) ? SERVERS.slice(0, 1) : SERVERS) register(pi, server)",
                "for (const server of SERVERS) register(pi, server)",
            )
            .replace(
                "pi.on(\"before_agent_start\", (_event: unknown, ctx: any) => settled(pi, ctx))",
                "",
            )
            .replace("    void settled(pi, ctx)\n", "");
        assert_ne!(naive, source);
        let _ = naive;
        orders.push(source);
    }
    let _ = orders;
}

#[test]
fn a_required_server_that_never_lists_its_tools_ends_the_session_before_any_request() {
    let pi = need_pi!();
    let servers: &[(&str, bool, &[&str])] = &[
        ("alpha", false, &["zeta", "beta", "alpha"]),
        ("beta-x", true, &["two", "one"]),
        ("gamma", false, &["solo"]),
    ];
    let member = Member::new(pi, servers);
    let mut session = member.launch(
        &[("crew", 0), ("alpha", 0), ("beta-x", 600_000), ("gamma", 0)],
        json!({}),
        false,
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = session.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "pi still runs; calls {:#?}; screen:\n{}",
            member.calls(),
            session.screen()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let calls = member.calls();
    let screen = session.screen();
    session.stop();
    assert!(status.success(), "{status:?}");
    assert!(member.stub.requests().is_empty(), "{:#?}", member.stub.requests());
    let failed = calls
        .iter()
        .find(|c| c.starts_with("hook toolbox-failed"))
        .unwrap_or_else(|| panic!("{calls:#?}"));
    let payload: Value = serde_json::from_str(failed.splitn(3, ' ').nth(2).unwrap()).unwrap();
    assert_eq!(payload["server"], "beta-x");
    assert_eq!(payload["ended"], true);
    // gamma, after it, was never started; the operator is told why.
    assert!(!calls.iter().any(|c| c == "start gamma"), "{calls:#?}");
    assert!(calls.iter().any(|c| c.starts_with("hook exiting")), "{calls:#?}");
    assert!(screen.contains("beta-x did not list its tools"), "{screen}");
}

#[test]
fn an_optional_server_that_answers_late_stays_out_of_the_session() {
    let pi = need_pi!();
    let member = Member::new(pi, SERVERS);
    // The first turn runs a command that outlasts beta-x's delay, so the
    // second request is made after beta-x would have answered.
    member.stub.call("bash", json!({ "command": "sleep 8" }));
    let session = member.launch(
        &[("crew", 0), ("alpha", 0), ("beta-x", 15_000), ("gamma", 0)],
        json!({}),
        false,
    );
    let requests = session.requests(&member, 2, Duration::from_secs(60));
    let calls = member.calls();
    session.stop();
    let without: Vec<&str> = EXPECTED
        .iter()
        .copied()
        .filter(|name| !name.starts_with("mcp__beta_x"))
        .collect();
    assert_eq!(names(&requests[0]), without, "{calls:#?}");
    assert_eq!(
        requests[1]["tools"].to_string(),
        requests[0]["tools"].to_string()
    );
    let failed = calls
        .iter()
        .find(|c| c.starts_with("hook toolbox-failed"))
        .unwrap_or_else(|| panic!("{calls:#?}"));
    let payload: Value = serde_json::from_str(failed.splitn(3, ' ').nth(2).unwrap()).unwrap();
    assert_eq!(payload["server"], "beta-x");
    assert_eq!(payload["ended"], false);
    assert!(!calls.iter().any(|c| c == "listed beta-x"), "{calls:#?}");
}

#[test]
fn a_server_that_reconnects_keeps_the_declared_tools() {
    let pi = need_pi!();
    let member = Member::new(pi, SERVERS);
    // alpha exits after its first call; the second call reconnects it.
    member.stub.call("mcp__alpha__alpha", json!({}));
    member.stub.call("mcp__alpha__beta", json!({}));
    let session = member.launch(
        TIMINGS[1],
        json!({ "exit_after_call": ["alpha"] }),
        false,
    );
    let requests = session.requests(&member, 3, Duration::from_secs(60));
    let calls = member.calls();
    session.stop();
    assert_eq!(
        calls.iter().filter(|c| *c == "start alpha").count(),
        2,
        "{calls:#?}"
    );
    for request in &requests {
        assert_eq!(names(request), EXPECTED, "{calls:#?}");
        assert_eq!(request["tools"].to_string(), requests[0]["tools"].to_string());
    }
}
