//! `crew toolbox mcp <name>`: the launcher every toolbox MCP server is
//! reached through (notes/toolboxes.md, Secrets and Prompt cache rules,
//! rule 5).
//!
//! The runtime starts it as a stdio MCP server. It asks the daemon, over the
//! agent's identity, how to reach the server: a local program it starts as a
//! child with the resolved environment, or a remote URL it bridges with the
//! resolved headers. Everything the runtime and the server say to each other
//! passes through it, except two things it answers itself: `initialize` and
//! `tools/list` come from a snapshot pinned for the session, tools sorted by
//! name and `listChanged` never advertised, so a slow, reordered or changed
//! server cannot change what the runtime sees. Values of the server's
//! secrets are replaced in its responses, as a best effort.
//!
//! The pinned snapshot is written by the daemon (`pin`), from the listing a
//! preflight run of this command printed: inside the sandbox the agent
//! directory is read-only to the session.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::rpc::{ToolboxReportKind, ToolboxServerLaunch, Upstream};
use crew_core::toolbox::McpServer;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

mod http;
mod stdio;

/// How long a preflight may take: starting or connecting to the server,
/// `initialize` and every page of `tools/list`.
pub const PREFLIGHT_WITHIN: Duration = Duration::from_secs(30);
/// The format of a pinned snapshot file.
const SNAPSHOT_VERSION: u32 = 1;
/// Ids of the requests the proxy itself sends to the server.
const OWN_ID_PREFIX: &str = "crew-proxy-";
/// Protocol versions a runtime may ask for; another is answered with the
/// server's own.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// What reaches the main loop of a proxy or a preflight.
pub(crate) enum Event {
    /// A message from the runtime (stdin).
    Runtime(Value),
    RuntimeClosed,
    /// A message from the server.
    Upstream(Value),
    /// The server went away, with what is known of why.
    UpstreamClosed(String),
}

/// A connection to the server: messages go out with `send`; what comes back
/// arrives as `Event::Upstream`, and the end as `Event::UpstreamClosed`.
pub(crate) trait Connection: Send {
    fn send(&mut self, message: &Value) -> Result<()>;
    /// Stop the server or close the connection.
    fn close(&mut self);
}

fn connect(
    launch: &ToolboxServerLaunch,
    events: mpsc::Sender<Event>,
) -> Result<Box<dyn Connection>> {
    match &launch.upstream {
        Upstream::Stdio { command, cwd, env } => Ok(Box::new(stdio::Child::start(
            command,
            cwd.as_deref(),
            env,
            &launch.scrub,
            events,
        )?)),
        Upstream::Http {
            url,
            transport,
            headers,
        } => http::connect(url, *transport, headers, events),
    }
}

/// SHA-256 (hex) of what decides how a server is reached and what it
/// serves: its command, environment references, working directory, URL,
/// transport and header references. `required` and `runtimes` are left
/// out: they change nothing the server says.
pub fn definition_hash(server: &McpServer) -> String {
    let definition = json!({
        "command": server.command,
        "env": server.env,
        "cwd": server.cwd,
        "url": server.url,
        "transport": server.transport,
        "headers": server.headers,
    });
    hex(&Sha256::digest(
        serde_json::to_vec(&definition).expect("JSON values serialize"),
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a server said at a preflight: its `initialize` result and its tools,
/// every page, as received.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub initialize: Value,
    pub tools: Vec<Value>,
}

impl Listing {
    pub fn to_json(&self) -> Value {
        json!({ "initialize": self.initialize, "tools": self.tools })
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        let initialize = value
            .get("initialize")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| anyhow!("a listing has no initialize result"))?;
        let tools = value
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| anyhow!("a listing has no tools"))?;
        Ok(Listing { initialize, tools })
    }
}

/// A pinned snapshot: what the session's runtime is shown of a server.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The definition (`definition_hash`) it was taken for.
    pub definition: String,
    pub initialize: Value,
    /// Sorted by name.
    pub tools: Vec<Value>,
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

