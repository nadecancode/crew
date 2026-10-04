//! `crew web`: the browser dashboard (notes/web-multi.md §multi-migration).
//! One server per user (`crew web-server`, started here when none runs)
//! shows every workspace it was asked to show; this command joins, takes
//! out, lists, replaces and stops, on the server's control socket.

use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crew_core::client::Client;
use crew_core::paths::{self, WebServerInfo};
use crew_core::rpc::{Call, WebInfo, Workspaces, PROTOCOL_VERSION};
use crew_core::types::WorkspaceState;
use serde_json::Value;

use super::Target;

/// What `crew web` joins.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WebJoin {
    /// The workspace it was run in.
    This,
    /// Every running session.
    All,
}

/// How long a server just started has to answer.
const STARTS_WITHIN: Duration = Duration::from_secs(5);

/// The running server's own description, if one runs.
fn live_server() -> Option<WebServerInfo> {
    paths::read_web_info().filter(WebServerInfo::is_live)
}

/// A server of another protocol is never replaced unasked.
fn refuse_another_protocol(info: &WebServerInfo) -> Result<()> {
    if info.protocol != PROTOCOL_VERSION {
        bail!(
            "The dashboard is served by another version of crew (protocol {}, this one speaks \
{PROTOCOL_VERSION}). Teams started by that version keep their dashboard until they are \
restarted. `crew web --restart` replaces the server: open tabs stop, and teams of the other \
version are shown as incompatible until they are restarted.",
            info.protocol
        );
    }
    Ok(())
}

/// The control socket of the running server, signed in.
fn control() -> Result<Option<Client>> {
    let Some(info) = live_server() else {
        return Ok(None);
    };
    refuse_another_protocol(&info)?;
    let socket = paths::web_socket();
    match Client::operator(&socket, "cli") {
        Ok((client, _)) => Ok(Some(client)),
        Err(e) if !socket.exists() => {
            tracing::debug!("the dashboard server's socket: {e}");
            Ok(None)
        }
        Err(e) => Err(e).context("speaking to the dashboard server"),
    }
}

