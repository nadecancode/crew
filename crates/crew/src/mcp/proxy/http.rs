//! A remote toolbox server, bridged from stdio: the MCP streamable HTTP
//! transport (each message a POST, answered with JSON or an event stream,
//! the session kept by `Mcp-Session-Id`), or the older SSE transport (one
//! event stream for everything the server says, messages POSTed to the
//! endpoint it names). A small HTTP/1.1 client, one connection per request.
//!
//! Limits, on purpose: TLS certificates are always verified (against the
//! bundled web roots, plus any in `SSL_CERT_FILE` / `SSL_CERT_DIR`);
//! redirects are not followed; an SSE endpoint must be on the server's own
//! origin; `HTTP_PROXY` / `HTTPS_PROXY` are not honoured, so a server behind
//! a required proxy is out of reach.
//!
//! Every wait is bounded. Resolving, connecting, the TLS handshake and
//! sending a request share one deadline, and a watchdog shuts the socket
//! down when it passes, so no read or write outlives it. The answer's head
//! has its own deadline; its body may pause for at most `ANSWER_IDLE`
//! between reads. A caller with an overall deadline (a preflight) bounds
//! the body by it as well.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::toolbox::McpTransport;
use serde_json::{json, Value};

use super::{Connection, Event};

/// How long resolving, connecting, the TLS handshake and sending a request
/// may take together.
const CONNECT_WITHIN: Duration = Duration::from_secs(30);
/// How long the SSE transport may take to name its endpoint.
const ENDPOINT_WITHIN: Duration = Duration::from_secs(30);
/// How long the server may stay silent while it answers a request.
const ANSWER_IDLE: Duration = Duration::from_secs(300);
/// How long ending a streamable HTTP session may take.
const CLOSE_WITHIN: Duration = Duration::from_secs(5);
/// The longest status or header line, and how many header lines (malformed
/// ones included).
const MAX_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 100;
/// The largest JSON answer, and the largest server-sent event.
const MAX_BODY: u64 = 16 * 1024 * 1024;
const MAX_EVENT: usize = 16 * 1024 * 1024;

/// Read one line of at most `max` bytes (its line break included).
fn read_line(reader: &mut dyn BufRead, max: usize) -> std::io::Result<String> {
    let mut line = Vec::new();
    reader.take(max as u64 + 1).read_until(b'\n', &mut line)?;
    if line.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a line longer than {max} bytes"),
        ));
    }
    String::from_utf8(line)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not UTF-8"))
}