impl Snapshot {
    fn of(definition: &str, listing: &Listing) -> Self {
        let mut tools = listing.tools.clone();
        // Stable: tools of one name (a server's mistake) keep their order.
        tools.sort_by(|a, b| tool_name(a).cmp(tool_name(b)));
        Snapshot {
            definition: definition.to_string(),
            initialize: listing.initialize.clone(),
            tools,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "version": SNAPSHOT_VERSION,
            "definition": self.definition,
            "initialize": self.initialize,
            "tools": self.tools,
        })
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let value: Value =
            serde_json::from_slice(&text).with_context(|| format!("parsing {}", path.display()))?;
        if value.get("version").and_then(Value::as_u64) != Some(u64::from(SNAPSHOT_VERSION)) {
            // Another format: as if there were none, so it is taken again.
            return Ok(None);
        }
        let definition = value
            .get("definition")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let listing = Listing::from_json(&value)?;
        Ok(Some(Snapshot {
            definition,
            initialize: listing.initialize,
            tools: listing.tools,
        }))
    }

    /// The bytes the definition digest and the tool-definition size are
    /// taken over: what the model can see of the server, its instructions and
    /// its tools. Object keys are sorted (serde_json's map is ordered).
    fn visible(&self) -> Vec<u8> {
        let visible = json!({
            "instructions": self.initialize.get("instructions"),
            "tools": self.tools,
        });
        serde_json::to_vec(&visible).expect("JSON values serialize")
    }

    fn tools_bytes(&self) -> u64 {
        serde_json::to_vec(&self.tools).map_or(0, |b| b.len() as u64)
    }
}

/// Where a server's pinned snapshot lives in the agent directory.
pub fn snapshot_path(agent_dir: &Path, name: &str) -> PathBuf {
    agent_dir
        .join("toolbox")
        .join("mcp")
        .join(format!("{name}.json"))
}

/// What `pin` did with a preflight's listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinOutcome {
    /// No snapshot, another definition, or a refresh: the listing is pinned.
    Written,
    /// The pinned snapshot already says the same.
    Kept,
    /// The listing differs; the pinned snapshot stays (logged by the caller).
    KeptDiffers,
}

impl PinOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            PinOutcome::Written => "written",
            PinOutcome::Kept => "kept",
            PinOutcome::KeptDiffers => "kept_differs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub outcome: PinOutcome,
    /// The pinned snapshot's tools and their definition bytes.
    pub tools: u32,
    pub bytes: u64,
}

/// Pin a preflight's listing for server `name` of the agent whose directory
/// is `agent_dir` (the daemon's side; the session cannot write there). A
/// snapshot is replaced only when it was taken for another definition, or
/// on `refresh` (`crew resume --restart`); otherwise a listing that differs
/// is reported and does not replace it.
pub fn pin(
    agent_dir: &Path,
    name: &str,
    definition: &str,
    listing: &Value,
    refresh: bool,
) -> Result<Pinned> {
    let listing = Listing::from_json(listing)?;
    let candidate = Snapshot::of(definition, &listing);
    let path = snapshot_path(agent_dir, name);
    let pinned = Snapshot::load(&path)?;
    let outcome = match &pinned {
        Some(pinned) if pinned.definition == definition && !refresh => {
            if *pinned == candidate {
                PinOutcome::Kept
            } else {
                PinOutcome::KeptDiffers
            }
        }
        _ => PinOutcome::Written,
    };
    let kept = if outcome == PinOutcome::Written {
        let dir = path.parent().expect("a snapshot path has a parent");
        crew_core::paths::ensure_private_dir(dir)?;
        let bytes = serde_json::to_vec_pretty(&candidate.to_json())?;
        crew_core::paths::write_private_file(&path, &bytes)?;
        candidate
    } else {
        pinned.expect("kept only when one is pinned")
    };
    Ok(Pinned {
        outcome,
        tools: kept.tools.len() as u32,
        bytes: kept.tools_bytes(),
    })
}

/// The definition digest of an agent's toolbox servers and what they add:
/// SHA-256 over each server's pinned snapshot, in `names` order (name
/// order), with its tool count and tool-definition bytes. None while any
/// server has no snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionDigest {
    pub sha256: String,
    pub tools: u32,
    pub bytes: u64,
}

