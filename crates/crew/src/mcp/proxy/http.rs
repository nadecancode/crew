//! A remote toolbox server, bridged from stdio: the MCP streamable HTTP
//! transport (each message a POST, answered with JSON or an event stream,
//! the session kept by `Mcp-Session-Id`), or the older SSE transport (one
//! event stream for everything the server says, messages POSTed to the
//! endpoint it names). A small HTTP/1.1 client, one connection per request.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::toolbox::McpTransport;
use serde_json::{json, Value};

use super::{Connection, Event};

/// How long connecting to the server may take.
const CONNECT_WITHIN: Duration = Duration::from_secs(30);
/// How long the SSE transport may take to name its endpoint.
const ENDPOINT_WITHIN: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Url {
    https: bool,
    host: String,
    port: u16,
    /// Path and query, starting with `/`.
    target: String,
}

impl Url {
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let (https, rest) = if let Some(rest) = text.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = text.strip_prefix("http://") {
            (false, rest)
        } else {
            bail!("not an http or https URL: {text}");
        };
        let (authority, target) = match rest.find(['/', '?']) {
            Some(at) if rest.as_bytes()[at] == b'/' => (&rest[..at], rest[at..].to_string()),
            Some(at) => (&rest[..at], format!("/{}", &rest[at..])),
            None => (rest, "/".to_string()),
        };
        let target = target.split('#').next().unwrap_or("/").to_string();
        if authority.contains('@') {
            bail!("a server URL cannot carry credentials; put them in headers: {text}");
        }
        let default = if https { 443 } else { 80 };
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let (host, after) = rest
                .split_once(']')
                .ok_or_else(|| anyhow!("unclosed IPv6 address in {text}"))?;
            let port = match after.strip_prefix(':') {
                Some(port) => port.parse().with_context(|| format!("bad port in {text}"))?,
                None => default,
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (
                    host.to_string(),
                    port.parse().with_context(|| format!("bad port in {text}"))?,
                ),
                None => (authority.to_string(), default),
            }
        };
        if host.is_empty() {
            bail!("no host in {text}");
        }
        Ok(Url {
            https,
            host,
            port,
            target,
        })
    }

    /// `reference` (an absolute URL, an absolute path, or a relative one)
    /// against this URL.
    pub(crate) fn join(&self, reference: &str) -> Result<Self> {
        if reference.starts_with("http://") || reference.starts_with("https://") {
            return Url::parse(reference);
        }
        let mut url = self.clone();
        url.target = if reference.starts_with('/') {
            reference.to_string()
        } else {
            let path = self.target.split('?').next().unwrap_or("/");
            let base = &path[..path.rfind('/').map_or(0, |i| i + 1)];
            format!("{base}{reference}")
        };
        Ok(url)
    }

    fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = if self.https { 443 } else { 80 };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// A connection to the server, and a handle that can shut it down from
/// another thread.
fn open(url: &Url) -> Result<(Box<dyn Stream>, TcpStream)> {
    if url.https {
        bail!(
            "crew cannot reach https toolbox servers yet (no TLS in this build): {}",
            url.host
        );
    }
    let mut last = None;
    for address in (url.host.as_str(), url.port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {}", url.host))?
    {
        match TcpStream::connect_timeout(&address, CONNECT_WITHIN) {
            Ok(stream) => {
                let handle = stream.try_clone()?;
                return Ok((Box::new(stream), handle));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => anyhow!(e).context(format!("connecting to {}", url.authority())),
        None => anyhow!("{} has no address", url.host),
    })
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Box<dyn BufRead + Send>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn is_event_stream(&self) -> bool {
        self.header("content-type")
            .is_some_and(|t| t.trim_start().starts_with("text/event-stream"))
    }
}

/// The body of a chunked response, decoded.
struct Chunked<R> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            self.inner.read_line(&mut line)?;
            let size = line.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let room = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..room])?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n;
        if self.left == 0 {
            let mut crlf = String::new();
            self.inner.read_line(&mut crlf)?;
        }
        Ok(n)
    }
}

/// Send one request and read the response head; the body is left to read.
fn request(
    url: &Url,
    method: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<(Response, TcpStream)> {
    let (mut stream, handle) = open(url)?;
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: crew/{}\r\nConnection: close\r\n",
        url.target,
        url.authority(),
        env!("CARGO_PKG_VERSION")
    );
    for (name, value) in headers {
        if value.contains(['\r', '\n']) || name.contains(['\r', '\n', ':']) {
            bail!("header {name} cannot be sent: it holds a line break");
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(body) = body {
        stream.write_all(body)?;
    }
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("not an HTTP response from {}", url.authority()))?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let body: Box<dyn BufRead + Send> = if find("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        Box::new(BufReader::new(Chunked {
            inner: reader,
            left: 0,
            done: false,
        }))
    } else if let Some(length) = find("content-length").and_then(|v| v.parse::<u64>().ok()) {
        Box::new(BufReader::new(reader.take(length)))
    } else {
        Box::new(reader)
    };
    Ok((
        Response {
            status,
            headers,
            body,
        },
        handle,
    ))
}