/// The TLS client: the Mozilla roots crew is built with, plus any the
/// operator adds with SSL_CERT_FILE or SSL_CERT_DIR (a company's own CA).
/// Verification is never turned off.
fn tls() -> Result<std::sync::Arc<rustls::ClientConfig>> {
    static CONFIG: std::sync::OnceLock<std::sync::Arc<rustls::ClientConfig>> =
        std::sync::OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut extra: Vec<std::path::PathBuf> = Vec::new();
    if let Some(file) = std::env::var_os("SSL_CERT_FILE") {
        extra.push(file.into());
    }
    if let Some(dir) = std::env::var_os("SSL_CERT_DIR") {
        if let Ok(entries) = std::fs::read_dir(dir) {
            extra.extend(entries.filter_map(|e| e.ok()).map(|e| e.path()));
        }
    }
    for path in extra {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        for cert in rustls_pemfile::certs(&mut BufReader::new(file)).flatten() {
            // A certificate rustls does not take is left out, not fatal.
            let _ = roots.add(cert);
        }
    }
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| std::sync::Arc::new(config)).clone())
}

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
                Some(port) => port
                    .parse()
                    .with_context(|| format!("bad port in {text}"))?,
                None => default,
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (
                    host.to_string(),
                    port.parse()
                        .with_context(|| format!("bad port in {text}"))?,
                ),
                None => (authority.to_string(), default),
            }
        };
        if host.is_empty() {
            bail!("no host in {text}");
        }
        if host.contains(|c: char| c.is_ascii_control() || c == ' ') {
            bail!("bad host in {text}");
        }
        check_target(&target)?;
        Ok(Url {
            https,
            host,
            port,
            target,
        })
    }

    /// `reference` (an absolute URL, a scheme-relative one, an absolute path,
    /// or a relative one) against this URL.
    pub(crate) fn join(&self, reference: &str) -> Result<Self> {
        if reference.starts_with("http://") || reference.starts_with("https://") {
            return Url::parse(reference);
        }
        if let Some(rest) = reference.strip_prefix("//") {
            let scheme = if self.https { "https" } else { "http" };
            return Url::parse(&format!("{scheme}://{rest}"));
        }
        let mut url = self.clone();
        url.target = if reference.starts_with('/') {
            reference.to_string()
        } else {
            let path = self.target.split('?').next().unwrap_or("/");
            let base = &path[..path.rfind('/').map_or(0, |i| i + 1)];
            format!("{base}{reference}")
        };
        check_target(&url.target)?;
        Ok(url)
    }

    /// The endpoint an SSE server names. It must be on the server's own
    /// origin: the server's headers carry its credentials, and crew sends
    /// them nowhere else.
    pub(crate) fn endpoint(&self, reference: &str) -> Result<Self> {
        let endpoint = self.join(reference)?;
        if endpoint.https != self.https
            || endpoint.port != self.port
            || !endpoint.host.eq_ignore_ascii_case(&self.host)
        {
            bail!(
                "the server named an endpoint on another origin ({}); crew sends the server's headers only to {}",
                endpoint.authority(),
                self.authority()
            );
        }
        Ok(endpoint)
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

/// A request target must fit on the request line.
fn check_target(target: &str) -> Result<()> {
    if target.contains(|c: char| c.is_ascii_control() || c == ' ') {
        bail!("a URL path cannot hold spaces or control characters");
    }
    Ok(())
}

/// Why a response other than 2xx is refused. Redirects are not followed.
fn refusal(status: u16) -> String {
    if (300..=399).contains(&status) {
        format!("the server answered HTTP {status}, a redirect, which crew does not follow")
    } else {
        format!("the server answered HTTP {status}")
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// A request that never reached the server: it could not be resolved or
/// connected to, or the TLS handshake failed. A session takes this as the
/// server being gone, unlike an answer that failed.
#[derive(Debug)]
struct Unreachable(String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

fn unreachable(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(Unreachable(format!("{error:#}")))
}

/// The earlier of `at` and `limit`.
fn earliest(at: Instant, limit: Option<Instant>) -> Instant {
    limit.map_or(at, |limit| limit.min(at))
}

/// Shuts a connection down when its deadline passes, so that nothing
/// blocked on it (a TLS handshake, a write, a read) outlives the deadline.
/// `extend` moves the deadline; dropping the watchdog stops it.
struct Watchdog {
    deadline: mpsc::Sender<Option<Instant>>,
    fired: Arc<AtomicBool>,
}

impl Watchdog {
    fn start(socket: TcpStream, by: Instant) -> Self {
        let (deadline, moved) = mpsc::channel::<Option<Instant>>();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        std::thread::spawn(move || {
            let mut by = by;
            loop {
                match moved.recv_timeout(by.saturating_duration_since(Instant::now())) {
                    Ok(Some(next)) => by = next,
                    Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        flag.store(true, Ordering::SeqCst);
                        let _ = socket.shutdown(Shutdown::Both);
                        return;
                    }
                }
            }
        });
        Watchdog { deadline, fired }
    }

    fn extend(&self, by: Instant) {
        let _ = self.deadline.send(Some(by));
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.deadline.send(None);
    }
}

/// The server's addresses, looked up by `by`.
fn resolve(url: &Url, by: Instant) -> Result<Vec<SocketAddr>> {
    let (tx, rx) = mpsc::channel();
    let target = (url.host.clone(), url.port);
    std::thread::spawn(move || {
        let _ = tx.send(target.to_socket_addrs().map(Iterator::collect::<Vec<_>>));
    });
    match rx.recv_timeout(by.saturating_duration_since(Instant::now())) {
        Ok(found) => found.with_context(|| format!("resolving {}", url.host)),
        Err(_) => bail!("resolving {} did not finish in time", url.host),
    }
}

/// A connection to the server, its TLS handshake done, by `by`; a handle
/// that can shut it down from another thread; and the watchdog that does
/// so at `by`.
fn open(url: &Url, by: Instant) -> Result<(Box<dyn Stream>, TcpStream, Watchdog)> {
    let mut last = None;
    for address in resolve(url, by).map_err(unreachable)? {
        let left = by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut stream = match TcpStream::connect_timeout(&address, left) {
            Ok(stream) => stream,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        let handle = stream.try_clone()?;
        let watchdog = Watchdog::start(stream.try_clone()?, by);
        if !url.https {
            return Ok((Box::new(stream), handle, watchdog));
        }
        let name = rustls::pki_types::ServerName::try_from(url.host.clone())
            .map_err(|_| unreachable(anyhow!("{} is not a host name TLS can verify", url.host)))?;
        let mut connection = rustls::ClientConnection::new(tls()?, name)?;
        while connection.is_handshaking() {
            if let Err(e) = connection.complete_io(&mut stream) {
                return Err(unreachable(if watchdog.fired() {
                    anyhow!(
                        "the TLS handshake with {} did not finish in time",
                        url.authority()
                    )
                } else {
                    anyhow!(e).context(format!("TLS with {}", url.authority()))
                }));
            }
        }
        return Ok((
            Box::new(rustls::StreamOwned::new(connection, stream)),
            handle,
            watchdog,
        ));
    }
    Err(unreachable(match last {
        _ if Instant::now() >= by => {
            anyhow!("connecting to {} did not finish in time", url.authority())
        }
        Some(e) => anyhow!(e).context(format!("connecting to {}", url.authority())),
        None => anyhow!("{} has no address", url.host),
    }))
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Box<dyn BufRead + Send>,
    /// Bounds the body by the caller's overall deadline, when it has one.
    watchdog: Option<Watchdog>,
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
            let line = read_line(&mut self.inner, MAX_LINE)?;
            let size = line.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size")
            })?;
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
            read_line(&mut self.inner, MAX_LINE)?;
        }
        Ok(n)
    }
}