pub fn definition_digest(agent_dir: &Path, names: &[String]) -> Option<DefinitionDigest> {
    let mut hasher = Sha256::new();
    let (mut tools, mut bytes) = (0u32, 0u64);
    for name in names {
        let snapshot = Snapshot::load(&snapshot_path(agent_dir, name)).ok()??;
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(snapshot.visible());
        hasher.update([0]);
        tools += snapshot.tools.len() as u32;
        bytes += snapshot.tools_bytes();
    }
    Some(DefinitionDigest {
        sha256: hex(&hasher.finalize()),
        tools,
        bytes,
    })
}

/// Replaces exact occurrences of secret values in what the server says.
struct Redactor {
    values: Vec<(String, String)>,
}

impl Redactor {
    fn new(launch: &ToolboxServerLaunch) -> Self {
        let mut values: Vec<(String, String)> = launch
            .redact
            .iter()
            .filter(|r| !r.value.is_empty())
            .map(|r| (r.value.clone(), format!("[redacted ${}]", r.name)))
            .collect();
        // A value that contains another is replaced first.
        values.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Redactor { values }
    }

    fn apply(&self, value: &mut Value) {
        if self.values.is_empty() {
            return;
        }
        match value {
            Value::String(text) => {
                for (secret, shown) in &self.values {
                    if text.contains(secret.as_str()) {
                        *text = text.replace(secret.as_str(), shown);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.apply(v)),
            Value::Object(map) => map.values_mut().for_each(|v| self.apply(v)),
            _ => {}
        }
    }
}

/// `text` with the server's secret values replaced, for what goes to the
/// daemon (a preflight's reason, a report).
pub fn redacted(launch: &ToolboxServerLaunch, text: &str) -> String {
    let mut value = Value::String(text.to_string());
    Redactor::new(launch).apply(&mut value);
    match value {
        Value::String(text) => text,
        _ => unreachable!("a string stays a string"),
    }
}

/// Ask the server for its `initialize` result and every page of its tools
/// within `within`, then stop it.
pub fn preflight(launch: &ToolboxServerLaunch, within: Duration) -> Result<Listing> {
    let deadline = Instant::now() + within;
    let (tx, rx) = mpsc::channel();
    let mut connection = connect(launch, tx)?;
    let result = (|| {
        let mut session = Handshake::default();
        let initialize = session.request(
            connection.as_mut(),
            &rx,
            deadline,
            "initialize",
            initialize_params(PROTOCOLS[0]),
        )?;
        connection.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        let tools = session.list_tools(connection.as_mut(), &rx, deadline)?;
        Ok(Listing { initialize, tools })
    })();
    connection.close();
    result
}

fn initialize_params(protocol: &str) -> Value {
    json!({
        "protocolVersion": protocol,
        "capabilities": {},
        "clientInfo": { "name": "crew-toolbox", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The proxy's own requests to the server, before and beside the runtime's.
#[derive(Default)]
struct Handshake {
    next: u64,
}

impl Handshake {
    fn id(&mut self) -> Value {
        self.next += 1;
        Value::String(format!("{OWN_ID_PREFIX}{}", self.next))
    }

    /// Send a request and wait for its answer; anything else the server says
    /// meanwhile is dropped (nobody listens yet).
    fn request(
        &mut self,
        connection: &mut dyn Connection,
        events: &mpsc::Receiver<Event>,
        deadline: Instant,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.id();
        connection
            .send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(left) {
                Ok(Event::Upstream(message)) if message.get("id") == Some(&id) => {
                    return answer(message, method);
                }
                Ok(Event::Upstream(_)) | Ok(Event::Runtime(_)) | Ok(Event::RuntimeClosed) => {}
                Ok(Event::UpstreamClosed(why)) => {
                    bail!("the server stopped during {method}: {why}")
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("the server did not answer {method} in time")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the server went away"),
            }
        }
    }

    fn list_tools(
        &mut self,
        connection: &mut dyn Connection,
        events: &mpsc::Receiver<Event>,
        deadline: Instant,
    ) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<Value> = None;
        let mut seen = HashSet::new();
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request(connection, events, deadline, "tools/list", params)?;
            let listed = page
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("tools/list answered without tools"))?;
            tools.extend(listed.iter().cloned());
            match page.get("nextCursor").filter(|c| !c.is_null()) {
                Some(next) if seen.insert(next.to_string()) => cursor = Some(next.clone()),
                Some(_) => bail!("tools/list repeated a cursor"),
                None => return Ok(tools),
            }
        }
    }
}

/// The result of a JSON-RPC response, or its error as one.
fn answer(message: Value, method: &str) -> Result<Value> {
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        bail!("the server refused {method}: {text}");
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("the server's answer to {method} has no result"))
}

/// What the runtime is told at `initialize`: the pinned result, with the
/// protocol version it asked for when that is one crew speaks, and no
/// `listChanged` anywhere.
fn initialize_result(snapshot: &Snapshot, asked: Option<&str>) -> Value {
    let mut result = snapshot.initialize.clone();
    if let Some(asked) = asked.filter(|v| PROTOCOLS.contains(v)) {
        result["protocolVersion"] = json!(asked);
    }
    if let Some(capabilities) = result
        .get_mut("capabilities")
        .and_then(Value::as_object_mut)
    {
        for (_, capability) in capabilities.iter_mut() {
            if let Some(capability) = capability.as_object_mut() {
                if capability.contains_key("listChanged") {
                    capability.insert("listChanged".into(), json!(false));
                }
            }
        }
    }
    result
}

fn is_list_changed(method: &str) -> bool {
    method.starts_with("notifications/") && method.ends_with("/list_changed")
}

/// Serve the session's runtime (`input`, `output`) for server `launch`, from
/// `snapshot` when one is pinned, else from a live listing taken now and
/// kept for the session (not pinned). `report` tells the daemon what became
/// of the server.
pub fn serve(
    launch: &ToolboxServerLaunch,
    snapshot: Option<Snapshot>,
    input: impl BufRead + Send + 'static,
    output: impl Write,
    report: &mut dyn FnMut(ToolboxReportKind, String),
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Event>();
    let reader = tx.clone();
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            // Not JSON: Null, answered as a parse error.
            let message = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null);
            if reader.send(Event::Runtime(message)).is_err() {
                return;
            }
        }
        let _ = reader.send(Event::RuntimeClosed);
    });
    let mut session = Session {
        launch,
        redactor: Redactor::new(launch),
        output,
        connection: None,
        down: None,
        own: Handshake::default(),
        asked: BTreeMap::new(),
        pinned: snapshot.is_some(),
        snapshot,
        listing: Vec::new(),
        ready: false,
        queued: VecDeque::new(),
        listed: false,
        open: HashSet::new(),
    };
    match connect(launch, tx) {
        Ok(connection) => {
            session.connection = Some(connection);
            let protocol = session
                .snapshot
                .as_ref()
                .and_then(|s| s.initialize.get("protocolVersion"))
                .and_then(Value::as_str)
                .filter(|v| PROTOCOLS.contains(v))
                .unwrap_or(PROTOCOLS[0])
                .to_string();
            session.ask("initialize", initialize_params(&protocol), report)?;
        }
        Err(e) => session.fail(format!("{e:#}"), report)?,
    }
    for event in rx.iter() {
        match event {
            Event::RuntimeClosed => break,
            Event::Runtime(message) => session.on_runtime(message)?,
            Event::Upstream(message) => session.on_server(message, report)?,
            Event::UpstreamClosed(why) => session.fail(why, report)?,
        }
    }
    if let Some(mut connection) = session.connection.take() {
        connection.close();
    }
    Ok(())
}

