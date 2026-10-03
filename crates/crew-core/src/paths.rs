//! Where crew keeps things: the per-workspace state directory, the daemon
//! socket, the registry of running daemons and per-agent identity files.

use std::ffi::OsStr;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::fnv1a64;

pub const STATE_DIR_NAME: &str = ".crew";
/// Environment variable pointing an agent's tooling at its identity file.
pub const IDENTITY_ENV: &str = "CREW_IDENTITY";
/// Environment variable overriding daemon discovery with an explicit socket.
pub const SOCKET_ENV: &str = "CREW_SOCKET";

/// Written by a running daemon to `<state>/daemon.json` and the registry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DaemonInfo {
    #[serde(default)]
    pub project: Option<String>,
    pub workspace: String,
    pub state_dir: String,
    pub socket: String,
    pub pid: u32,
    pub started_at: i64,
}

/// Credentials of one agent, stored `0600` in its state directory. Everything
/// that speaks for an agent (MCP bridge, `crew ctl`, hooks) reads this file,
/// so the token never appears in a command line.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Identity {
    pub socket: String,
    pub agent_id: String,
    pub token: String,
}

impl Identity {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading identity file {}", path.display()))?;
        serde_json::from_str(&text)
            .with_context(|| format!("parsing identity file {}", path.display()))
    }

    /// The identity named by `CREW_IDENTITY`, if this process runs inside an agent.
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os(IDENTITY_ENV)?;
        Self::load(Path::new(&path)).ok()
    }
}

pub fn state_dir(workspace: &Path) -> PathBuf {
    workspace.join(STATE_DIR_NAME)
}

pub fn uid() -> u32 {
    // SAFETY: getuid has no failure modes and touches no memory.
    unsafe { libc::getuid() }
}

/// Create a directory (and parents) readable by the current user only.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("private directory path is empty");
    }
    // Trusted ancestors may be symlinks. A component under a writable
    // parent (notably /tmp), and always the leaf, must be ours and not a link.
    #[cfg(target_os = "linux")]
    let search_flags = libc::O_PATH;
    // Other Unix platforms need read permission on intermediate directories.
    #[cfg(not(target_os = "linux"))]
    let search_flags = libc::O_RDONLY;
    let mut walked = PathBuf::from(if path.is_absolute() { "/" } else { "." });
    let mut directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(search_flags | libc::O_DIRECTORY)
        .open(&walked)?;
    let components: Vec<_> = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .collect();
    for (index, component) in components.iter().enumerate() {
        let metadata = directory.metadata()?;
        let leaf = index + 1 == components.len();
        let strict = leaf || metadata.permissions().mode() & 0o022 != 0;
        walked.push(component.as_os_str());
        let name = std::ffi::CString::new(component.as_os_str().as_bytes())?;
        let open = || {
            // SAFETY: directory is an open descriptor and name is NUL-terminated.
            unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    (if leaf { libc::O_RDONLY } else { search_flags })
                        | libc::O_DIRECTORY
                        | libc::O_CLOEXEC
                        | if strict { libc::O_NOFOLLOW } else { 0 },
                )
            }
        };
        let mut fd = open();
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            // SAFETY: the parent descriptor and name are valid; mode is private.
            let created = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            if created < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error).with_context(|| format!("creating {}", walked.display()));
                }
            }
            fd = open();
        }
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if strict && matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                bail!(
                    "{} is a symlink or not a directory; refusing to use it",
                    walked.display()
                );
            }
            return Err(error)
                .with_context(|| format!("opening private directory {}", walked.display()));
        }
        // SAFETY: openat returned a new owned descriptor.
        directory = unsafe { fs::File::from_raw_fd(fd) };
        if strict && directory.metadata()?.uid() != uid() {
            bail!("{} is owned by another user", walked.display());
        }
    }
    if directory.metadata()?.uid() != uid() {
        bail!("{} is owned by another user", path.display());
    }
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting permissions of {}", path.display()))?;
    Ok(())
}