/// One server-sent event: its name and data.
fn next_event(body: &mut dyn BufRead) -> Result<Option<(String, String)>> {
    let mut name = String::new();
    let mut data: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        if body.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if data.is_empty() {
                name.clear();
                continue;
            }
            let kind = if name.is_empty() { "message".to_string() } else { name };
            return Ok(Some((kind, data.join("\n"))));
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => name = value.to_string(),
            "data" => data.push(value.to_string()),
            _ => {}
        }
    }
}

/// Pass each JSON-RPC message in `data` (one, or a batch) on.
fn pass_on(data: &str, events: &mpsc::Sender<Event>) {
    match serde_json::from_str::<Value>(data) {
        Ok(Value::Array(batch)) => {
            for message in batch {
                let _ = events.send(Event::Upstream(message));
            }
        }
        Ok(message) => {
            let _ = events.send(Event::Upstream(message));
        }
        Err(_) => {}
    }
}

/// An error answer for a request the server did not answer.
fn failed(message: &Value, why: &str, events: &mpsc::Sender<Event>) {
    if let (Some(id), Some(_)) = (message.get("id"), message.get("method")) {
        let _ = events.send(Event::Upstream(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": why },
        })));
    }
}

fn header_list(headers: &BTreeMap<String, String>) -> Vec<(String, String)> {
    headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub(crate) fn connect(
    url: &str,
    transport: McpTransport,
    headers: &BTreeMap<String, String>,
    events: mpsc::Sender<Event>,
) -> Result<Box<dyn Connection>> {
    let url = Url::parse(url)?;
    match transport {
        McpTransport::Http => Ok(Box::new(Streamable {
            url,
            headers: header_list(headers),
            session: Arc::new(Mutex::new(None)),
            protocol: Arc::new(Mutex::new(None)),
            events,
            closed: Arc::new(AtomicBool::new(false)),
        })),
        McpTransport::Sse => Ok(Box::new(Legacy::start(url, header_list(headers), events)?)),
    }
}

/// The streamable HTTP transport.
struct Streamable {
    url: Url,
    headers: Vec<(String, String)>,
    session: Arc<Mutex<Option<String>>>,
    /// The protocol version agreed at `initialize`, sent with every later
    /// request.
    protocol: Arc<Mutex<Option<String>>>,
    events: mpsc::Sender<Event>,
    closed: Arc<AtomicBool>,
}

impl Streamable {
    fn request_headers(&self) -> Vec<(String, String)> {
        let mut headers = self.headers.clone();
        headers.push(("Content-Type".into(), "application/json".into()));
        headers.push((
            "Accept".into(),
            "application/json, text/event-stream".into(),
        ));
        if let Some(session) = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            headers.push(("Mcp-Session-Id".into(), session));
        }
        if let Some(protocol) = self.protocol.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            headers.push(("MCP-Protocol-Version".into(), protocol));
        }
        headers
    }
}

impl Connection for Streamable {
    fn send(&mut self, message: &Value) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            bail!("the connection to the server is closed");
        }
        let url = self.url.clone();
        let headers = self.request_headers();
        let body = serde_json::to_vec(message)?;
        let message = message.clone();
        let session = self.session.clone();
        let protocol = self.protocol.clone();
        let events = self.events.clone();
        let had_session = session.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        let initializing = message.get("method").and_then(Value::as_str) == Some("initialize");
        std::thread::spawn(move || {
            let (mut response, _handle) = match request(&url, "POST", &headers, Some(&body)) {
                Ok(response) => response,
                Err(e) => return failed(&message, &format!("{e:#}"), &events),
            };
            if let Some(id) = response.header("mcp-session-id") {
                *session.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
            }
            match response.status {
                200..=299 => {}
                404 if had_session => {
                    let _ = events.send(Event::UpstreamClosed(
                        "the server ended the session".into(),
                    ));
                    return;
                }
                status => {
                    return failed(&message, &format!("the server answered HTTP {status}"), &events);
                }
            }
            if response.is_event_stream() {
                while let Ok(Some((kind, data))) = next_event(response.body.as_mut()) {
                    if kind == "message" {
                        note_protocol(initializing, &data, &protocol);
                        pass_on(&data, &events);
                    }
                }
            } else {
                let mut text = String::new();
                if response.body.read_to_string(&mut text).is_ok() && !text.trim().is_empty() {
                    note_protocol(initializing, &text, &protocol);
                    pass_on(&text, &events);
                }
            }
        });
        Ok(())
    }

    fn close(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if session.is_some() {
            // Ending the session is a courtesy; a server that does not
            // answer in time is left to expire it.
            let url = self.url.clone();
            let headers = self.request_headers();
            let _ = std::thread::spawn(move || request(&url, "DELETE", &headers, None)).join();
        }
    }
}