/// One session of the proxy: the runtime on one side, the server on the
/// other.
struct Session<'a, W: Write> {
    launch: &'a ToolboxServerLaunch,
    redactor: Redactor,
    output: W,
    connection: Option<Box<dyn Connection>>,
    /// Why the server is not there, once it is not.
    down: Option<String>,
    own: Handshake,
    /// The proxy's own requests in flight, by id.
    asked: BTreeMap<String, &'static str>,
    /// The snapshot came pinned (else it is the live listing, once taken).
    pinned: bool,
    snapshot: Option<Snapshot>,
    /// Pages of the proxy's own `tools/list`.
    listing: Vec<Value>,
    /// The server has answered the proxy's `initialize`.
    ready: bool,
    /// Runtime messages waiting for the server, or for the live listing.
    queued: VecDeque<Value>,
    /// The live listing is complete (or not needed).
    listed: bool,
    /// The runtime's requests sent on and not answered yet.
    open: HashSet<String>,
}

impl<W: Write> Session<'_, W> {
    fn write(&mut self, message: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.output, message)?;
        self.output.write_all(b"\n")?;
        self.output.flush()?;
        Ok(())
    }

    fn ask(
        &mut self,
        method: &'static str,
        params: Value,
        report: &mut dyn FnMut(ToolboxReportKind, String),
    ) -> Result<()> {
        let id = self.own.id();
        let sent = match self.connection.as_mut() {
            Some(c) => {
                c.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            }
            None => return Ok(()),
        };
        match sent {
            Ok(()) => {
                self.asked
                    .insert(id.as_str().unwrap_or_default().to_string(), method);
                Ok(())
            }
            Err(e) => self.fail(format!("{e:#}"), report),
        }
    }

    /// Whether a runtime message can be answered now: the snapshot is there,
    /// pinned or listed live.
    fn can_answer_locally(&self) -> bool {
        self.snapshot.is_some() && (self.pinned || self.listed)
    }

    fn refusal(&self, message: &Value) -> Option<Value> {
        let id = message.get("id")?.clone();
        let why = self.down.as_deref().unwrap_or("not started");
        Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": format!("toolbox server {} is not running: {why}", self.launch.name),
            },
        }))
    }

    fn on_runtime(&mut self, message: Value) -> Result<()> {
        if !message.is_object() {
            return self.write(&json!({
                "jsonrpc": "2.0", "id": Value::Null,
                "error": { "code": -32700, "message": "parse error" },
            }));
        }
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" | "tools/list" if !self.can_answer_locally() => {
                if self.down.is_some() {
                    if let Some(refusal) = self.refusal(&message) {
                        self.write(&refusal)?;
                    }
                } else {
                    self.queued.push_back(message);
                }
                Ok(())
            }
            "initialize" => {
                let asked = message
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str);
                let snapshot = self.snapshot.as_ref().expect("checked above");
                let result = initialize_result(snapshot, asked);
                self.write(&json!({"jsonrpc": "2.0", "id": message["id"], "result": result}))
            }
            "tools/list" => {
                let tools = self.snapshot.as_ref().expect("checked above").tools.clone();
                self.write(
                    &json!({"jsonrpc": "2.0", "id": message["id"], "result": { "tools": tools }}),
                )
            }
            "ping" if message.get("id").is_some() => {
                self.write(&json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}))
            }
            // The proxy has told the server itself.
            "notifications/initialized" => Ok(()),
            _ => {
                if self.down.is_some() {
                    if let Some(refusal) = self.refusal(&message) {
                        self.write(&refusal)?;
                    }
                    return Ok(());
                }
                if !self.ready {
                    self.queued.push_back(message);
                    return Ok(());
                }
                self.forward(&message)
            }
        }
    }

    fn forward(&mut self, message: &Value) -> Result<()> {
        if message.get("method").is_some() {
            if let Some(id) = message.get("id") {
                self.open.insert(id.to_string());
            }
        }
        if let Some(connection) = self.connection.as_mut() {
            connection.send(message)?;
        }
        Ok(())
    }

    fn on_server(
        &mut self,
        mut message: Value,
        report: &mut dyn FnMut(ToolboxReportKind, String),
    ) -> Result<()> {
        let own = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| id.starts_with(OWN_ID_PREFIX))
            .filter(|_| message.get("method").is_none())
            .map(str::to_string);
        if let Some(id) = own {
            let Some(method) = self.asked.remove(&id) else {
                return Ok(());
            };
            return match answer(message, method) {
                Ok(result) if method == "initialize" => self.initialized(result, report),
                Ok(page) => self.listed_page(page, report),
                Err(e) if method == "initialize" => self.fail(format!("{e:#}"), report),
                // A failed drift check changes nothing the runtime sees.
                Err(_) => self.listing_done(report),
            };
        }
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if is_list_changed(method) {
            // Never passed on: the runtime's view is pinned.
            return Ok(());
        }
        if method.is_empty() {
            if let Some(id) = message.get("id") {
                self.open.remove(&id.to_string());
            }
        }
        self.redactor.apply(&mut message);
        self.write(&message)
    }

    fn initialized(
        &mut self,
        result: Value,
        report: &mut dyn FnMut(ToolboxReportKind, String),
    ) -> Result<()> {
        if let Some(connection) = self.connection.as_mut() {
            connection.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        }
        if self.snapshot.is_none() {
            self.snapshot = Some(Snapshot {
                definition: self.launch.definition.clone(),
                initialize: result,
                tools: Vec::new(),
            });
        }
        self.ready = true;
        // Pinned: a drift check, logged only. Unpinned: the session's listing.
        self.ask("tools/list", json!({}), report)?;
        // Calls that waited for the server go on; the snapshot's own wait for
        // the listing when there is no pinned one.
        let waiting: Vec<Value> = self.queued.drain(..).collect();
        for message in waiting {
            self.on_runtime(message)?;
        }
        Ok(())
    }

    fn listed_page(
        &mut self,
        page: Value,
        report: &mut dyn FnMut(ToolboxReportKind, String),
    ) -> Result<()> {
        if let Some(tools) = page.get("tools").and_then(Value::as_array) {
            self.listing.extend(tools.iter().cloned());
        }
        match page.get("nextCursor").filter(|c| !c.is_null()).cloned() {
            Some(next) => self.ask("tools/list", json!({ "cursor": next }), report),
            None => self.listing_done(report),
        }
    }

    fn listing_done(&mut self, report: &mut dyn FnMut(ToolboxReportKind, String)) -> Result<()> {
        let listing = Listing {
            initialize: Value::Null,
            tools: std::mem::take(&mut self.listing),
        };
        let live = Snapshot::of(&self.launch.definition, &listing).tools;
        let snapshot = self.snapshot.as_mut().expect("set at initialize");
        if !self.pinned {
            snapshot.tools = live;
        } else if snapshot.tools != live {
            let pinned: HashSet<&str> = snapshot.tools.iter().map(tool_name).collect();
            let new: Vec<&str> = live
                .iter()
                .map(tool_name)
                .filter(|n| !pinned.contains(n))
                .collect();
            if new.is_empty() {
                report(
                    ToolboxReportKind::ListingDiffers,
                    "the server's tools differ from the pinned snapshot, which is kept".into(),
                );
            } else {
                report(
                    ToolboxReportKind::NewTools,
                    format!("not exposed until a refresh: {}", new.join(", ")),
                );
            }
        }
        if !self.listed {
            self.listed = true;
            let waiting: Vec<Value> = self.queued.drain(..).collect();
            for message in waiting {
                self.on_runtime(message)?;
            }
        }
        Ok(())
    }

    /// The server is gone: the daemon is told once, and what waits for it is
    /// refused. `initialize` and `tools/list` stay answered from the
    /// snapshot, so the runtime's view does not change.
    fn fail(
        &mut self,
        why: String,
        report: &mut dyn FnMut(ToolboxReportKind, String),
    ) -> Result<()> {
        if self.down.is_some() {
            return Ok(());
        }
        report(ToolboxReportKind::Failed, why.clone());
        if let Some(mut connection) = self.connection.take() {
            connection.close();
        }
        self.down = Some(why);
        let waiting: Vec<Value> = self.queued.drain(..).collect();
        for message in waiting {
            self.on_runtime(message)?;
        }
        let open: Vec<String> = self.open.drain().collect();
        for id in open {
            let id: Value = serde_json::from_str(&id).unwrap_or(Value::Null);
            if let Some(refusal) = self.refusal(&json!({ "id": id })) {
                self.write(&refusal)?;
            }
        }
        Ok(())
    }
}

/// Copy the pinned snapshots of servers `names` from one agent directory to
/// another (a subagent serves its parent's definitions). A server without
/// a snapshot is left without one.
pub fn copy_snapshots(from: &Path, to: &Path, names: &[String]) -> Result<()> {
    for name in names {
        let source = snapshot_path(from, name);
        let bytes = match std::fs::read(&source) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", source.display())),
        };
        let target = snapshot_path(to, name);
        crew_core::paths::ensure_private_dir(
            target.parent().expect("a snapshot path has a parent"),
        )?;
        crew_core::paths::write_private_file(&target, &bytes)?;
    }
    Ok(())
}
