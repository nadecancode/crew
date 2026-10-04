//! `crew web-server`: the dashboard's own process, one per user
//! (notes/web-multi.md §multi-server, M1).
//!
//! Its files are in the runtime directory beside the daemons' sockets:
//! `web.lock` (held for its life), `web.token` (written before) `web.sock`
//! (the control socket, both mode 0600) and `web.json` (what it is, for
//! `crew web`). It serves the listeners, pairing and sessions of
//! `crate::web`, the workspaces of `hub`, a page's connections (`page`) and
//! the control socket (`control`).
//!
//! It stops when asked (`web_close` without a workspace, SIGTERM, SIGINT),
//! when no joined workspace ran for the linger time, when it loses a
//! listener, or when its control socket's path no longer names its socket.
//! Stopping is one order: every membership, code and session revoked; every
//! connection let go of its daemon and closed; the listeners released; the
//! watches closed; its files removed.

use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crew_core::paths::{self, WebServerInfo};
use crew_core::rpc::{RpcError, WebInfo, PROTOCOL_VERSION};
use crew_core::types::WebEventKind;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::daemons::{Daemons, Registered};
use super::hub::{Hub, Join};
use super::{Admitted, Host, News, Web, CODE_TTL_MS, MAX_PREFS_BYTES};

/// The housekeeping tick.
const TICK: Duration = Duration::from_secs(5);
/// A server nobody asked anything of stops after this.
const UNASKED_FOR: Duration = Duration::from_secs(60);
/// No joined workspace has been running for this long: the server stops.
pub(crate) const LINGER: Duration = Duration::from_secs(600);

/// `[web]` of the user's and the global `config.toml` (the project's is not
/// read: a workspace does not opt itself in).
#[derive(Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// `"all"`: every running session is joined at each rescan.
    #[serde(default)]
    pub join: Option<String>,
    #[serde(default)]
    pub linger_secs: Option<u64>,
}

impl Settings {
    pub(crate) fn load() -> Self {
        let mut settings = Settings::default();
        for scope in [
            crate::endpoints::Scope::Global,
            crate::endpoints::Scope::User,
        ] {
            let Some(path) = scope.path(Path::new("/")) else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = toml::from_str::<toml::Value>(&text) else {
                continue;
            };
            let web = file
                .get("web")
                .cloned()
                .and_then(|v| v.try_into::<Settings>().ok());
            if let Some(web) = web {
                if web.join.is_some() {
                    settings.join = web.join;
                }
                if web.linger_secs.is_some() {
                    settings.linger_secs = web.linger_secs;
                }
            }
        }
        settings
    }

    pub(crate) fn join_all(&self) -> bool {
        self.join.as_deref() == Some("all")
    }

    pub(crate) fn linger(&self) -> Duration {
        self.linger_secs.map(Duration::from_secs).unwrap_or(LINGER)
    }
}

/// Test hooks of a page's connections.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct PageHooks {
    /// Held before a workspace connection's commit.
    pub commit_hold: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    /// Awaited by a connection told of its revocation, before it lets go.
    pub let_go: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// Dropped when a connection held by `let_go` was dropped.
    pub held: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[cfg(test)]
impl PageHooks {
    pub(crate) async fn before_commit(&self) {
        let hold = self.commit_hold.lock().unwrap().clone();
        if let Some(hold) = hold {
            if let Ok(permit) = hold.acquire().await {
                permit.forget();
            }
        }
    }

    pub(crate) async fn letting_go(&self) {
        let wait = self.let_go.lock().unwrap().clone();
        if let Some(wait) = wait {
            // The guard records its own drop, whenever the future is dropped.
            let _held = self.held.lock().unwrap().take();
            wait.notified().await;
        }
    }
}