/// Write a file readable by the current user only, replacing it atomically.
pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(target_os = "linux")]
    let access = libc::O_PATH;
    #[cfg(not(target_os = "linux"))]
    let access = libc::O_RDONLY;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(access | libc::O_DIRECTORY)
        .open(parent)
        .with_context(|| format!("opening {}", parent.display()))?;
    let name = std::ffi::CString::new(
        path.file_name()
            .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
            .as_bytes(),
    )?;
    let temporary = std::ffi::CString::new(format!(".crew-{}.tmp", uuid::Uuid::new_v4().simple()))?;
    // SAFETY: names are NUL-terminated and directory is a live descriptor.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("creating private temporary file for {}", path.display()));
    }
    // SAFETY: openat returned an owned descriptor for a newly created regular file.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let result = (|| -> Result<()> {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents)?;
        file.sync_all()?;
        // SAFETY: both names resolve relative to our pinned parent descriptor.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                name.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("replacing {}", path.display()));
        }
        Ok(())
    })();
    if result.is_err() {
        // SAFETY: remove only this call's random temporary entry in its pinned parent.
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
}

/// Read a regular file beneath a trusted pre-opened directory, with no links.
/// Size limits belong to the reader. No absolute, dot or dotdot paths are allowed.
pub fn open_beneath(root: impl AsFd, relative: &Path) -> Result<fs::File> {
    let flags =
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = resolve_beneath(root.as_fd(), relative, flags)?;
    regular_file(fd, relative)
}

/// Acquire a descendant directory from the same pinned root used for reads.
/// On Linux this is an O_PATH anchor for lookup and metadata, not an fd for
/// listing or fchmod. Use list_dir_beneath when a readable directory is needed.
pub fn open_dir_beneath(root: impl AsFd, relative: &Path) -> Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let access = libc::O_PATH;
    #[cfg(not(target_os = "linux"))]
    let access = libc::O_RDONLY;
    resolve_beneath(
        root.as_fd(),
        relative,
        access | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )
}

/// Open a readable descendant directory without following any symlink.
/// Use descriptor-relative lookup or enumeration; do not reconstruct its path.
pub fn list_dir_beneath(root: impl AsFd, relative: &Path) -> Result<fs::File> {
    let fd = resolve_beneath(
        root.as_fd(),
        relative,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
    )?;
    Ok(fs::File::from(fd))
}

fn relative_name(relative: &Path) -> std::io::Result<std::ffi::CString> {
    let bytes = relative.as_os_str().as_bytes();
    if bytes.is_empty()
        || relative.is_absolute()
        || bytes
            .split(|b| *b == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a nonempty relative path without dot or dotdot components",
        ));
    }
    std::ffi::CString::new(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn resolve_beneath(root: BorrowedFd<'_>, relative: &Path, flags: libc::c_int) -> Result<OwnedFd> {
    let name = relative_name(relative)?;
    #[cfg(target_os = "linux")]
    let native = {
        // SAFETY: open_how is made only of integer fields; all arguments live through the syscall.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = flags as u64;
        how.resolve =
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                root.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<libc::open_how>(),
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: the successful syscall returned a new owned descriptor.
            Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
        }
    };
    #[cfg(not(target_os = "linux"))]
    let native = {
        let _ = name;
        Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
    };
    finish_beneath_open(native, root, relative, flags)
        .with_context(|| format!("opening {} beneath pinned root", relative.display()))
}

fn finish_beneath_open(
    native: std::io::Result<OwnedFd>,
    root: BorrowedFd<'_>,
    relative: &Path,
    flags: libc::c_int,
) -> std::io::Result<OwnedFd> {
    match native {
        Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {
            walk_beneath(root, relative, flags)
        }
        result => result,
    }
}

/// Kernels without openat2 and other Unix platforms use descriptor-relative
/// lookup with O_NOFOLLOW at every component, never an absolute path lookup.
fn walk_beneath(
    root: BorrowedFd<'_>,
    relative: &Path,
    flags: libc::c_int,
) -> std::io::Result<OwnedFd> {
    relative_name(relative)?;
    let mut directory = root.try_clone_to_owned()?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let name = std::ffi::CString::new(component.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL")
        })?;
        #[cfg(target_os = "linux")]
        let access = libc::O_PATH;
        #[cfg(not(target_os = "linux"))]
        let access = libc::O_RDONLY;
        let component_flags = if index + 1 == components.len() {
            flags
        } else {
            access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        // SAFETY: the pinned directory and NUL-terminated component name are valid.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), component_flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new owned descriptor.
        directory = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn regular_file(fd: OwnedFd, relative: &Path) -> Result<fs::File> {
    let file = fs::File::from(fd);
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", relative.display());
    }
    // SAFETY: file owns the fd; clearing NONBLOCK only after fstat avoids FIFO hangs.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error())
            .context("clearing nonblocking flag on regular file");
    }
    Ok(file)
}

