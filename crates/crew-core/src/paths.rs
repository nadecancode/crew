//! Where crew keeps things: the per-workspace state directory, the daemon
//! socket, the registry of running daemons and per-agent identity files.

use std::ffi::OsStr;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
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