/// The dashboard server's state.
pub(crate) struct Server {
    pub(crate) web: Arc<Web>,
    pub(crate) hub: Arc<Hub>,
    /// The operator's preferences (`web-prefs.json`).
    prefs: PathBuf,
    /// Asked to stop, and why.
    stop: watch::Sender<Option<String>>,
    /// A `web_open` was answered.
    opened: AtomicBool,
    /// Stopping is done once.
    stopping: tokio::sync::Mutex<bool>,
    files: Mutex<Option<Files>>,
    #[cfg(test)]
    pub(crate) hooks: PageHooks,
}

/// The files the running server owns.
struct Files {
    socket: PathBuf,
    token: PathBuf,
    info: PathBuf,
    /// The control socket as bound: (device, inode).
    bound: (u64, u64),
}

/// The server as the host of its listeners.
struct ServerHost(Weak<Server>);

impl Host for ServerHost {
    fn first_port(&self) -> u16 {
        paths::web_port(&paths::runtime_dir())
    }

    fn news(&self, news: News) {
        let Some(server) = self.0.upgrade() else {
            return;
        };
        match news {
            News::Started(url) => tracing::info!("serving the dashboard at {url}"),
            News::Stopped(Some(why)) => tracing::warn!("stopped serving: {why}"),
            News::Stopped(None) => tracing::info!("stopped serving"),
            News::Paired(n) => {
                tracing::info!("a browser was paired ({n} sessions)");
                server.hub.web_event(WebEventKind::Paired, n as u32);
            }
            News::AuthFailed(n) => {
                tracing::warn!("{n} refused web logins in the last minute");
                server.hub.web_event(WebEventKind::AuthFailed, n);
            }
        }
    }

    fn serve(
        &self,
        stream: tokio::net::TcpStream,
        admitted: Admitted,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        match self.0.upgrade() {
            Some(server) => Box::pin(super::page::serve(server, stream, admitted)),
            None => Box::pin(async {}),
        }
    }
}

impl Server {
    pub(crate) fn new(daemons: Arc<dyn Daemons>, settings: &Settings, prefs: PathBuf) -> Arc<Self> {
        let server = Arc::new(Self {
            web: Arc::new(Web::default()),
            hub: Hub::new(daemons, settings.join_all()),
            prefs,
            stop: watch::channel(None).0,
            opened: AtomicBool::new(false),
            stopping: tokio::sync::Mutex::new(false),
            files: Mutex::new(None),
            #[cfg(test)]
            hooks: PageHooks::default(),
        });
        server
            .web
            .set_host(Arc::new(ServerHost(Arc::downgrade(&server))));
        server
    }

    /// Ask the server to stop.
    pub(crate) fn request_stop(&self, why: impl Into<String>) {
        self.stop.send_replace(Some(why.into()));
    }

    pub(crate) fn stop_requested(&self) -> watch::Receiver<Option<String>> {
        self.stop.subscribe()
    }

    /// `web_open` on the control socket.
    pub(crate) async fn open(
        &self,
        port: Option<u16>,
        workspace: Option<String>,
        all: bool,
        restore: Option<Vec<String>>,
    ) -> Result<WebInfo, RpcError> {
        let serving = self
            .web
            .serving()
            .ok_or_else(|| RpcError::unavailable("the dashboard server is stopping"))?;
        if port.is_some_and(|p| p != serving.port) {
            return Err(RpcError::conflict(format!(
                "the dashboard is already served on port {}; `crew web --stop` first to use another",
                serving.port
            )));
        }
        let mut joined = Vec::new();
        let mut first = None;
        if let Some(dir) = workspace {
            let dir = PathBuf::from(dir);
            first = Some(paths::session_id(&dir));
            joined.extend(self.hub.join(Join::One(dir)).await?);
        }
        if all {
            joined.extend(self.hub.join(Join::All).await?);
        }
        if let Some(dirs) = restore {
            let dirs = dirs.into_iter().map(PathBuf::from).collect();
            joined.extend(self.hub.join(Join::Restore(dirs)).await?);
        }
        self.hub.rescan();
        let started = !self.opened.swap(true, Ordering::SeqCst);
        let code = self.web.code();
        let url = serving.url();
        let pair_url = match &first {
            Some(id) => format!("{url}#pair={code}&w={id}"),
            None => format!("{url}#pair={code}"),
        };
        Ok(WebInfo {
            url: url.clone(),
            port: serving.port,
            pair_url,
            expires_in_secs: (CODE_TTL_MS / 1000) as u32,
            started,
            workspace_url: first.as_ref().map(|id| format!("{url}#/w/{id}/")),
            workspace: first,
            joined,
        })
    }