fn note_protocol(initializing: bool, data: &str, protocol: &Mutex<Option<String>>) {
    if !initializing {
        return;
    }
    if let Some(version) = serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|m| m.pointer("/result/protocolVersion").cloned())
        .and_then(|v| v.as_str().map(str::to_string))
    {
        *protocol.lock().unwrap_or_else(|e| e.into_inner()) = Some(version);
    }
}

/// The SSE transport: everything the server says comes on one stream.
struct Legacy {
    endpoint: Url,
    headers: Vec<(String, String)>,
    events: mpsc::Sender<Event>,
    stream: TcpStream,
}

impl Legacy {
    fn start(url: Url, headers: Vec<(String, String)>, events: mpsc::Sender<Event>) -> Result<Self> {
        let mut get = headers.clone();
        get.push(("Accept".into(), "text/event-stream".into()));
        let (mut response, stream) = request(&url, "GET", &get, None)?;
        if !(200..=299).contains(&response.status) {
            bail!("the server answered HTTP {}", response.status);
        }
        stream.set_read_timeout(Some(ENDPOINT_WITHIN))?;
        let endpoint = loop {
            match next_event(response.body.as_mut()) {
                Ok(Some((kind, data))) if kind == "endpoint" => break url.join(data.trim())?,
                Ok(Some(_)) => {}
                Ok(None) => bail!("the server's event stream ended before it named an endpoint"),
                Err(e) => bail!("the server did not name its endpoint: {e}"),
            }
        };
        stream.set_read_timeout(None)?;
        let reader = events.clone();
        std::thread::spawn(move || {
            let mut body = response.body;
            let why = loop {
                match next_event(body.as_mut()) {
                    Ok(Some((kind, data))) => {
                        if kind == "message" {
                            pass_on(&data, &reader);
                        }
                    }
                    Ok(None) => break "the server's event stream ended".to_string(),
                    Err(e) => break format!("reading the server's event stream failed: {e}"),
                }
            };
            let _ = reader.send(Event::UpstreamClosed(why));
        });
        Ok(Legacy {
            endpoint,
            headers,
            events,
            stream,
        })
    }
}

impl Connection for Legacy {
    fn send(&mut self, message: &Value) -> Result<()> {
        let url = self.endpoint.clone();
        let mut headers = self.headers.clone();
        headers.push(("Content-Type".into(), "application/json".into()));
        let body = serde_json::to_vec(message)?;
        let message = message.clone();
        let events = self.events.clone();
        std::thread::spawn(move || match request(&url, "POST", &headers, Some(&body)) {
            Ok((response, _)) if (200..=299).contains(&response.status) => {}
            Ok((response, _)) => failed(
                &message,
                &format!("the server answered HTTP {}", response.status),
                &events,
            ),
            Err(e) => failed(&message, &format!("{e:#}"), &events),
        });
        Ok(())
    }

    fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_and_join() {
        let url = Url::parse("http://127.0.0.1:8080/sse?x=1").unwrap();
        assert_eq!((url.host.as_str(), url.port, url.target.as_str()), ("127.0.0.1", 8080, "/sse?x=1"));
        assert_eq!(url.join("/messages?session=1").unwrap().target, "/messages?session=1");
        assert_eq!(url.join("messages").unwrap().target, "/messages");
        let url = Url::parse("https://docs.example.com/mcp").unwrap();
        assert_eq!((url.port, url.authority().as_str()), (443, "docs.example.com"));
        let url = Url::parse("http://[::1]:9/a/b").unwrap();
        assert_eq!((url.host.as_str(), url.authority().as_str()), ("::1", "[::1]:9"));
        assert_eq!(url.join("c").unwrap().target, "/a/c");
        assert!(Url::parse("http://user:pw@host/").is_err());
        assert!(Url::parse("ftp://host/").is_err());
    }

    #[test]
    fn events_are_read_one_at_a_time() {
        let text = ": comment\nevent: endpoint\ndata: /messages\n\ndata: {\"a\":\ndata: 1}\n\n";
        let mut body: Box<dyn BufRead> = Box::new(text.as_bytes());
        assert_eq!(
            next_event(body.as_mut()).unwrap(),
            Some(("endpoint".to_string(), "/messages".to_string()))
        );
        assert_eq!(
            next_event(body.as_mut()).unwrap(),
            Some(("message".to_string(), "{\"a\":\n1}".to_string()))
        );
        assert_eq!(next_event(body.as_mut()).unwrap(), None);
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        let raw = "5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut decoded = String::new();
        Chunked {
            inner: raw.as_bytes(),
            left: 0,
            done: false,
        }
        .read_to_string(&mut decoded)
        .unwrap();
        assert_eq!(decoded, "hello world");
    }
}
