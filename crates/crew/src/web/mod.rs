//! The web dashboard's server, inside the daemon (notes/web-design.md
//! §transport, §security).
//!
//! It runs only after `crew web` asked for it (`web_open`), on one port of
//! both loopback addresses, under a name made at its start and never used
//! again: `http://<label>.localhost:<port>/`. It serves the embedded bundle
//! and a WebSocket that carries the daemon's own protocol, one connection
//! per socket, through the same [`Session`](crate::daemon::server) the Unix
//! socket uses.
//!
//! A browser gets in with a one-time pairing code (printed by `crew web`),
//! which it trades for a session token and a verifier; every `hello` with
//! the token is answered with the secret whose SHA-256 the verifier is, so
//! the page can tell its server from whoever holds the port later. Codes,
//! tokens and secrets live in this process's memory only.
//!
//! Stopping goes in one order: the credentials go, then every connection,
//! and only then the listeners, so no address is free while a token is
//! still good.

pub(crate) mod assets;
pub(crate) mod control;
pub(crate) mod daemons;
pub(crate) mod http;
pub(crate) mod hub;
pub(crate) mod page;
pub mod process;
pub(crate) mod ws;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crew_core::rpc::{RpcError, WebPaired};
use crew_core::util::{ct_eq, new_token, now_ms};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

/// How long a pairing code works.
pub(crate) const CODE_TTL_MS: i64 = 120_000;
/// Pairing codes outstanding at once; a new one drops the oldest.
pub(crate) const MAX_CODES: usize = 8;
/// Browser sessions at once.
pub(crate) const MAX_SESSIONS: usize = 16;
/// Connections not authenticated yet (a head being read, a WebSocket before
/// `hello`); more are closed at accept.
pub(crate) const MAX_UNAUTHENTICATED: usize = 32;
/// Authenticated WebSockets (a page has an index connection, a workspace
/// connection and a terminal's).
pub(crate) const MAX_AUTHENTICATED: usize = 64;
/// Ports tried from the workspace's own.
const PORTS_TRIED: u16 = 16;
/// The largest `web_prefs` object.
pub(crate) const MAX_PREFS_BYTES: usize = 16 * 1024;
/// The `meta` key of the preferences in `crew.db`.
pub(crate) const PREFS_KEY: &str = "web_prefs";
/// At most one `web.auth_failed` event per this long.
const AUTH_FAILED_EVERY_MS: i64 = 60_000;

/// Where the server is while it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Serving {
    /// 32 hex digits, new at every start.
    pub label: String,
    pub port: u16,
}

impl Serving {
    /// `<label>.localhost:<port>`: the only name the server answers to.
    pub fn host(&self) -> String {
        format!("{}.localhost:{}", self.label, self.port)
    }

    /// `http://<label>.localhost:<port>/`.
    pub fn url(&self) -> String {
        format!("http://{}/", self.host())
    }
}

struct Code {
    value: String,
    expires_at: i64,
}

struct WebSession {
    id: u64,
    token: String,
    secret: String,
    /// The last successful `hello` (or the pairing).
    last_hello: i64,
}

/// A connection the server accepted, until the last task of it is gone
/// (see [`Lease`]). Closed by `close` when the server stops.
struct ConnEntry {
    close: watch::Sender<bool>,
    /// The connection's task, once spawned.
    task: Option<JoinHandle<()>>,
    /// The web session it said `hello` with: it counts as authenticated
    /// from the moment the registry let it in, not from when the reply was
    /// sent.
    session: Option<u64>,
}

/// Codes, sessions and connections: everything the server keeps that
/// `dispatch` reads too. Behind a plain mutex, never held across an await.
/// What is counted (connections with and without a session, a session's
/// connections) is counted from `conns`, so nothing can drift.
#[derive(Default)]
pub(crate) struct Registry {
    serving: Option<Serving>,
    /// Step 1 of stopping happened: no `web_pair` or `hello` succeeds.
    stopping: bool,
    /// Step 2 of stopping happened: connections accepted now are closed.
    closing: bool,
    codes: VecDeque<Code>,
    sessions: Vec<WebSession>,
    next_session: u64,
    conns: HashMap<u64, ConnEntry>,
    next_conn: u64,
    failures: u32,
    failures_reported_at: Option<i64>,
}