    pub(crate) fn opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }

    /// The operator's preferences; with `set`, replaced first.
    pub(crate) fn prefs(&self, set: Option<Value>) -> Result<Value, RpcError> {
        if let Some(set) = set {
            if !set.is_object() {
                return Err(RpcError::invalid("web_prefs takes an object"));
            }
            let text = serde_json::to_vec(&set).map_err(|e| RpcError::internal(e.to_string()))?;
            if text.len() > MAX_PREFS_BYTES {
                return Err(RpcError::invalid(format!(
                    "the dashboard's preferences are at most {MAX_PREFS_BYTES} bytes"
                )));
            }
            if let Some(dir) = self.prefs.parent() {
                paths::ensure_private_dir(dir).map_err(|e| RpcError::internal(format!("{e:#}")))?;
            }
            paths::write_private_file(&self.prefs, &text)
                .map_err(|e| RpcError::internal(format!("{e:#}")))?;
            return Ok(set);
        }
        Ok(std::fs::read(&self.prefs)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Map::new())))
    }

    /// The stop order. Once; the files go last.
    pub(crate) async fn shut(&self, why: Option<String>) {
        let mut stopping = self.stopping.lock().await;
        if *stopping {
            return;
        }
        *stopping = true;
        // 1. Memberships (the web's codes and sessions go first in its stop).
        self.hub.close();
        // 2-3. Codes and sessions, every connection, the listeners.
        self.web.stop(why.clone()).await;
        // 4. The watches.
        self.hub.close_watches().await;
        // 5. The files.
        if let Some(files) = self.files.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // Only ours: a socket bound by another server since is left alone.
            if same_file(&files.socket, files.bound) {
                let _ = std::fs::remove_file(&files.socket);
            }
            let _ = std::fs::remove_file(&files.token);
            let _ = std::fs::remove_file(&files.info);
        }
        self.request_stop(why.unwrap_or_else(|| "asked to".into()));
    }

    /// The control socket's path still names the socket it bound.
    fn socket_is_ours(&self) -> bool {
        match &*self.files.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(files) => same_file(&files.socket, files.bound),
            None => true,
        }
    }
}

fn same_file(path: &Path, (dev, ino): (u64, u64)) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.dev() == dev && m.ino() == ino)
}

/// The single-instance lock, held for the process's life: `None` when
/// another server holds it.
fn take_lock(path: &Path) -> Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: flock on a descriptor this function owns.
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(taken.then_some(file))
}

/// What the embedded bundle is: its manifest's hash, or `builtin`.
pub(crate) fn build() -> String {
    let bundle = super::assets::find("manifest.json")
        .map(|asset| format!("{:016x}", crew_core::util::fnv1a64(asset.bytes)))
        .unwrap_or_else(|| "builtin".into());
    format!("{}+{bundle}", crate::daemon::VERSION)
}

/// The preferences file, beside the registry.
fn prefs_path() -> PathBuf {
    paths::registry_dir()
        .parent()
        .map(|dir| dir.join("web-prefs.json"))
        .unwrap_or_else(|| PathBuf::from("web-prefs.json"))
}