/// Send one request and read the response head; the body is left to read.
/// Connecting and sending must finish within `CONNECT_WITHIN`, the head
/// within `head_within` after that, and everything by `overall` when given.
fn request(
    url: &Url,
    method: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    head_within: Duration,
    overall: Option<Instant>,
) -> Result<(Response, TcpStream)> {
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
    let (mut stream, handle, watchdog) =
        open(url, earliest(Instant::now() + CONNECT_WITHIN, overall))?;
    let sent = stream
        .write_all(head.as_bytes())
        .and_then(|()| body.map_or(Ok(()), |body| stream.write_all(body)))
        .and_then(|()| stream.flush());
    if let Err(e) = sent {
        if watchdog.fired() {
            bail!("sending to {} did not finish in time", url.authority());
        }
        return Err(anyhow!(e).context(format!("sending to {}", url.authority())));
    }
    watchdog.extend(earliest(Instant::now() + head_within, overall));
    handle.set_read_timeout(Some(ANSWER_IDLE))?;
    let mut reader = BufReader::new(stream);
    let (status, headers) = match read_head(&mut reader, &url.authority()) {
        Ok(head) => head,
        Err(_) if watchdog.fired() => bail!("{} did not answer in time", url.authority()),
        Err(e) => return Err(e),
    };
    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let body: Box<dyn BufRead + Send> =
        if find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
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
    // Without an overall deadline the body is bounded by ANSWER_IDLE between
    // reads only: a tool may take its time.
    let watchdog = overall.map(|by| {
        watchdog.extend(by);
        watchdog
    });
    Ok((
        Response {
            status,
            headers,
            body,
            watchdog,
        },
        handle,
    ))
}