fn verifier(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fill `buf` from the kernel's random source: how many bytes, or -1.
#[cfg(target_os = "linux")]
fn fill_random(buf: &mut [u8]) -> isize {
    // SAFETY: the pointer and length name `buf`.
    unsafe { libc::getrandom(buf.as_mut_ptr().cast(), buf.len(), 0) }
}

/// `getentropy` (at most 256 bytes a call, all or nothing).
#[cfg(not(target_os = "linux"))]
fn fill_random(buf: &mut [u8]) -> isize {
    let len = buf.len().min(256);
    // SAFETY: the pointer and length name the start of `buf`.
    if unsafe { libc::getentropy(buf.as_mut_ptr().cast(), len) } == 0 {
        len as isize
    } else {
        -1
    }
}

/// 128 random bits as 32 hex digits.
fn random_label() -> String {
    let mut bytes = [0u8; 16];
    let mut filled = 0;
    while filled < bytes.len() {
        let n = fill_random(&mut bytes[filled..]);
        if n > 0 {
            filled += n as usize;
        } else if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            // No getrandom: two v4 UUIDs' random bits are as good.
            return new_token()[..32].to_string();
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Registry {
    fn live(&self) -> Result<(), RpcError> {
        if self.serving.is_none() || self.stopping {
            return Err(RpcError::unavailable(
                "the web dashboard is not served (it is stopping or stopped)",
            ));
        }
        Ok(())
    }

    /// A new pairing code.
    pub fn code(&mut self, now: i64) -> String {
        self.codes.retain(|c| c.expires_at > now);
        while self.codes.len() >= MAX_CODES {
            self.codes.pop_front();
        }
        let value = new_token();
        self.codes.push_back(Code {
            value: value.clone(),
            expires_at: now + CODE_TTL_MS,
        });
        value
    }

    /// Trade a code for a session. A code works once, until it expires.
    pub fn pair(&mut self, code: &str, now: i64) -> Result<WebPaired, RpcError> {
        self.live()?;
        self.codes.retain(|c| c.expires_at > now);
        let Some(at) = self.codes.iter().position(|c| ct_eq(&c.value, code)) else {
            return Err(RpcError::unauthorized(
                "this pairing code is wrong, spent or expired: run `crew web` again",
            ));
        };
        if self.sessions.len() >= MAX_SESSIONS {
            // Make room: the idle session whose last hello is oldest.
            let idle = self
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| self.open(s.id) == 0)
                .min_by_key(|(_, s)| s.last_hello)
                .map(|(i, _)| i);
            match idle {
                Some(i) => {
                    self.sessions.remove(i);
                }
                None => {
                    return Err(RpcError::unavailable(format!(
                        "{MAX_SESSIONS} browser sessions are open; close a dashboard tab and try \
again"
                    )))
                }
            }
        }
        self.codes.remove(at);
        let (token, secret) = (new_token(), new_token());
        self.next_session += 1;
        let paired = WebPaired {
            token: token.clone(),
            verifier: verifier(&secret),
        };
        self.sessions.push(WebSession {
            id: self.next_session,
            token,
            secret,
            last_hello: now,
        });
        Ok(paired)
    }

    /// A connection, unless the server is closing or too many are not
    /// authenticated yet: its id and the signal that closes it.
    fn admit(&mut self) -> Option<(u64, watch::Receiver<bool>)> {
        if self.closing || self.serving.is_none() || self.unauthenticated() >= MAX_UNAUTHENTICATED {
            return None;
        }
        self.next_conn += 1;
        let (close, rx) = watch::channel(false);
        self.conns.insert(
            self.next_conn,
            ConnEntry {
                close,
                task: None,
                session: None,
            },
        );
        Some((self.next_conn, rx))
    }

    /// A `hello` with a session token on connection `conn`: the session's
    /// id and secret. From here the connection counts as authenticated with
    /// it, for as long as the connection lasts, whatever becomes of the
    /// reply.
    pub fn hello(&mut self, conn: u64, token: &str, now: i64) -> Result<(u64, String), RpcError> {
        self.live()?;
        let Some(at) = self.sessions.iter().position(|s| ct_eq(&s.token, token)) else {
            return Err(RpcError::unauthorized(
                "this browser is signed out: run `crew web` and open the address it prints",
            ));
        };
        if self.authenticated() >= MAX_AUTHENTICATED {
            return Err(RpcError::unavailable(format!(
                "{MAX_AUTHENTICATED} dashboard connections are open; close a tab"
            )));
        }
        let session = &mut self.sessions[at];
        let entry = match self.conns.get_mut(&conn) {
            Some(entry) if entry.session.is_none() => entry,
            Some(_) => return Err(RpcError::forbidden("this connection said hello already")),
            None => return Err(RpcError::unavailable("this connection is closing")),
        };
        entry.session = Some(session.id);
        session.last_hello = now;
        Ok((session.id, session.secret.clone()))
    }

    /// Connection `conn` is gone, every task of it.
    fn release(&mut self, conn: u64) {
        self.conns.remove(&conn);
    }

    fn unauthenticated(&self) -> usize {
        self.conns.values().filter(|c| c.session.is_none()).count()
    }

    fn authenticated(&self) -> usize {
        self.conns.values().filter(|c| c.session.is_some()).count()
    }

    /// The connections open with session `id`.
    fn open(&self, id: u64) -> usize {
        self.conns
            .values()
            .filter(|c| c.session == Some(id))
            .count()
    }

    /// Codes and sessions: every one goes.
    fn revoke(&mut self) {
        self.codes.clear();
        self.sessions.clear();
    }

    /// A refused `web_pair` or `hello`: the count to report, when one is
    /// due.
    fn failed(&mut self, now: i64) -> Option<u32> {
        self.failures += 1;
        let due = self
            .failures_reported_at
            .is_none_or(|at| now - at >= AUTH_FAILED_EVERY_MS);
        due.then(|| {
            self.failures_reported_at = Some(now);
            std::mem::take(&mut self.failures)
        })
    }

    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }
}

/// How a listener is bound: `std::net::TcpListener::bind` (never
/// `SO_REUSEPORT`); tests put their own in its place.
pub(crate) type Binder = Arc<dyn Fn(SocketAddr) -> io::Result<std::net::TcpListener> + Send + Sync>;

fn bind_listener(addr: SocketAddr) -> io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// The IPv6 loopback does not exist on this host.
fn no_ipv6(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EADDRNOTAVAIL) | Some(libc::EAFNOSUPPORT)
    )
}