/// `crew web-server [--port N]`, in the foreground.
pub fn run(port: Option<u16>) -> Result<()> {
    crew_core::client::refuse_in_agent_context()?;
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .try_init();
    let runtime_dir = paths::runtime_dir();
    paths::ensure_private_dir(&runtime_dir)?;
    let Some(lock) = take_lock(&paths::web_lock())? else {
        bail!(
            "a dashboard server is already running ({}): run `crew web` to use it",
            paths::web_info_path().display()
        );
    };
    // Whatever a server before this one left is stale: it held the lock.
    let socket = paths::web_socket();
    let token_path = paths::operator_token_path(&socket);
    for stale in [&socket, &token_path, &paths::web_info_path()] {
        let _ = std::fs::remove_file(stale);
    }
    let settings = Settings::load();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        // The registry is read on a thread of the blocking pool, one read at
        // a time (`Registered::sessions`); that thread then waits for the
        // next for ever instead of waking 10 s later to exit, a wake-up of
        // an idle server outside its tick (notes/web-multi.md P20).
        .thread_keep_alive(Duration::MAX)
        .build()
        .context("starting the async runtime")?;
    let result = runtime.block_on(serve(port, settings, socket, token_path));
    drop(lock);
    result
}

async fn serve(
    port: Option<u16>,
    settings: Settings,
    socket: PathBuf,
    token_path: PathBuf,
) -> Result<()> {
    let server = Server::new(Arc::new(Registered), &settings, prefs_path());
    let (serving, _) = server
        .web
        .listen(port)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.message))?;
    let reconciler = server.hub.start(serving.url());
    // The token before the socket: whoever finds the socket finds its token.
    let token = crew_core::util::new_token();
    paths::write_private_file(&token_path, token.as_bytes())?;
    let listener = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(&socket)?;
    let info = WebServerInfo {
        pid: std::process::id(),
        pid_start: paths::process_start(std::process::id()),
        boot_id: paths::boot_id(),
        port: serving.port,
        url: serving.url(),
        protocol: PROTOCOL_VERSION,
        version: crate::daemon::VERSION.to_string(),
        build: build(),
    };
    paths::write_private_file(&paths::web_info_path(), &serde_json::to_vec_pretty(&info)?)?;
    *server.files.lock().unwrap_or_else(|e| e.into_inner()) = Some(Files {
        socket: socket.clone(),
        token: token_path,
        info: paths::web_info_path(),
        bound: (meta.dev(), meta.ino()),
    });
    let control = tokio::spawn(super::control::accept(server.clone(), listener, token));
    if settings.join_all() {
        server.hub.rescan();
    }
    let why = lifetime(&server, settings.linger()).await;
    server.shut(why).await;
    control.abort();
    reconciler.abort();
    Ok(())
}

/// Until the server is to stop: why, when it stopped by itself.
async fn lifetime(server: &Arc<Server>, linger: Duration) -> Option<String> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).ok();
    let mut sigint = signal(SignalKind::interrupt()).ok();
    let mut stop = server.stop_requested();
    let mut running = server.hub.running();
    let started = tokio::time::Instant::now();
    let mut idle_since: Option<tokio::time::Instant> = None;
    let mut tick = tokio::time::interval_at(started + TICK, TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // Linger counts once something was asked for and nothing runs.
        if server.opened() && *running.borrow() == 0 {
            idle_since.get_or_insert_with(tokio::time::Instant::now);
        } else {
            idle_since = None;
        }
        let deadline = match idle_since {
            Some(since) => since + linger,
            None if !server.opened() => started + UNASKED_FOR,
            None => started + Duration::from_secs(u32::MAX as u64),
        };
        tokio::select! {
            _ = stop.changed() => {
                let why = stop.borrow().clone();
                return why.filter(|w| w != "asked to");
            }
            _ = running.changed() => {}
            _ = async { sigterm.as_mut()?.recv().await }, if sigterm.is_some() => return None,
            _ = async { sigint.as_mut()?.recv().await }, if sigint.is_some() => return None,
            _ = tokio::time::sleep_until(deadline) => {
                return Some(match idle_since {
                    Some(_) => format!("no workspace in it ran for {} s", linger.as_secs()),
                    None => "nobody asked it for anything".into(),
                });
            }
            _ = tick.tick() => {
                server.web.tick().await;
                if server.web.serving().is_none() {
                    return Some("it lost its listener".into());
                }
                if !server.socket_is_ours() {
                    return Some("its control socket was removed".into());
                }
            }
        }
    }
}