/// A response's status and headers. At most `MAX_HEADERS` header lines are
/// read, whether they parse or not.
fn read_head(reader: &mut dyn BufRead, authority: &str) -> Result<(u16, Vec<(String, String)>)> {
    let status_line = read_line(reader, MAX_LINE)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("not an HTTP response from {authority}"))?;
    let mut headers = Vec::new();
    for lines in 0.. {
        let line = read_line(reader, MAX_LINE)?;
        if line.is_empty() {
            bail!("{authority} ended its answer within the headers");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if lines >= MAX_HEADERS {
            bail!("{authority} sent more than {MAX_HEADERS} header lines");
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok((status, headers))
}

/// One server-sent event: its name and data.
fn next_event(body: &mut dyn BufRead) -> Result<Option<(String, String)>> {
    next_event_within(body, MAX_EVENT)
}

/// One server-sent event of at most `max` bytes, counting every byte read
/// for it: data, field names, comments and line breaks.
fn next_event_within(body: &mut dyn BufRead, max: usize) -> Result<Option<(String, String)>> {
    let mut name = String::new();
    let mut data: Option<String> = None;
    let mut size = 0;
    loop {
        let line = read_line(body, max)?;
        if line.is_empty() {
            return Ok(None);
        }
        size += line.len();
        if size > max {
            bail!("a server-sent event larger than {max} bytes");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            match data.take() {
                Some(data) => {
                    let kind = if name.is_empty() {
                        "message".to_string()
                    } else {
                        name
                    };
                    return Ok(Some((kind, data)));
                }
                // Nothing to dispatch: what was read is let go.
                None => {
                    name.clear();
                    size = 0;
                    continue;
                }
            }
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
            "data" => match &mut data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => data = Some(value.to_string()),
            },
            _ => {}
        }
    }
}

/// Pass each JSON-RPC message in `data` (one, or a batch) on, and say
/// whether one of them answers the request `id`.
fn pass_on(data: &str, events: &mpsc::Sender<Event>, id: Option<&Value>) -> Result<bool> {
    let messages = match serde_json::from_str::<Value>(data)
        .context("the server sent a message that is not JSON")?
    {
        Value::Array(batch) => batch,
        message => vec![message],
    };
    let mut answered = false;
    for message in messages {
        answered |=
            id.is_some_and(|id| message.get("id") == Some(id) && message.get("method").is_none());
        let _ = events.send(Event::Upstream(message));
    }
    Ok(answered)
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

/// A request that never reached the server ends the session: the request
/// is refused, and the server is gone.
fn unreached(message: &Value, error: &anyhow::Error, events: &mpsc::Sender<Event>) {
    let why = format!("{error:#}");
    failed(message, &why, events);
    if error.downcast_ref::<Unreachable>().is_some() {
        let _ = events.send(Event::UpstreamClosed(why));
    }
}

fn header_list(headers: &BTreeMap<String, String>) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Reach the server at `url`. `overall`, when given, bounds every request
/// on the connection (a preflight's deadline).
pub(crate) fn connect(
    url: &str,
    transport: McpTransport,
    headers: &BTreeMap<String, String>,
    events: mpsc::Sender<Event>,
    overall: Option<Instant>,
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
            overall,
        })),
        McpTransport::Sse => Ok(Box::new(Legacy::start(
            url,
            header_list(headers),
            events,
            overall,
        )?)),
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
    overall: Option<Instant>,
}

impl Streamable {
    fn request_headers(&self) -> Vec<(String, String)> {
        let mut headers = self.headers.clone();
        headers.push(("Content-Type".into(), "application/json".into()));
        headers.push((
            "Accept".into(),
            "application/json, text/event-stream".into(),
        ));
        if let Some(session) = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            headers.push(("Mcp-Session-Id".into(), session));
        }
        if let Some(protocol) = self
            .protocol
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
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
        let overall = self.overall;
        let had_session = session.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        let initializing = message.get("method").and_then(Value::as_str) == Some("initialize");
        // A request (not a notification) must be answered, here or with an
        // error: nothing the server does leaves it waiting.
        let id = message
            .get("id")
            .filter(|_| message.get("method").is_some())
            .cloned();
        std::thread::spawn(move || {
            let (mut response, _handle) =
                match request(&url, "POST", &headers, Some(&body), ANSWER_IDLE, overall) {
                    Ok(response) => response,
                    Err(e) => return unreached(&message, &e, &events),
                };
            if let Some(id) = response.header("mcp-session-id") {
                *session.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
            }
            match response.status {
                200..=299 => {}
                404 if had_session => {
                    failed(&message, "the server ended the session", &events);
                    let _ =
                        events.send(Event::UpstreamClosed("the server ended the session".into()));
                    return;
                }
                status => {
                    return failed(&message, &refusal(status), &events);
                }
            }
            let unanswered = if response.is_event_stream() {
                // The stream is read until it answers the request; then, as
                // the server should close it anyway, it is let go. A message
                // that is not JSON may have been the answer: the request is
                // refused at once rather than left to wait on later events.
                loop {
                    match next_event(response.body.as_mut()) {
                        Ok(Some((kind, data))) if kind == "message" => {
                            note_protocol(initializing, &data, &protocol);
                            match pass_on(&data, &events, id.as_ref()) {
                                Ok(true) => break None,
                                Ok(false) => {}
                                Err(e) => break Some(format!("{e:#}")),
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            break Some("the server's event stream ended without an answer".into())
                        }
                        Err(e) => {
                            break Some(format!("reading the server's event stream failed: {e:#}"))
                        }
                    }
                }
            } else {
                let mut text = String::new();
                let read = (&mut response.body)
                    .take(MAX_BODY + 1)
                    .read_to_string(&mut text);
                match read {
                    Err(e) => Some(format!("reading the server's answer failed: {e}")),
                    Ok(n) if n as u64 > MAX_BODY => {
                        Some("the server's answer is larger than 16 MiB".into())
                    }
                    Ok(_) if text.trim().is_empty() => {
                        Some("the server answered without a message".into())
                    }
                    Ok(_) => {
                        note_protocol(initializing, &text, &protocol);
                        match pass_on(&text, &events, id.as_ref()) {
                            Ok(true) => None,
                            Ok(false) => Some("the server's answer was for another request".into()),
                            Err(e) => Some(format!("{e:#}")),
                        }
                    }
                }
            };
            if let (Some(_), Some(why)) = (&id, unanswered) {
                failed(&message, &why, &events);
            }
        });
        Ok(())
    }