fn in_use(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AddrInUse
}

/// A test's ear on each step of stopping: it is told the step and holds the
/// stop until it answers.
#[cfg(test)]
pub(crate) type StopHook =
    tokio::sync::mpsc::UnboundedSender<(u8, tokio::sync::oneshot::Sender<()>)>;

/// What a web server tells whoever runs it, for its log.
pub(crate) enum News {
    /// Serving at this address.
    Started(String),
    /// Stopped; by itself, for this reason.
    Stopped(Option<String>),
    /// A browser was paired; this many sessions now.
    Paired(usize),
    /// This many refused logins (at most one report a minute).
    AuthFailed(u32),
}

/// Whoever runs a web server: where its ports start, where its news goes,
/// and what serves a connection it accepted.
pub(crate) trait Host: Send + Sync {
    fn first_port(&self) -> u16;
    fn news(&self, news: News);
    fn serve(
        &self,
        stream: tokio::net::TcpStream,
        admitted: Admitted,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

/// The running listeners.
struct Server {
    serving: Serving,
    v4: JoinHandle<()>,
    /// None: served on `127.0.0.1` alone (no IPv6 loopback when it started).
    v6: Option<JoinHandle<()>>,
}

/// A web server: its listeners, its codes and sessions, its connections.
pub(crate) struct Web {
    registry: Mutex<Registry>,
    /// Who runs it; set once, before it serves.
    host: OnceLock<Arc<dyn Host>>,
    /// Starting and stopping, one at a time.
    lifecycle: tokio::sync::Mutex<Option<Server>>,
    /// A connection ended.
    gone: Notify,
    bind: Mutex<Binder>,
    /// Test hook: told each step of stopping, which waits for the reply.
    #[cfg(test)]
    pub(crate) stop_hook: Mutex<Option<StopHook>>,
    /// Test clock: added to `now_ms()`.
    #[cfg(test)]
    pub(crate) skew: std::sync::atomic::AtomicI64,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            registry: Mutex::new(Registry::default()),
            host: OnceLock::new(),
            lifecycle: tokio::sync::Mutex::new(None),
            gone: Notify::new(),
            bind: Mutex::new(Arc::new(bind_listener)),
            #[cfg(test)]
            stop_hook: Mutex::new(None),
            #[cfg(test)]
            skew: std::sync::atomic::AtomicI64::new(0),
        }
    }
}