/// The running server, or one started now (`true`).
fn ensure_server(port: Option<u16>) -> Result<(Client, bool)> {
    if let Some(client) = control()? {
        return Ok((client, false));
    }
    paths::ensure_private_dir(&paths::runtime_dir())?;
    let log_dir = paths::registry_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(paths::runtime_dir);
    paths::ensure_private_dir(&log_dir)?;
    let log_path = log_dir.join("web.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let mut cmd = Command::new(std::env::current_exe().context("locating the crew binary")?);
    cmd.arg("web-server")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(port) = port {
        cmd.arg("--port").arg(port.to_string());
    }
    for marker in crate::cli::daemonize::SESSION_MARKERS {
        cmd.env_remove(marker);
    }
    // A session of its own: it outlives the terminal it was started from.
    // SAFETY: setsid is async-signal-safe and runs in the child before exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting the dashboard server")?;
    let deadline = Instant::now() + STARTS_WITHIN;
    loop {
        if let Some(client) = control()? {
            // Its exit is the server's own business from here on.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok((client, true));
        }
        // It lost the lock to one started at the same moment: that one is
        // waited for. Anything else, it says why in its log.
        if child.try_wait()?.is_some() && live_server().is_none() && Instant::now() > deadline {
            bail!(
                "the dashboard server did not start; see {}",
                log_path.display()
            );
        }
        if Instant::now() > deadline {
            bail!(
                "the dashboard server did not answer within {} s; see {}",
                STARTS_WITHIN.as_secs(),
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Stop the running server and wait for it to go: the workspaces it showed
/// (state directories), when it speaks this protocol.
fn replace_server() -> Result<Option<Vec<String>>> {
    let Some(old) = live_server() else {
        return Ok(None);
    };
    let restore = if old.protocol == PROTOCOL_VERSION {
        let (mut client, _) = Client::operator(&paths::web_socket(), "cli")
            .context("speaking to the dashboard server")?;
        let listing: Workspaces = client.call(Call::Workspaces)?;
        let _: Value = client.call(Call::WebClose { workspace: None })?;
        Some(
            listing
                .joined
                .iter()
                .map(|w| {
                    paths::state_dir(Path::new(&w.workspace))
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
        )
    } else {
        // SAFETY: a signal to the process `web.json` names, which is_live
        // checked by its start time and boot.
        unsafe { libc::kill(old.pid as libc::pid_t, libc::SIGTERM) };
        // Not on stdout: `--json` prints only the address there.
        eprintln!(
            "note       the server it replaces spoke protocol {}: its workspaces are shown again with \
`crew web` in each, or `crew web --all`",
            old.protocol
        );
        None
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while live_server().is_some_and(|info| info.pid == old.pid) {
        if Instant::now() > deadline {
            bail!("the dashboard server (pid {}) did not stop", old.pid);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(restore)
}

/// Join a workspace (or all of them), print the address and a one-time
/// pairing address, and open that in the browser.
pub fn web(
    target: &Target,
    how: WebJoin,
    port: Option<u16>,
    no_open: bool,
    json: bool,
    restart: bool,
) -> Result<()> {
    // Before anything else: an agent never gets a pairing code.
    crew_core::client::refuse_in_agent_context()?;
    // A restart carries the memberships of a server of this protocol over,
    // stopped ones included: then the daemon it was run in need not run.
    let carried = restart && live_server().is_some_and(|old| old.protocol == PROTOCOL_VERSION);
    let state_dir = match how {
        // The daemon it was run for must run, and speak this protocol.
        WebJoin::This => match target.connect("cli") {
            Ok((_, info)) => Some(info.state_dir),
            Err(_) if carried => None,
            Err(e) => return Err(e),
        },
        WebJoin::All => {
            if !carried && paths::list_sessions().is_empty() {
                bail!(
                    "no crew session is running (start one with `crew run`); nothing was started"
                );
            }
            None
        }
    };
    let restore = if restart { replace_server()? } else { None };
    let (mut client, started) = ensure_server(port)?;
    let mut info: WebInfo = client.call(Call::WebOpen {
        port,
        workspace: state_dir,
        all: how == WebJoin::All,
        restore,
    })?;
    info.started = started;
    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    println!(
        "web        {}",
        info.workspace_url.as_deref().unwrap_or(&info.url)
    );
    println!(
        "open       {}   (works once, for {} minutes)",
        info.pair_url,
        info.expires_in_secs / 60
    );
    if how == WebJoin::All {
        println!("joined     {} workspace(s)", info.joined.len());
    }
    if let Some(server) = live_server().filter(|s| s.build != crate::web::process::build()) {
        println!(
            "note       the dashboard server is another build of crew ({}); `crew web --restart` \
replaces it with this one",
            server.build
        );
    }
    println!(
        "note       opens in Chromium-based browsers and Firefox; Safari cannot resolve this \
address. Open it yourself, and paste it nowhere else"
    );
    if !no_open && std::io::stdout().is_terminal() {
        open_browser(&info.pair_url);
    }
    Ok(())
}

/// The state directory `crew web --remove` names: its daemon's, or the one
/// of the directory given (its daemon may have stopped).
fn state_dir_of(target: &Target) -> Result<PathBuf> {
    if let Some(dir) = &target.dir {
        let dir = dir
            .canonicalize()
            .with_context(|| format!("resolving {}", dir.display()))?;
        return Ok(paths::state_dir(&dir));
    }
    // A project named is that project's, or an error: never the directory
    // this was run in.
    if let Some(project) = target.project.as_deref() {
        let info = paths::discover(None, Some(project))?;
        return Ok(PathBuf::from(info.state_dir));
    }
    if let Ok(info) = paths::discover(None, None) {
        if !info.state_dir.is_empty() {
            return Ok(PathBuf::from(info.state_dir));
        }
    }
    Ok(paths::state_dir(&std::env::current_dir()?))
}

/// `crew web --remove`: take this workspace out of the dashboard.
pub fn web_remove(target: &Target) -> Result<()> {
    let state_dir = state_dir_of(target)?;
    let Some(mut client) = control()? else {
        bail!("the dashboard server is not running");
    };
    let _: Value = client.call(Call::WebClose {
        workspace: Some(state_dir.to_string_lossy().into_owned()),
    })?;
    println!(
        "took {} out of the dashboard: its browser connections have ended",
        state_dir.parent().unwrap_or(&state_dir).display()
    );
    Ok(())
}

/// `crew web --list`: what the dashboard shows, and what runs beside it.
pub fn web_list(json: bool) -> Result<()> {
    let Some(mut client) = control()? else {
        if json {
            println!("null");
        } else {
            println!("the dashboard server is not running");
        }
        return Ok(());
    };
    let listing: Workspaces = client.call(Call::Workspaces)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(());
    }
    if let Some(server) = live_server() {
        println!("web        {}", server.url);
    }
    for w in &listing.joined {
        let state = match w.state {
            WorkspaceState::Running => "running".to_string(),
            WorkspaceState::Stopped => "stopped".to_string(),
            other => format!("{other}: {}", w.detail),
        };
        println!(
            "{:<16} {:<12} {}   {}",
            w.id,
            w.project.as_deref().unwrap_or("-"),
            w.workspace,
            state
        );
    }
    if !listing.others.is_empty() {
        println!("running, not in this dashboard:");
        for o in &listing.others {
            println!(
                "  {:<12} {}   crew web -C {}",
                o.project.as_deref().unwrap_or("-"),
                o.workspace,
                o.workspace
            );
        }
    }
    Ok(())
}

/// `crew web --stop`: stop the server; every browser session ends.
pub fn web_stop() -> Result<()> {
    let Some(mut client) = control()? else {
        println!("the dashboard server was not running");
        return Ok(());
    };
    let pid = live_server().map(|s| s.pid);
    let reply: Value = client.call(Call::WebClose { workspace: None })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid.is_some() && live_server().map(|s| s.pid) == pid && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "stopped the web dashboard: every browser session has ended ({} workspace(s) were shown)",
        reply["workspaces"].as_u64().unwrap_or(0)
    );
    Ok(())
}

/// Hand `url` to the desktop's opener; failing to is not an error, the
/// address was printed.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let spawned = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}
