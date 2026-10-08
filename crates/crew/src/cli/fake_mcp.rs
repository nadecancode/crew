//! `crew fake-mcp`: a stand-in MCP server for tests of crew's toolbox proxy
//! (`crew toolbox mcp`). It speaks MCP on stdio and misbehaves on request:
//! tools in a different order at every listing, a description that changes
//! after a listing, an exit or a silence at a chosen point, a secret echoed
//! in an error. It never prints a variable's value except where a test asks
//! for exactly that (`--echo-token-in-error`).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Args, Clone)]
pub struct FakeMcpArgs {
    /// The tools it serves
    #[arg(long, value_delimiter = ',', default_value = "alpha,beta,gamma")]
    tools: Vec<String>,
    /// List the tools in a different order every time
    #[arg(long)]
    shuffle: bool,
    /// Count listings in this file, across processes (a preflight and the
    /// session are two processes)
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// From this listing on (counted from 1), the first tool's description
    /// changes
    #[arg(long, value_name = "N")]
    change_description_after: Option<u64>,
    /// Exit when this method arrives: initialize or tools/list
    #[arg(long, value_name = "METHOD")]
    exit_during: Option<String>,
    /// Never answer this method
    #[arg(long, value_name = "METHOD")]
    hang: Option<String>,
    /// Exit this long after starting
    #[arg(long, value_name = "SECONDS")]
    exit_after: Option<f64>,
    /// Advertise listChanged and announce a changed list after initialize
    #[arg(long)]
    list_changed: bool,
    /// Tools per page of tools/list (nextCursor beyond)
    #[arg(long, value_name = "N")]
    page: Option<usize>,
    /// The instructions it returns at initialize
    #[arg(long, default_value = "")]
    instructions: String,
    /// The `echo_token` tool fails with this variable's value in its error
    #[arg(long, value_name = "VAR")]
    echo_token_in_error: Option<String>,
    /// Put this variable's value everywhere a server could echo it: the
    /// instructions, each tool's description and a schema key, its log
    /// (stderr), and the error of `--refuse-initialize`
    #[arg(long, value_name = "VAR")]
    leak: Option<String>,
    /// Answer initialize with an error
    #[arg(long)]
    refuse_initialize: bool,
    /// Answer this method with a broken message: over streamable HTTP, JSON
    /// cut short or an event stream whose only event is not JSON; over SSE,
    /// an event that is not JSON
    #[arg(long, value_name = "METHOD")]
    break_answer: Option<String>,
    /// `NAME=<sha256 hex>`: the `env` tool says whether the variable's value
    /// has this digest (never the value)
    #[arg(long, value_name = "NAME=HEX")]
    expect_sha256: Vec<String>,
    /// Append every method received to this file
    #[arg(long, value_name = "FILE")]
    log: Option<PathBuf>,
    /// Serve MCP over HTTP on 127.0.0.1 instead of stdio, printing the URL
    /// first: `http` (streamable HTTP) or `sse` (the legacy SSE transport)
    #[arg(long, value_name = "TRANSPORT")]
    http: Option<String>,
    /// Streamable HTTP: answer requests as an event stream, not JSON
    #[arg(long)]
    stream_responses: bool,
    /// `NAME=<sha256 hex>`: refuse (401) a request whose header NAME does
    /// not have this digest
    #[arg(long, value_name = "NAME=HEX")]
    require_header: Vec<String>,
    /// Serve https with this certificate chain (PEM) and its key
    #[arg(long, value_name = "FILE", requires = "tls_key")]
    tls_cert: Option<PathBuf>,
    #[arg(long, value_name = "FILE")]
    tls_key: Option<PathBuf>,
}

fn listings(state: &Option<PathBuf>) -> u64 {
    let Some(path) = state else { return 1 };
    let count = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    let _ = std::fs::write(path, count.to_string());
    count
}