    fn close(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let session = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if session.is_some() {
            // Ending the session is a courtesy, bounded: a server that does
            // not answer in time is left to expire it.
            let url = self.url.clone();
            let headers = self.request_headers();
            let by = earliest(Instant::now() + CLOSE_WITHIN, self.overall);
            let _ = std::thread::spawn(move || {
                request(&url, "DELETE", &headers, None, CLOSE_WITHIN, Some(by))
            })
            .join();
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
    overall: Option<Instant>,
}

impl Legacy {
    fn start(
        url: Url,
        headers: Vec<(String, String)>,
        events: mpsc::Sender<Event>,
        overall: Option<Instant>,
    ) -> Result<Self> {
        let mut get = headers.clone();
        get.push(("Accept".into(), "text/event-stream".into()));
        let endpoint_by = || earliest(Instant::now() + ENDPOINT_WITHIN, overall);
        let (mut response, stream) = request(
            &url,
            "GET",
            &get,
            None,
            ENDPOINT_WITHIN,
            Some(endpoint_by()),
        )?;
        if !(200..=299).contains(&response.status) {
            bail!("{}", refusal(response.status));
        }
        let watchdog = response.watchdog.take().expect("bounded by a deadline");
        let endpoint = loop {
            match next_event(response.body.as_mut()) {
                Ok(Some((kind, data))) if kind == "endpoint" => break url.endpoint(data.trim())?,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) if watchdog.fired() => {
                    bail!("the server did not name its endpoint in time")
                }
                Ok(None) => bail!("the server's event stream ended before it named an endpoint"),
                Err(e) => bail!("the server did not name its endpoint: {e}"),
            }
        };
        // From here the stream stays open as long as the session, bounded by
        // the overall deadline only, when there is one.
        match overall {
            Some(by) => {
                watchdog.extend(by);
                response.watchdog = Some(watchdog);
            }
            None => drop(watchdog),
        }
        stream.set_read_timeout(None)?;
        let reader = events.clone();
        std::thread::spawn(move || {
            let mut body = response.body;
            let _bounded = response.watchdog;
            let why = loop {
                match next_event(body.as_mut()) {
                    // A message that is not JSON cannot be told apart from
                    // the answer some request waits for: the server is
                    // broken, and every request still open is refused.
                    Ok(Some((kind, data))) if kind == "message" => {
                        if let Err(e) = pass_on(&data, &reader, None) {
                            break format!("{e:#}");
                        }
                    }
                    Ok(Some(_)) => {}
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
            overall,
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
        let overall = self.overall;
        std::thread::spawn(move || {
            match request(&url, "POST", &headers, Some(&body), ANSWER_IDLE, overall) {
                Ok((response, _)) if (200..=299).contains(&response.status) => {}
                Ok((response, _)) => failed(&message, &refusal(response.status), &events),
                Err(e) => unreached(&message, &e, &events),
            }
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
        assert_eq!(
            (url.host.as_str(), url.port, url.target.as_str()),
            ("127.0.0.1", 8080, "/sse?x=1")
        );
        assert_eq!(
            url.join("/messages?session=1").unwrap().target,
            "/messages?session=1"
        );
        assert_eq!(url.join("messages").unwrap().target, "/messages");
        let url = Url::parse("https://docs.example.com/mcp").unwrap();
        assert_eq!(
            (url.port, url.authority().as_str()),
            (443, "docs.example.com")
        );
        let url = Url::parse("http://[::1]:9/a/b").unwrap();
        assert_eq!(
            (url.host.as_str(), url.authority().as_str()),
            ("::1", "[::1]:9")
        );
        assert_eq!(url.join("c").unwrap().target, "/a/c");
        assert!(Url::parse("http://user:pw@host/").is_err());
        assert!(Url::parse("ftp://host/").is_err());
        assert!(Url::parse("http://host/a b").is_err());
        assert!(url.join("x\nEvil: 1").is_err());
    }

    #[test]
    fn an_sse_endpoint_stays_on_the_server_s_origin() {
        let url = Url::parse("https://mcp.example.com/sse").unwrap();
        assert_eq!(
            url.endpoint("/messages?s=1").unwrap().target,
            "/messages?s=1"
        );
        assert_eq!(
            url.endpoint("https://MCP.example.com:443/m")
                .unwrap()
                .target,
            "/m"
        );
        assert_eq!(url.endpoint("//mcp.example.com/m").unwrap().target, "/m");
        for elsewhere in [
            "https://other.example/messages",
            "//other.example/messages",
            "http://mcp.example.com/messages",
            "https://mcp.example.com:8443/messages",
        ] {
            let error = url.endpoint(elsewhere).unwrap_err().to_string();
            assert!(error.contains("another origin"), "{elsewhere}: {error}");
        }
    }

    #[test]
    fn a_redirect_is_named_as_one() {
        assert!(refusal(307).contains("redirect"));
        assert_eq!(refusal(500), "the server answered HTTP 500");
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
    fn an_event_is_bounded_by_every_byte_read_for_it() {
        // Empty data lines, field names, comments and line breaks all count.
        for text in [
            "data:\n".repeat(20),
            format!("event: {}\ndata: x\n\n", "n".repeat(60)),
            format!("{}data: x\n\n", format!(": {}\n", "c".repeat(30)).repeat(3)),
        ] {
            let mut body: Box<dyn BufRead> = Box::new(text.as_bytes());
            let error = next_event_within(body.as_mut(), 64).unwrap_err();
            assert!(error.to_string().contains("than 64"), "{text:?}: {error}");
        }
        // Blank lines between events let go of what was read.
        let text = format!(
            "{}data: {}\n\n",
            ": keep-alive\n\n".repeat(20),
            "y".repeat(40)
        );
        let mut body: Box<dyn BufRead> = Box::new(text.as_bytes());
        assert_eq!(
            next_event_within(body.as_mut(), 64).unwrap(),
            Some(("message".to_string(), "y".repeat(40)))
        );
    }

    #[test]
    fn header_lines_are_counted_whether_they_parse_or_not() {
        let many = format!("HTTP/1.1 200 OK\r\n{}\r\n", "no colon here\r\n".repeat(101));
        let mut reader: Box<dyn BufRead> = Box::new(many.as_bytes());
        let error = read_head(reader.as_mut(), "host").unwrap_err();
        assert!(error.to_string().contains("header lines"), "{error}");
        let few = "HTTP/1.1 204 No Content\r\nA: 1\r\nmalformed\r\n\r\n";
        let mut reader: Box<dyn BufRead> = Box::new(few.as_bytes());
        let (status, headers) = read_head(reader.as_mut(), "host").unwrap();
        assert_eq!((status, headers.len()), (204, 1));
        let cut = "HTTP/1.1 200 OK\r\nA: 1\r\n";
        let mut reader: Box<dyn BufRead> = Box::new(cut.as_bytes());
        assert!(read_head(reader.as_mut(), "host").is_err());
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