/// Directory for sockets: `$XDG_RUNTIME_DIR/crew`, else `/tmp/crew-<uid>`.
pub fn runtime_dir() -> PathBuf {
    runtime_dir_from(std::env::var_os("XDG_RUNTIME_DIR").as_deref(), uid())
}

fn runtime_dir_from(xdg: Option<&OsStr>, user: u32) -> PathBuf {
    let preferred = match xdg {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("crew"),
        _ => PathBuf::from(format!("/tmp/crew-{user}")),
    };
    // SAFETY: sockaddr_un consists only of integer fields and a byte array.
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let probe = socket_path_in(Path::new(""), &preferred);
    if probe.as_os_str().as_bytes().len() < address.sun_path.len() {
        preferred
    } else {
        PathBuf::from(format!("/tmp/crew-{user}"))
    }
}

/// Socket path for a state directory. Sockets live in the runtime dir under a
/// hash of the state directory because `sun_path` is limited to ~108 bytes.
pub fn socket_path(state_dir: &Path) -> PathBuf {
    socket_path_in(state_dir, &runtime_dir())
}

/// The credential beside a daemon socket. Avoid a collision for socket names
/// that already end in .token (including explicit CREW_SOCKET overrides).
pub fn operator_token_path(socket: &Path) -> PathBuf {
    if socket.extension() == Some(OsStr::new("token")) {
        let mut name = socket.as_os_str().to_os_string();
        name.push(".token");
        PathBuf::from(name)
    } else {
        socket.with_extension("token")
    }
}

/// Read an existing private operator credential. A missing file returns None;
/// callers decide whether it is required. Every other error fails closed.
pub fn read_operator_token(socket: &Path) -> Result<Option<String>> {
    use std::io::Read;
    if socket.file_name().is_none() {
        return Ok(None);
    }
    let path = operator_token_path(socket);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(target_os = "linux")]
    let access = libc::O_PATH;
    #[cfg(not(target_os = "linux"))]
    let access = libc::O_RDONLY;
    let directory = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(access | libc::O_DIRECTORY)
        .open(parent)
    {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening operator token directory {}", parent.display()))
        }
    };
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("operator token path has no file name"))?;
    // One component below the pinned parent needs no openat2. This keeps
    // missing credentials optional even when a container blocks openat2.
    let flags =
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let fd = match walk_beneath(directory.as_fd(), Path::new(name), flags) {
        Ok(fd) => fd,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening operator token file"),
    };
    let file = regular_file(fd, Path::new(name))?;
    let parent_metadata = directory.metadata()?;
    if parent_metadata.uid() != uid() || parent_metadata.permissions().mode() & 0o022 != 0 {
        bail!(
            "operator token directory must be owned by the current user and not writable by others"
        );
    }
    let metadata = file.metadata()?;
    if metadata.uid() != uid() || metadata.permissions().mode() & 0o777 != 0o600 {
        bail!("operator token file must be owned by the current user with mode 0600");
    }
    if metadata.len() == 0 || metadata.len() > 1024 {
        bail!("operator token file must contain 1 to 1024 bytes");
    }
    let mut token = String::new();
    file.take(1025)
        .read_to_string(&mut token)
        .context("reading operator token file")?;
    let token = token.trim();
    if token.is_empty() || token.len() > 1024 {
        bail!("operator token file must contain 1 to 1024 bytes");
    }
    Ok(Some(token.to_owned()))
}

fn socket_path_in(state_dir: &Path, runtime: &Path) -> PathBuf {
    runtime.join(format!("{:016x}.sock", session_hash(state_dir)))
}

fn session_hash(state_dir: &Path) -> u64 {
    fnv1a64(state_dir.to_string_lossy().as_bytes())
}

/// Registry of running daemons, so `crew attach` works from any directory.
pub fn registry_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local").join("state"),
            None => PathBuf::from(format!("/tmp/crew-{}", uid())),
        },
    };
    base.join("crew").join("sessions")
}

fn registry_file(state_dir: &Path) -> PathBuf {
    registry_dir().join(format!("{:016x}.json", session_hash(state_dir)))
}