/// A small, unseeded shuffle: the order differs from run to run.
fn shuffle<T>(items: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

/// What the stand-in does with one message.
enum Outcome {
    /// Messages to send back (a response, then any notifications).
    Reply(Vec<Value>),
    /// Stay silent (`--hang`, or a notification).
    Silent,
    /// Exit now (`--exit-during`).
    Exit,
}

struct Server {
    args: FakeMcpArgs,
    pages: BTreeMap<String, Vec<Value>>,
}

impl Server {
    fn handle(&mut self, message: &Value) -> Result<Outcome> {
        let args = &self.args;
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if let Some(log) = &args.log {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)?;
            let entry = if method.is_empty() {
                "response"
            } else {
                method
            };
            writeln!(file, "{entry}")?;
        }
        if args.exit_during.as_deref() == Some(method) {
            return Ok(Outcome::Exit);
        }
        if args.hang.as_deref() == Some(method) {
            return Ok(Outcome::Silent);
        }
        let Some(id) = message.get("id").cloned() else {
            return Ok(Outcome::Silent);
        };
        if method.is_empty() {
            return Ok(Outcome::Silent);
        }
        let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let refuse = |code: i64, text: String| json!({"jsonrpc": "2.0", "id": id, "error": { "code": code, "message": text }});
        let leaked = args
            .leak
            .as_deref()
            .and_then(|var| std::env::var(var).ok())
            .unwrap_or_default();
        if !leaked.is_empty() {
            eprintln!("fake-mcp {method} with token {leaked}");
        }
        Ok(Outcome::Reply(match method {
            "initialize" => {
                if args.refuse_initialize {
                    return Ok(Outcome::Reply(vec![refuse(
                        -32002,
                        format!("initialize refused for token {leaked}"),
                    )]));
                }
                let protocol = message
                    .pointer("/params/protocolVersion")
                    .cloned()
                    .unwrap_or(json!("2025-06-18"));
                let mut out = vec![reply(json!({
                    "protocolVersion": protocol,
                    "capabilities": { "tools": { "listChanged": args.list_changed } },
                    "serverInfo": { "name": "fake-mcp", "version": "1" },
                    "instructions": format!("{}{leaked}", args.instructions),
                }))];
                if args.list_changed {
                    out.push(
                        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                    );
                }
                out
            }
            "tools/list" => {
                let cursor = message
                    .pointer("/params/cursor")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let remaining = match cursor.and_then(|c| self.pages.remove(&c)) {
                    Some(rest) => rest,
                    None => {
                        let count = listings(&args.state);
                        let changed = args.change_description_after.is_some_and(|n| count > n);
                        let mut tools: Vec<Value> = args
                            .tools
                            .iter()
                            .enumerate()
                            .map(|(i, name)| {
                                let mut description = format!("The {name} tool.{leaked}");
                                if changed && i == 0 {
                                    description.push_str(" Changed.");
                                }
                                json!({
                                    "name": name,
                                    "description": description,
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": if leaked.is_empty() {
                                            json!({})
                                        } else {
                                            json!({ leaked.clone(): { "type": "string" } })
                                        },
                                    },
                                })
                            })
                            .collect();
                        if args.shuffle {
                            shuffle(&mut tools);
                        }
                        tools
                    }
                };
                let page = match args.page {
                    Some(size) if remaining.len() > size => {
                        let rest = remaining[size..].to_vec();
                        let cursor = format!("page-{}", self.pages.len() + 1);
                        self.pages.insert(cursor.clone(), rest);
                        json!({ "tools": remaining[..size].to_vec(), "nextCursor": cursor })
                    }
                    _ => json!({ "tools": remaining }),
                };
                vec![reply(page)]
            }
            "tools/call" => {
                let name = message
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match name {
                    "echo_token" => {
                        let var = args.echo_token_in_error.as_deref().unwrap_or("");
                        let value = std::env::var(var).unwrap_or_default();
                        vec![refuse(-32001, format!("bad token {value}"))]
                    }
                    "env" => {
                        let mut names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
                        names.sort();
                        let matches: BTreeMap<String, bool> = args
                            .expect_sha256
                            .iter()
                            .filter_map(|pair| pair.split_once('='))
                            .map(|(name, hex)| {
                                let value = std::env::var(name).unwrap_or_default();
                                (name.to_string(), sha256_hex(&value) == hex)
                            })
                            .collect();
                        let text = json!({ "names": names, "matches": matches }).to_string();
                        vec![reply(
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )]
                    }
                    _ => {
                        let text = message
                            .pointer("/params/arguments")
                            .cloned()
                            .unwrap_or(Value::Null)
                            .to_string();
                        vec![reply(
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )]
                    }
                }
            }
            "ping" => vec![reply(json!({}))],
            _ => vec![refuse(-32601, format!("no method {method}"))],
        }))
    }
}

fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn run(args: FakeMcpArgs) -> Result<()> {
    if let Some(secs) = args.exit_after {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            std::process::exit(4);
        });
    }
    let http = args.http.clone();
    let mut server = Server {
        args,
        pages: BTreeMap::new(),
    };
    if let Some(transport) = http {
        return http::serve(server, &transport);
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match server.handle(&message)? {
            Outcome::Exit => std::process::exit(3),
            Outcome::Silent => {}
            Outcome::Reply(messages) => {
                for message in messages {
                    serde_json::to_writer(&mut stdout, &message)?;
                    stdout.write_all(b"\n")?;
                    stdout.flush()?;
                }
            }
        }
    }
    Ok(())
}

/// The stand-in over HTTP (and https): one connection at a time is enough
/// for a test.
mod http {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use anyhow::{bail, Context, Result};
    use serde_json::Value;

    use super::{sha256_hex, Outcome, Server};

    trait Stream: Read + Write + Send {}
    impl<T: Read + Write + Send> Stream for T {}

    struct Request {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Request {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    fn read_request(stream: &mut dyn Stream) -> Result<Request> {
        // One request per connection (the client closes it), so nothing
        // read ahead is lost with the reader.
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let mut parts = line.split_whitespace();
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            bail!("no request line");
        };
        let (method, path) = (method.to_string(), path.to_string());
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let length = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        Ok(Request {
            method,
            path,
            headers,
            body,
        })
    }