/// A connection the server accepted, held by every task of it (the
/// connection's own, its writer, its push forwarder): the connection is
/// gone, and leaves the registry, when the last of them is. Whatever a task
/// holds is dropped before its lease (see [`spawn_held`]), so by then no
/// frame of it can be written any more.
pub(crate) struct Lease {
    web: Arc<Web>,
    pub(crate) id: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.web.registry().release(self.id);
        self.web.gone.notify_waiters();
    }
}

/// A future that holds a lease: its own state is dropped first, the lease
/// last, whether it ran to its end or was aborted.
pub(crate) struct Held<F> {
    task: std::pin::Pin<Box<F>>,
    _lease: Option<Arc<Lease>>,
}

impl<F: std::future::Future> std::future::Future for Held<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<F::Output> {
        self.get_mut().task.as_mut().poll(cx)
    }
}

/// Spawn a task of the connection `lease` is of (none: a socket
/// connection's).
pub(crate) fn spawn_held<F>(lease: Option<Arc<Lease>>, task: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(Held {
        task: Box::pin(task),
        _lease: lease,
    })
}

/// A connection the server admitted: its lease, and the signal that closes
/// it.
pub(crate) struct Admitted {
    pub lease: Arc<Lease>,
    pub close: watch::Receiver<bool>,
}

impl Web {
    /// Who runs this server. Set once; a later call changes nothing.
    pub(crate) fn set_host(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }

    fn news(&self, news: News) {
        if let Some(host) = self.host.get() {
            host.news(news);
        }
    }

    /// Bind with `bind` from now on.
    #[cfg(test)]
    pub(crate) fn set_binder(&self, bind: Binder) {
        *self.bind.lock().unwrap_or_else(|e| e.into_inner()) = bind;
    }

    fn bind(&self, addr: SocketAddr) -> io::Result<std::net::TcpListener> {
        let bind = self.bind.lock().unwrap_or_else(|e| e.into_inner()).clone();
        bind(addr)
    }

    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> i64 {
        #[cfg(test)]
        return now_ms() + self.skew.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        now_ms()
    }

    /// The dashboard's address while it is served, else empty.
    #[cfg(test)]
    pub(crate) fn url(&self) -> String {
        self.registry()
            .serving
            .as_ref()
            .map(Serving::url)
            .unwrap_or_default()
    }

    pub(crate) fn serving(&self) -> Option<Serving> {
        self.registry().serving.clone()
    }

    /// Accept a connection, unless the server is closing or too many are
    /// not authenticated yet.
    fn admit(self: &Arc<Self>) -> Option<Admitted> {
        let (id, close) = self.registry().admit()?;
        Some(Admitted {
            lease: Arc::new(Lease {
                web: self.clone(),
                id,
            }),
            close,
        })
    }

    fn track(&self, id: u64, task: JoinHandle<()>) {
        if let Some(entry) = self.registry().conns.get_mut(&id) {
            entry.task = Some(task);
        }
    }

    /// `web_pair` on a web connection.
    pub(crate) fn pair(&self, code: &str) -> Result<WebPaired, RpcError> {
        let now = self.now();
        let (result, report, sessions) = {
            let mut registry = self.registry();
            let result = registry.pair(code, now);
            let report = match &result {
                Err(e) if e.code == "unauthorized" => registry.failed(now),
                _ => None,
            };
            (result, report, registry.sessions())
        };
        if let Some(n) = report {
            self.news(News::AuthFailed(n));
        }
        if result.is_ok() {
            self.news(News::Paired(sessions));
        }
        result
    }

    /// `hello` with `token` on web connection `conn`: the session and its
    /// secret.
    pub(crate) fn hello(&self, conn: u64, token: Option<&str>) -> Result<(u64, String), RpcError> {
        let now = self.now();
        let (result, report) = {
            let mut registry = self.registry();
            let result = registry.hello(conn, token.unwrap_or_default(), now);
            let report = match &result {
                Err(e) if e.code == "unauthorized" => registry.failed(now),
                _ => None,
            };
            (result, report)
        };
        if let Some(n) = report {
            self.news(News::AuthFailed(n));
        }
        result
    }

    /// A `hello` refused before the token was looked at (role, protocol).
    pub(crate) fn refused(&self) {
        let now = self.now();
        let report = self.registry().failed(now);
        if let Some(n) = report {
            self.news(News::AuthFailed(n));
        }
    }