pub fn daemon_info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.json")
}

pub fn write_daemon_info(info: &DaemonInfo) -> Result<()> {
    let state_dir = Path::new(&info.state_dir);
    let json = serde_json::to_vec_pretty(info)?;
    write_private_file(&daemon_info_path(state_dir), &json)?;
    ensure_private_dir(&registry_dir())?;
    write_private_file(&registry_file(state_dir), &json)?;
    Ok(())
}

pub fn remove_daemon_info(state_dir: &Path) {
    let _ = fs::remove_file(daemon_info_path(state_dir));
    let _ = fs::remove_file(registry_file(state_dir));
}

pub fn read_daemon_info(state_dir: &Path) -> Option<DaemonInfo> {
    let text = fs::read_to_string(daemon_info_path(state_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence/permission check.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn is_live(info: &DaemonInfo) -> bool {
    pid_alive(info.pid) && Path::new(&info.socket).exists()
}

/// Every daemon currently running for this user; stale entries are pruned.
pub fn list_sessions() -> Vec<DaemonInfo> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(registry_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<DaemonInfo>(&text) {
            Ok(info) if is_live(&info) => out.push(info),
            _ => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    out.sort_by_key(|i| i.started_at);
    out
}

/// Locate the daemon to talk to.
///
/// Order: an explicit workspace directory, `CREW_SOCKET`, the agent identity
/// in `CREW_IDENTITY`, a `.crew/` found walking up from the current directory,
/// then the registry (which must be unambiguous, or narrowed by `project`).
pub fn discover(dir: Option<&Path>, project: Option<&str>) -> Result<DaemonInfo> {
    if let Some(dir) = dir {
        let dir = dir
            .canonicalize()
            .with_context(|| format!("resolving {}", dir.display()))?;
        return read_daemon_info(&state_dir(&dir))
            .filter(is_live)
            .ok_or_else(|| anyhow!("no crew daemon is running for {}", dir.display()));
    }
    if project.is_none() {
        if let Some(socket) = std::env::var_os(SOCKET_ENV) {
            let socket = PathBuf::from(socket);
            return Ok(DaemonInfo {
                project: None,
                workspace: String::new(),
                state_dir: String::new(),
                socket: socket.to_string_lossy().into_owned(),
                pid: 0,
                started_at: 0,
            });
        }
        if let Some(identity) = Identity::from_env() {
            return Ok(DaemonInfo {
                project: None,
                workspace: String::new(),
                state_dir: String::new(),
                socket: identity.socket,
                pid: 0,
                started_at: 0,
            });
        }
        if let Ok(cwd) = std::env::current_dir() {
            let mut cur: Option<&Path> = Some(cwd.as_path());
            while let Some(d) = cur {
                if let Some(info) = read_daemon_info(&state_dir(d)).filter(is_live) {
                    return Ok(info);
                }
                cur = d.parent();
            }
        }
    }
    let mut sessions = list_sessions();
    if let Some(project) = project {
        sessions.retain(|s| s.project.as_deref() == Some(project));
    }
    match sessions.len() {
        0 => match project {
            Some(p) => bail!("no running crew session for project '{p}'"),
            None => bail!("no crew daemon is running (start one with `crew run <team.yaml>`)"),
        },
        1 => Ok(sessions.remove(0)),
        _ => {
            let names: Vec<String> = sessions
                .iter()
                .map(|s| {
                    format!(
                        "{} ({})",
                        s.project.as_deref().unwrap_or("<no team>"),
                        s.workspace
                    )
                })
                .collect();
            bail!(
                "several crew sessions are running; pick one with --project or -C: {}",
                names.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_token_paths_are_distinct_and_credentials_are_private_bounded_files() {
        assert_eq!(
            operator_token_path(Path::new("/run/crew/hash.sock")),
            Path::new("/run/crew/hash.token")
        );
        assert_eq!(
            operator_token_path(Path::new("/run/crew/socket")),
            Path::new("/run/crew/socket.token")
        );
        assert_eq!(
            operator_token_path(Path::new("/run/crew/socket.token")),
            Path::new("/run/crew/socket.token.token")
        );
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let token = operator_token_path(&socket);
        assert_eq!(read_operator_token(&socket).unwrap(), None);
        write_private_file(&token, b"synthetic private credential\n").unwrap();
        assert_eq!(
            read_operator_token(&socket).unwrap().as_deref(),
            Some("synthetic private credential")
        );
        for bytes in [b"".as_slice(), b" \n ", &[0xff], &vec![b'x'; 1025]] {
            write_private_file(&token, bytes).unwrap();
            assert!(read_operator_token(&socket).is_err());
        }
        write_private_file(&token, b"must-not-appear-in-error").unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
        let error = read_operator_token(&socket).unwrap_err();
        assert!(!error.to_string().contains("must-not-appear-in-error"));
        fs::remove_file(&token).unwrap();
        fs::create_dir(&token).unwrap();
        assert!(read_operator_token(&socket).is_err());
        fs::remove_dir(&token).unwrap();
        let outside = temp.path().join("outside");
        write_private_file(&outside, b"outside sentinel").unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"outside sentinel");
        std::os::unix::fs::symlink(&outside, &token).unwrap();
        assert!(read_operator_token(&socket).is_err());
        fs::remove_file(&token).unwrap();
        std::os::unix::fs::symlink(temp.path().join("missing"), &token).unwrap();
        assert!(
            read_operator_token(&socket).is_err(),
            "a dangling link must not look like an absent credential"
        );
        fs::remove_file(&token).unwrap();
        write_private_file(&token, b"private but unsafe parent").unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let result = read_operator_token(&socket);
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn token_reader_seccomp_filter_does_not_escape_its_probe_thread() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("file"), b"readable").unwrap();
        let root = fs::File::open(temp.path()).unwrap();
        assert!(open_beneath(&root, Path::new("file")).is_ok());
        // Invoke the probe directly as libtest can do on a fallback thread.
        // Its irreversible syscall filter must not contaminate this caller.
        token_reader_stays_optional_when_openat2_is_denied();
        let file = open_beneath(&root, Path::new("file")).unwrap();
        assert!(file.metadata().unwrap().is_file());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn token_reader_stays_optional_when_openat2_is_denied() {
        std::thread::spawn(|| {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("daemon.sock");
            assert_eq!(read_operator_token(&socket).unwrap(), None);
            let mut instructions = [
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 1,
                    k: libc::SYS_openat2 as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ALLOW,
                },
            ];
            let filter = libc::sock_fprog {
                len: instructions.len() as u16,
                filter: instructions.as_mut_ptr(),
            };
            // SAFETY: this filter affects only this test thread, denying one syscall.
            assert_eq!(
                unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
                0
            );
            assert_eq!(
                unsafe {
                    libc::prctl(
                        libc::PR_SET_SECCOMP,
                        libc::SECCOMP_MODE_FILTER,
                        &filter,
                        0,
                        0,
                    )
                },
                0
            );
            let root = fs::File::open(temp.path()).unwrap();
            let denied = open_beneath(&root, Path::new("missing")).unwrap_err();
            assert_eq!(
                denied
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .raw_os_error(),
                Some(libc::EPERM)
            );
            assert_eq!(read_operator_token(&socket).unwrap(), None);
            write_private_file(&operator_token_path(&socket), b"synthetic").unwrap();
            assert_eq!(
                read_operator_token(&socket).unwrap().as_deref(),
                Some("synthetic")
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn open_beneath_refuses_symlinks_dotdot_and_special_files() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        fs::create_dir_all(root_path.join("reports")).unwrap();
        fs::write(root_path.join("reports/session"), b"inside").unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"sentinel").unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"sentinel");
        let root = fs::File::open(&root_path).unwrap();
        let mut contents = String::new();
        open_beneath(&root, Path::new("reports/session"))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "inside");
        let subdir = open_dir_beneath(&root, Path::new("reports")).unwrap();
        contents.clear();
        open_beneath(&subdir, Path::new("session"))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "inside");
        std::os::unix::fs::symlink(&outside, root_path.join("link")).unwrap();
        std::os::unix::fs::symlink(temp.path(), root_path.join("linked-directory")).unwrap();
        std::os::unix::fs::symlink("reports", root_path.join("internal-link")).unwrap();
        for relative in [
            "link",
            "internal-link/session",
            "linked-directory/outside",
            "../outside",
            "reports/../reports/session",
            "reports/./session",
            "./reports/session",
            "",
            "reports",
        ] {
            assert!(
                open_beneath(&root, Path::new(relative)).is_err(),
                "{relative}"
            );
        }
        assert!(open_beneath(&root, &root_path.join("reports/session")).is_err());
        assert!(open_dir_beneath(&root, Path::new("linked-directory")).is_err());
        let fifo = root_path.join("fifo");
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the name is valid and creates only this fixture's FIFO.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let positive = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        assert!(!positive.metadata().unwrap().is_file());
        let start = std::time::Instant::now();
        assert!(open_beneath(&root, Path::new("fifo")).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
        let socket = root_path.join("socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let _positive = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        assert!(open_beneath(&root, Path::new("socket")).is_err());
        fs::rename(&root_path, temp.path().join("moved-root")).unwrap();
        fs::create_dir_all(root_path.join("reports")).unwrap();
        fs::write(root_path.join("reports/session"), b"replacement").unwrap();
        contents.clear();
        open_beneath(&root, Path::new("reports/session"))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(
            contents, "inside",
            "the pinned root must survive pathname replacement"
        );
    }

    #[test]
    fn list_dir_beneath_is_readable_pinned_and_refuses_links() {
        use std::os::fd::IntoRawFd;
        fn entries(file: &fs::File) -> Vec<String> {
            let fd = file.try_clone().unwrap().into_raw_fd();
            // SAFETY: fdopendir takes ownership of the duplicate descriptor.
            let directory = unsafe { libc::fdopendir(fd) };
            if directory.is_null() {
                // SAFETY: fdopendir failed, so this descriptor is still ours.
                unsafe { libc::close(fd) };
                panic!(
                    "cannot enumerate directory: {}",
                    std::io::Error::last_os_error()
                );
            }
            let mut names = Vec::new();
            // SAFETY: directory stays live until closedir; each name is copied
            // before the next readdir invalidates the returned entry.
            unsafe {
                loop {
                    let entry = libc::readdir(directory);
                    if entry.is_null() {
                        break;
                    }
                    names.push(
                        std::ffi::CStr::from_ptr((*entry).d_name.as_ptr())
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    );
                }
                assert_eq!(libc::closedir(directory), 0);
            }
            names.sort();
            names
        }
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("root/reports/nested")).unwrap();
        fs::write(temp.path().join("root/reports/session"), b"inside").unwrap();
        let root_path = temp.path().join("root");
        let root = fs::File::open(&root_path).unwrap();
        let expected = vec![
            ".".to_owned(),
            "..".to_owned(),
            "nested".to_owned(),
            "session".to_owned(),
        ];
        let positive = fs::File::open(root_path.join("reports")).unwrap();
        assert_eq!(entries(&positive), expected);
        let listing = list_dir_beneath(&root, Path::new("reports")).unwrap();
        assert!(listing.metadata().unwrap().is_dir());
        assert_eq!(entries(&listing), expected);
        // Check the ENOSYS component-walk path returns a listable fd as well.
        let fallback = finish_beneath_open(
            Err(std::io::Error::from_raw_os_error(libc::ENOSYS)),
            root.as_fd(),
            Path::new("reports"),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
        .unwrap();
        assert_eq!(entries(&fs::File::from(fallback)), expected);
        std::os::unix::fs::symlink("reports", root_path.join("link")).unwrap();
        for relative in ["link", "link/nested", "reports/session", ".", "..", ""] {
            assert!(
                list_dir_beneath(&root, Path::new(relative)).is_err(),
                "{relative}"
            );
        }
        fs::rename(&root_path, temp.path().join("moved-root")).unwrap();
        fs::create_dir_all(root_path.join("reports")).unwrap();
        fs::write(root_path.join("reports/replacement"), b"wrong root").unwrap();
        let pinned = list_dir_beneath(&root, Path::new("reports")).unwrap();
        assert_eq!(entries(&pinned), expected);
    }

    #[test]
    fn open_beneath_enosys_walk_refuses_links_and_never_falls_back_for_other_errors() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("sub")).unwrap();
        fs::write(temp.path().join("sub/file"), b"inside").unwrap();
        assert_eq!(fs::read(temp.path().join("sub/file")).unwrap(), b"inside");
        std::os::unix::fs::symlink("sub", temp.path().join("link")).unwrap();
        let root = fs::File::open(temp.path()).unwrap();
        let flags =
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let fallback = |relative: &str| {
            finish_beneath_open(
                Err(std::io::Error::from_raw_os_error(libc::ENOSYS)),
                root.as_fd(),
                Path::new(relative),
                flags,
            )
        };
        let mut contents = String::new();
        regular_file(fallback("sub/file").unwrap(), Path::new("sub/file"))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "inside");
        for relative in ["link/file", "../file", "sub/./file", "/sub/file", ""] {
            assert!(fallback(relative).is_err(), "{relative}");
        }
        let denied = finish_beneath_open(
            Err(std::io::Error::from_raw_os_error(libc::EACCES)),
            root.as_fd(),
            Path::new("sub/file"),
            flags,
        );
        assert_eq!(denied.unwrap_err().raw_os_error(), Some(libc::EACCES));
    }

    #[test]
    fn write_private_file_does_not_follow_a_planted_tmp_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("outside");
        fs::write(&sentinel, b"sentinel").unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
        let file = temp.path().join("operator.token");
        std::os::unix::fs::symlink(&sentinel, file.with_extension("tmp")).unwrap();
        write_private_file(&file, b"synthetic token").unwrap();
        assert_eq!(
            fs::read(&sentinel).unwrap(),
            b"sentinel",
            "temporary names must never truncate a linked target"
        );
        assert_eq!(fs::read(&file).unwrap(), b"synthetic token");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::os::unix::fs::symlink(&sentinel, temp.path().join("destination")).unwrap();
        write_private_file(&temp.path().join("destination"), b"replacement").unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
        assert_eq!(
            fs::read(temp.path().join("destination")).unwrap(),
            b"replacement"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 4);
        let directory = temp.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(write_private_file(&directory, b"must not replace a directory").is_err());
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            5,
            "failed rename must remove its temporary file"
        );
        #[cfg(target_os = "linux")]
        {
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o311)).unwrap();
            let result = write_private_file(&directory.join("private"), b"search-only parent");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            result.unwrap();
            assert_eq!(
                fs::read(directory.join("private")).unwrap(),
                b"search-only parent"
            );
        }
    }

    #[test]
    fn long_runtime_dirs_use_a_bindable_socket_fallback() {
        for prefix in [
            format!("/tmp/{}", "a".repeat(100)),
            format!("/tmp/{}", "界".repeat(50)),
        ] {
            let runtime = runtime_dir_from(Some(OsStr::new(&prefix)), uid());
            let socket = socket_path_in(Path::new("/workspace/.crew"), &runtime);
            assert!(
                socket.as_os_str().as_bytes().len() <= 103,
                "{}",
                socket.display()
            );
            assert_eq!(runtime, runtime_dir_from(None, uid()));
        }
    }

    #[test]
    fn socket_paths_are_short_and_stable() {
        let runtime = Path::new("/tmp/crew-test");
        let a = socket_path_in(Path::new("/some/very/long/workspace/path/.crew"), runtime);
        let b = socket_path_in(Path::new("/some/very/long/workspace/path/.crew"), runtime);
        let c = socket_path_in(Path::new("/another/.crew"), runtime);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.to_string_lossy().len() < 100, "{}", a.display());
    }

    #[test]
    fn private_files_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        ensure_private_dir(&sub).unwrap();
        assert_eq!(
            fs::metadata(&sub).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = sub.join("identity.json");
        write_private_file(&file, b"{}").unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_private_file(&file, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn private_directories_reject_symlinks_without_chmodding_the_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = temp.path().join("crew-uid");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn private_directories_reject_symlink_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o1777)).unwrap();
        let target = temp.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temp.path().join("crew-uid");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ensure_private_dir(&link.join("crew/sessions")).is_err());
        assert!(!target.join("crew").exists());
    }

    #[test]
    fn private_directories_preserve_trusted_ancestor_layouts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o775)).unwrap();
        ensure_private_dir(&workspace.join(".crew")).unwrap();
        assert_eq!(
            fs::metadata(workspace.join(".crew"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
            0o775
        );
        let link = temp.path().join("home-manager-link");
        std::os::unix::fs::symlink(&workspace, &link).unwrap();
        ensure_private_dir(&link.join("state/crew/sessions")).unwrap();
        assert!(workspace.join("state/crew/sessions").is_dir());
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o311)).unwrap();
        let result = ensure_private_dir(&workspace.join("search-only"));
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o775)).unwrap();
        result.unwrap();
    }

    #[test]
    fn pid_liveness() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
    }
}