    fn respond(
        stream: &mut dyn Stream,
        status: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<()> {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )?;
        for (k, v) in headers {
            write!(stream, "{k}: {v}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.write_all(body)?;
        stream.flush()?;
        Ok(())
    }

    /// The head of an event stream, chunked, kept open.
    fn start_stream(stream: &mut dyn Stream, headers: &[(&str, String)]) -> Result<()> {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n",
        )?;
        for (k, v) in headers {
            write!(stream, "{k}: {v}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        stream.flush()?;
        Ok(())
    }

    fn event(stream: &mut dyn Stream, name: Option<&str>, data: &str) -> Result<()> {
        let mut text = String::new();
        if let Some(name) = name {
            text.push_str(&format!("event: {name}\n"));
        }
        text.push_str(&format!("data: {data}\n\n"));
        write!(stream, "{:x}\r\n{text}\r\n", text.len())?;
        stream.flush()?;
        Ok(())
    }

    fn end_stream(stream: &mut dyn Stream) -> Result<()> {
        stream.write_all(b"0\r\n\r\n")?;
        stream.flush()?;
        Ok(())
    }

    fn tls(cert: &std::path::Path, key: &std::path::Path) -> Result<Arc<rustls::ServerConfig>> {
        let certs = rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(cert)?))
            .collect::<Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(key)?))?
            .context("no private key")?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Ok(Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(certs, key)?,
        ))
    }

    pub(super) fn serve(server: Server, transport: &str) -> Result<()> {
        let tls = match (&server.args.tls_cert, &server.args.tls_key) {
            (Some(cert), Some(key)) => Some(tls(cert, key)?),
            _ => None,
        };
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let path = if transport == "sse" { "/sse" } else { "/mcp" };
        let (scheme, host) = if tls.is_some() {
            ("https", "localhost")
        } else {
            ("http", "127.0.0.1")
        };
        println!("{scheme}://{host}:{port}{path}");
        std::io::stdout().flush()?;
        let required: Vec<(String, String)> = server
            .args
            .require_header
            .iter()
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let stream_responses = server.args.stream_responses;
        let server = Arc::new(Mutex::new(server));
        // Legacy SSE: the open stream every answer goes out on.
        let sse: Arc<Mutex<Option<Box<dyn Stream>>>> = Arc::new(Mutex::new(None));
        let session = "fake-session-1";
        for tcp in listener.incoming() {
            let tcp = tcp?;
            let mut stream: Box<dyn Stream> = match &tls {
                Some(config) => Box::new(rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone())?,
                    tcp,
                )),
                None => Box::new(tcp),
            };
            // A client that gives up on the handshake (an untrusted
            // certificate) is no reason to stop.
            let Ok(request) = read_request(stream.as_mut()) else {
                continue;
            };
            let refused = required.iter().any(|(name, hex)| {
                request.header(name).map(sha256_hex).as_deref() != Some(hex.as_str())
            });
            if refused {
                respond(stream.as_mut(), "401 Unauthorized", &[], b"no")?;
                continue;
            }
            if transport == "sse" {
                if request.method == "GET" && request.path == "/sse" {
                    start_stream(stream.as_mut(), &[])?;
                    event(stream.as_mut(), Some("endpoint"), "/messages?session=1")?;
                    *sse.lock().unwrap() = Some(stream);
                    continue;
                }
                if request.method == "POST" && request.path.starts_with("/messages") {
                    respond(stream.as_mut(), "202 Accepted", &[], b"")?;
                    let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
                        continue;
                    };
                    let method = message.get("method").and_then(Value::as_str);
                    let broken = method.is_some()
                        && server.lock().unwrap().args.break_answer.as_deref() == method;
                    let outcome = server.lock().unwrap().handle(&message)?;
                    match outcome {
                        Outcome::Exit => std::process::exit(3),
                        Outcome::Silent => {}
                        Outcome::Reply(_) if broken => {
                            if let Some(out) = sse.lock().unwrap().as_mut() {
                                event(out.as_mut(), Some("message"), "{broken")?;
                            }
                        }
                        Outcome::Reply(messages) => {
                            if let Some(out) = sse.lock().unwrap().as_mut() {
                                for message in messages {
                                    event(out.as_mut(), Some("message"), &message.to_string())?;
                                }
                            }
                        }
                    }
                    continue;
                }
                respond(stream.as_mut(), "404 Not Found", &[], b"")?;
                continue;
            }
            // Streamable HTTP.
            match request.method.as_str() {
                "DELETE" => {
                    respond(stream.as_mut(), "200 OK", &[], b"")?;
                    continue;
                }
                "POST" => {}
                _ => {
                    respond(stream.as_mut(), "405 Method Not Allowed", &[], b"")?;
                    continue;
                }
            }
            let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
                respond(stream.as_mut(), "400 Bad Request", &[], b"")?;
                continue;
            };
            let initializing = message.get("method").and_then(Value::as_str) == Some("initialize");
            if !initializing && request.header("mcp-session-id") != Some(session) {
                respond(stream.as_mut(), "400 Bad Request", &[], b"no session")?;
                continue;
            }
            let outcome = server.lock().unwrap().handle(&message)?;
            let messages = match outcome {
                Outcome::Exit => std::process::exit(3),
                Outcome::Silent => {
                    if message.get("id").is_some() && message.get("method").is_some() {
                        // A hung request: hold the connection open.
                        std::thread::spawn(move || {
                            let _keep = stream;
                            std::thread::sleep(std::time::Duration::from_secs(3600));
                        });
                    } else {
                        respond(stream.as_mut(), "202 Accepted", &[], b"")?;
                    }
                    continue;
                }
                Outcome::Reply(messages) => messages,
            };
            let mut headers = vec![];
            if initializing {
                headers.push(("Mcp-Session-Id", session.to_string()));
            }
            let method = message.get("method").and_then(Value::as_str);
            if method.is_some() && server.lock().unwrap().args.break_answer.as_deref() == method {
                if stream_responses {
                    start_stream(stream.as_mut(), &headers)?;
                    event(stream.as_mut(), None, "{broken")?;
                    end_stream(stream.as_mut())?;
                } else {
                    headers.push(("Content-Type", "application/json".to_string()));
                    respond(
                        stream.as_mut(),
                        "200 OK",
                        &headers,
                        br#"{"jsonrpc":"2.0","id":"#,
                    )?;
                }
                continue;
            }
            if stream_responses {
                start_stream(stream.as_mut(), &headers)?;
                // Notifications first, then the response, as a server may.
                for message in messages.iter().rev() {
                    event(stream.as_mut(), None, &message.to_string())?;
                }
                end_stream(stream.as_mut())?;
            } else {
                headers.push(("Content-Type", "application/json".to_string()));
                // JSON carries the response; notifications are dropped.
                respond(
                    stream.as_mut(),
                    "200 OK",
                    &headers,
                    messages[0].to_string().as_bytes(),
                )?;
            }
        }
        Ok(())
    }
}