    /// Serve, if not served yet, and make a pairing code.
    #[cfg(test)]
    pub(crate) async fn open(
        self: &Arc<Self>,
        port: Option<u16>,
    ) -> Result<crew_core::rpc::WebInfo, RpcError> {
        let (serving, started) = self.listen(port).await?;
        let code = self.code();
        Ok(crew_core::rpc::WebInfo {
            url: serving.url(),
            port: serving.port,
            pair_url: format!("{}#pair={code}", serving.url()),
            expires_in_secs: (CODE_TTL_MS / 1000) as u32,
            started,
            workspace: None,
            workspace_url: None,
            joined: Vec::new(),
        })
    }

    /// A new pairing code.
    pub(crate) fn code(&self) -> String {
        let now = self.now();
        self.registry().code(now)
    }

    /// Serve, if not served yet: where, and whether this call started it.
    pub(crate) async fn listen(
        self: &Arc<Self>,
        port: Option<u16>,
    ) -> Result<(Serving, bool), RpcError> {
        // Port 0 would give each address another port the system picks, and
        // a name with neither in it.
        if port == Some(0) {
            return Err(RpcError::invalid(
                "port 0 cannot be served: choose a port from 1 to 65535, or leave --port out",
            ));
        }
        let mut lifecycle = self.lifecycle.lock().await;
        let started = lifecycle.is_none();
        if let Some(server) = lifecycle.as_ref() {
            if port.is_some_and(|p| p != server.serving.port) {
                return Err(RpcError::conflict(format!(
                    "the dashboard is already served on port {}; `crew web --stop` first to \
use another",
                    server.serving.port
                )));
            }
        } else {
            let server = self.start(port)?;
            *lifecycle = Some(server);
        }
        let serving = lifecycle.as_ref().expect("served").serving.clone();
        drop(lifecycle);
        if started {
            self.news(News::Started(serving.url()));
        }
        Ok((serving, started))
    }

    /// End the connections `ids` now: each one's task is aborted and
    /// awaited, so whatever it owned has been dropped when this returns.
    pub(crate) async fn cancel(&self, ids: &[u64]) {
        let tasks: Vec<JoinHandle<()>> = {
            let mut registry = self.registry();
            ids.iter()
                .filter_map(|id| registry.conns.get_mut(id).and_then(|c| c.task.take()))
                .collect()
        };
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Bind both loopback addresses on one port, then make the name.
    fn start(self: &Arc<Self>, port: Option<u16>) -> Result<Server, RpcError> {
        let first = port.unwrap_or_else(|| {
            self.host
                .get()
                .map(|host| host.first_port())
                .unwrap_or(23000)
        });
        let tries = if port.is_some() { 1 } else { PORTS_TRIED };
        let mut bound = None;
        for p in (0..tries).filter_map(|i| first.checked_add(i)) {
            let v4 = match self.bind(SocketAddr::from((Ipv4Addr::LOCALHOST, p))) {
                Ok(listener) => listener,
                Err(e) if in_use(&e) => continue,
                Err(e) => {
                    return Err(RpcError::unavailable(format!(
                        "cannot listen on 127.0.0.1:{p}: {e}"
                    )))
                }
            };
            match self.bind(SocketAddr::from((Ipv6Addr::LOCALHOST, p))) {
                Ok(v6) => {
                    bound = Some((p, v4, Some(v6)));
                    break;
                }
                // Someone answers for the name on [::1]: not this port.
                Err(e) if in_use(&e) => continue,
                Err(e) if no_ipv6(&e) => {
                    bound = Some((p, v4, None));
                    break;
                }
                Err(e) => {
                    return Err(RpcError::unavailable(format!(
                        "cannot listen on [::1]:{p}: {e}"
                    )))
                }
            }
        }
        let Some((port, v4, v6)) = bound else {
            return Err(match port {
                Some(p) => RpcError::unavailable(format!(
                    "port {p} is taken on 127.0.0.1 or [::1]; choose another with --port"
                )),
                None => RpcError::unavailable(format!(
                    "ports {first} to {} are all taken; choose one with `crew web --port`",
                    first.saturating_add(PORTS_TRIED - 1)
                )),
            });
        };
        // Only now, with both addresses held, does the name exist.
        let serving = Serving {
            label: random_label(),
            port,
        };
        {
            let mut registry = self.registry();
            registry.serving = Some(serving.clone());
            registry.stopping = false;
            registry.closing = false;
        }
        let v4 = spawn_accept(self, v4).map_err(|e| RpcError::internal(e.to_string()))?;
        let v6 = v6
            .map(|v6| spawn_accept(self, v6))
            .transpose()
            .map_err(|e| RpcError::internal(e.to_string()))?;
        Ok(Server { serving, v4, v6 })
    }

    /// Stop serving: every code, token and secret goes, then every
    /// connection, then the listeners. `why`: the server stopped itself.
    /// False when nothing was served.
    pub(crate) async fn stop(&self, why: Option<String>) -> bool {
        let mut lifecycle = self.lifecycle.lock().await;
        let Some(server) = lifecycle.take() else {
            return false;
        };
        // 1. No pairing or hello succeeds from here on.
        {
            let mut registry = self.registry();
            registry.stopping = true;
            registry.revoke();
        }
        self.step(1).await;
        // 2. Every web connection is closed: told to, then made to. A
        // connection is gone when every task of it is (its lease), so once
        // `conns` is empty nothing of any connection is left to write.
        let closes: Vec<watch::Sender<bool>> = {
            let mut registry = self.registry();
            registry.closing = true;
            registry.conns.values().map(|c| c.close.clone()).collect()
        };
        for close in closes {
            let _ = close.send(true);
        }
        let mut waited = std::time::Duration::from_secs(1);
        while !self.wait_closed(waited).await {
            // One that did not close (it writes to a browser that does not
            // read, say): its task is aborted and awaited, which aborts its
            // writer and its forwarder; then their leases go.
            let tasks: Vec<JoinHandle<()>> = self
                .registry()
                .conns
                .values_mut()
                .filter_map(|c| c.task.take())
                .collect();
            for task in &tasks {
                task.abort();
            }
            for task in tasks {
                let _ = task.await;
            }
            if waited >= std::time::Duration::from_secs(5) {
                tracing::warn!("web connections are still ending; waiting for them");
            }
            waited = std::time::Duration::from_secs(5);
        }
        self.step(2).await;
        // 3. Only then the listeners, both.
        server.v4.abort();
        let _ = server.v4.await;
        if let Some(v6) = server.v6 {
            v6.abort();
            let _ = v6.await;
        }
        {
            let mut registry = self.registry();
            registry.serving = None;
            registry.stopping = false;
            registry.closing = false;
        }
        drop(lifecycle);
        self.step(3).await;
        self.news(News::Stopped(why));
        true
    }

    /// Wait until no connection is open, at most `limit`.
    async fn wait_closed(&self, limit: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let gone = self.gone.notified();
            if self.registry().conns.is_empty() {
                return true;
            }
            if tokio::time::timeout_at(deadline, gone).await.is_err() {
                return self.registry().conns.is_empty();
            }
        }
    }

    #[cfg(test)]
    async fn step(&self, n: u8) {
        let hook = self
            .stop_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if hook.send((n, tx)).is_ok() {
                let _ = rx.await;
            }
        }
    }

    #[cfg(not(test))]
    async fn step(&self, _n: u8) {}

    /// The housekeeping tick: a server on `127.0.0.1` alone tries `[::1]`
    /// again; if someone else holds it, the server stops itself.
    pub(crate) async fn tick(self: &Arc<Self>) {
        let taken = {
            let mut lifecycle = self.lifecycle.lock().await;
            let Some(server) = lifecycle.as_mut() else {
                return;
            };
            if server.v6.is_some() {
                return;
            }
            let port = server.serving.port;
            match self.bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
                Ok(v6) => {
                    match spawn_accept(self, v6) {
                        Ok(task) => server.v6 = Some(task),
                        Err(e) => tracing::warn!("serving [::1]:{port}: {e}"),
                    }
                    None
                }
                Err(e) if in_use(&e) => Some(port),
                Err(_) => None,
            }
        };
        if let Some(port) = taken {
            self.stop(Some(format!("[::1]:{port} was taken by another process")))
                .await;
        }
    }
}

/// Accept connections on `listener` until aborted.
fn spawn_accept(web: &Arc<Web>, listener: std::net::TcpListener) -> io::Result<JoinHandle<()>> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let web = web.clone();
    Ok(tokio::spawn(async move {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::debug!("web accept: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let Some(host) = web.host.get().cloned() else {
                continue;
            };
            let Some(admitted) = web.admit() else {
                continue;
            };
            let id = admitted.lease.id;
            let task = tokio::spawn(host.serve(stream, admitted));
            web.track(id, task);
        }
    }))
}

#[cfg(test)]
mod tests;
