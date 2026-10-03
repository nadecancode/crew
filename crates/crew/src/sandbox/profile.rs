//! Building a launch [`Spec`] from the daemon's paths (design-b3 §3.2).
//!
//! A sandbox starts empty: a private `/tmp` (with `/tmp/run` as
//! `XDG_RUNTIME_DIR`), `$HOME` as an empty tmpfs, then only what is listed
//! here. Three profiles:
//! - a **worker** gets its worktree and the git data it needs to commit,
//!   its agent directory (read-only, with `rw/` writable), the team's
//!   cargo cache, the daemon socket and its sidecar directory;
//! - the **orchestrator** gets the checkout read-only with `.crew` and
//!   `.git` masked, and the same agent mounts, but no worktree;
//! - a **helper** (git on behalf of no agent) gets one worktree and the git
//!   data: no identity, socket, agent directory, credentials or network.
//!
//! Every profile gets the system trees, the toolchain leaves and the PATH
//! entries that pass validation, all read-only.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use super::landlock;
use super::pin::{identify, Identity, Kind};
use super::scan::{scan_excluding, Findings};
use super::spec::{check_name, Mount, Spec};
use super::validate::{host_members, Protected};

/// System trees bound read-only when present. Not `/nix` as a whole: that
/// would expose the Nix daemon socket.
pub const SYSTEM_TREES: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/nix/store",
    "/nix/var/nix/profiles",
    "/run/current-system",
];

/// Inside the sandbox, `XDG_RUNTIME_DIR`.
const RUNTIME_DIR: &str = "/tmp/run";

/// Facts about the host every profile uses.
#[derive(Clone, Debug)]
pub struct Host {
    pub home: PathBuf,
    /// `PATH` as the daemon sees it.
    pub path: OsString,
    pub bwrap: PathBuf,
    pub crew_bin: PathBuf,
    /// `$CARGO_HOME/bin`: rustup's proxies.
    pub cargo_bin: PathBuf,
    pub rustup_home: PathBuf,
    pub gitconfig: PathBuf,
    pub system: Vec<PathBuf>,
    pub resolv_conf: PathBuf,
    /// The Nix store, whose socket scan the Landlock socket rule replaces.
    pub nix_store: PathBuf,
    pub landlock_abi: u32,
    /// The host part of the protected set.
    pub protected: Protected,
}

impl Host {
    /// The host as the daemon's environment describes it. `bwrap` and the
    /// Landlock ABI come from the probe.
    pub fn detect(crew_bin: &Path, bwrap: &Path, landlock_abi: u32) -> Self {
        let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let home = env("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let cargo_home = env("CARGO_HOME").map_or_else(|| home.join(".cargo"), PathBuf::from);
        Self {
            path: env("PATH").unwrap_or_default(),
            bwrap: bwrap.to_path_buf(),
            crew_bin: crew_bin.to_path_buf(),
            cargo_bin: cargo_home.join("bin"),
            rustup_home: env("RUSTUP_HOME").map_or_else(|| home.join(".rustup"), PathBuf::from),
            gitconfig: home.join(".gitconfig"),
            system: SYSTEM_TREES.iter().map(PathBuf::from).collect(),
            resolv_conf: "/etc/resolv.conf".into(),
            nix_store: "/nix/store".into(),
            landlock_abi,
            protected: host_members(&home, &env),
            home,
        }
    }
}

/// Paths of the session, as the daemon created them. All canonical.
#[derive(Clone, Copy, Debug)]
pub struct Session<'a> {
    pub workspace: &'a Path,
    /// The repository's common git directory (usually `<ws>/.git`).
    pub git_dir: &'a Path,
    /// The daemon's runtime directory `<R>`.
    pub runtime_dir: &'a Path,
    pub daemon_socket: &'a Path,
    /// `<C>/cargo`, the team's `CARGO_HOME`.
    pub cargo_home: &'a Path,
}

#[derive(Clone, Copy, Debug)]
pub enum Role<'a> {
    Worker {
        agent_dir: &'a Path,
        worktree: &'a Path,
        /// `<git_dir>/worktrees/<id>`.
        admin: &'a Path,
        sidecar_dir: Option<&'a Path>,
    },
    Orchestrator {
        agent_dir: &'a Path,
        sidecar_dir: Option<&'a Path>,
    },
    Helper {
        worktree: &'a Path,
        admin: &'a Path,
    },
}

/// What the workspace configuration adds.
#[derive(Clone, Debug, Default)]
pub struct Extra {
    pub read_only: Vec<PathBuf>,
    pub read_write: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub session: Session<'a>,
    pub role: Role<'a>,
    pub extra: &'a Extra,
    /// The agent CLI's program; its install root is bound.
    pub cli: Option<&'a Path>,
    /// Names of the runtime's launch variables (their values reach
    /// `crew sandbox-run` separately). Helpers get none.
    pub pass_env: &'a [String],
    /// The process runs on a PTY (otherwise it gets a new session).
    pub terminal: bool,
}

#[derive(Debug)]
pub struct Built {
    pub spec: Spec,
    /// What was left out, for a warning event (dropped PATH entries and
    /// toolchain leaves).
    pub warnings: Vec<String>,
}

pub fn build(host: &Host, request: &Request) -> Result<Built, String> {
    Builder::new(host, request)?.build()
}

struct Builder<'a> {
    host: &'a Host,
    request: &'a Request<'a>,
    protected: Protected,
    mounts: Vec<Mount>,
    /// Read-only binds, to tell what an earlier mount already covers.
    covered: Vec<PathBuf>,
    findings: Findings,
    path: Vec<PathBuf>,
    warnings: Vec<String>,
}

impl<'a> Builder<'a> {
    fn new(host: &'a Host, request: &'a Request<'a>) -> Result<Self, String> {
        let s = &request.session;
        let mut generated = vec![
            s.workspace,
            s.git_dir,
            s.runtime_dir,
            s.daemon_socket,
            s.cargo_home,
        ];
        match request.role {
            Role::Worker {
                agent_dir,
                worktree,
                admin,
                sidecar_dir,
            } => {
                generated.extend([agent_dir, worktree, admin]);
                generated.extend(sidecar_dir);
            }
            Role::Orchestrator {
                agent_dir,
                sidecar_dir,
            } => {
                generated.push(agent_dir);
                generated.extend(sidecar_dir);
            }
            Role::Helper { worktree, admin } => {
                generated.extend([worktree, admin]);
                if !request.pass_env.is_empty() {
                    return Err("a helper sandbox gets no launch variables".into());
                }
            }
        }
        for path in generated {
            canonical(path)?;
        }
        let mut protected = host.protected.clone();
        protected.add(s.workspace);
        protected.add(s.git_dir);
        if let Some(repo) = s.git_dir.parent() {
            protected.add(repo);
        }
        protected.add(s.runtime_dir);
        Ok(Self {
            host,
            request,
            protected,
            mounts: Vec::new(),
            covered: Vec::new(),
            findings: Findings::default(),
            path: Vec::new(),
            warnings: Vec::new(),
        })
    }

    fn enforces_resolve_unix(&self) -> bool {
        self.host.landlock_abi >= landlock::RESOLVE_UNIX_ABI
    }

    fn build(mut self) -> Result<Built, String> {
        let host = self.host;
        let request = self.request;
        let session = request.session;
        self.mounts.push(Mount::Tmpfs {
            path: "/tmp".into(),
        });
        self.mounts.push(Mount::Dir {
            path: RUNTIME_DIR.into(),
        });
        if host.home != Path::new("/") {
            self.mounts.push(Mount::Tmpfs {
                path: host.home.clone(),
            });
        }
        for tree in &host.system {
            self.system_tree(tree)?;
        }
        if let Role::Orchestrator { .. } = request.role {
            self.checkout_read_only()?;
        }
        self.toolchain()?;
        self.path_entries();
        for (paths, writable) in [
            (&request.extra.read_only, false),
            (&request.extra.read_write, true),
        ] {
            for path in paths {
                self.protected
                    .validate(path)
                    .and_then(|canonical| self.bind_scanned(&canonical, writable, &[]))
                    .map_err(|e| format!("workspace.sandbox entry refused: {e}"))?;
            }
        }
        self.resolv_conf(&host.resolv_conf);
        let home = match request.role {
            Role::Worker {
                agent_dir,
                worktree,
                admin,
                sidecar_dir,
            } => {
                self.git(admin)?;
                self.generated(worktree, true)?;
                self.agent(agent_dir, sidecar_dir)?
            }
            Role::Orchestrator {
                agent_dir,
                sidecar_dir,
            } => self.agent(agent_dir, sidecar_dir)?,
            Role::Helper { worktree, admin } => {
                self.git(admin)?;
                self.generated(worktree, true)?;
                host.home.clone()
            }
        };
        for socket in std::mem::take(&mut self.findings.sockets) {
            self.mounts.push(Mount::Mask { path: socket });
        }
        for dir in std::mem::take(&mut self.findings.unlisted) {
            self.mounts.push(Mount::Tmpfs { path: dir });
        }

        let agent = !matches!(request.role, Role::Helper { .. });
        let mut env = vec![
            ("HOME".to_string(), utf8(&home)?),
            ("XDG_RUNTIME_DIR".to_string(), RUNTIME_DIR.to_string()),
            ("PATH".to_string(), self.sandbox_path()),
        ];
        if agent {
            env.push(("CARGO_HOME".into(), utf8(session.cargo_home)?));
        }
        if let Some(rustup) = self.bound(&host.rustup_home) {
            env.push(("RUSTUP_HOME".into(), utf8(&rustup)?));
        }
        if let Some(gitconfig) = self.bound(&host.gitconfig) {
            env.push(("GIT_CONFIG_GLOBAL".into(), utf8(&gitconfig)?));
        }
        let crew_set: Vec<String> = env.iter().map(|(n, _)| n.clone()).collect();
        let mut pass_env = Vec::new();
        for name in request.pass_env {
            check_name(name)?;
            if !crew_set.contains(name) && !pass_env.contains(name) {
                pass_env.push(name.clone());
            }
        }
        let cwd = match request.role {
            Role::Worker { worktree, .. } | Role::Helper { worktree, .. } => worktree,
            Role::Orchestrator { .. } => session.workspace,
        };
        let min_landlock_abi = if self.enforces_resolve_unix() {
            landlock::RESOLVE_UNIX_ABI
        } else {
            landlock::MIN_ABI
        };
        let spec = Spec {
            bwrap: host.bwrap.clone(),
            crew_bin: canonical_file(&host.crew_bin)?,
            mounts: self.mounts,
            env,
            pass_env,
            cwd: cwd.to_path_buf(),
            network: agent,
            new_session: !agent || !request.terminal,
            min_landlock_abi,
            allow_unix: if agent {
                vec![session.runtime_dir.to_path_buf()]
            } else {
                Vec::new()
            },
        };
        spec.check()?;
        Ok(Built {
            spec,
            warnings: self.warnings,
        })
    }

    /// A system tree: root-owned and not writable by the operator at its
    /// top. A symlink (merged /usr, /run/current-system) is recreated.
    fn system_tree(&mut self, tree: &Path) -> Result<(), String> {
        let Ok(meta) = std::fs::symlink_metadata(tree) else {
            return Ok(());
        };
        if meta.file_type().is_symlink() {
            let target =
                std::fs::read_link(tree).map_err(|e| format!("{}: {e}", tree.display()))?;
            self.mounts.push(Mount::Symlink {
                path: tree.to_path_buf(),
                target,
            });
            return Ok(());
        }
        let canonical = canonical(tree)?;
        self.protected.check(&canonical)?;
        if meta.uid() != 0 || writable_by_operator(&canonical) {
            return Err(format!(
                "system tree {} must be owned by root and not writable by you",
                canonical.display()
            ));
        }
        if !needs_socket_scan(&canonical, &self.host.nix_store, self.host.landlock_abi) {
            self.bind(&canonical, false)
        } else {
            self.bind_scanned(&canonical, false, &[])
        }
    }

    /// `/etc/resolv.conf` usually points outside the bound trees (into
    /// `/run`). Follow the symlink chain, recreating the links that lie
    /// outside the bound trees, and bind exactly the final regular file,
    /// never a directory around it.
    fn resolv_conf(&mut self, start: &Path) {
        let mut at = start.to_path_buf();
        for _ in 0..8 {
            let Ok(meta) = std::fs::symlink_metadata(&at) else {
                return;
            };
            let covered = self.covered(&at);
            if meta.file_type().is_symlink() {
                let Ok(target) = std::fs::read_link(&at) else {
                    return;
                };
                if !covered {
                    self.mounts.push(Mount::Symlink {
                        path: at.clone(),
                        target: target.clone(),
                    });
                }
                match normalize(&at.parent().unwrap_or(Path::new("/")).join(target)) {
                    Some(next) => at = next,
                    None => return,
                }
                continue;
            }
            if covered {
                return;
            }
            match identify(&at) {
                Ok(id) if id.kind == Kind::File => self.mounts.push(Mount::Bind {
                    path: at,
                    id,
                    writable: false,
                }),
                _ => self.warnings.push(format!(
                    "{} is not a regular file; DNS may not work",
                    at.display()
                )),
            }
            return;
        }
    }

    /// The orchestrator's view of the checkout: read-only, with `.crew` and
    /// `.git` masked.
    fn checkout_read_only(&mut self) -> Result<(), String> {
        let ws = self.request.session.workspace;
        let crew = ws.join(crew_core::paths::STATE_DIR_NAME);
        let dot_git = ws.join(".git");
        self.generated(ws, false)?;
        self.scan(ws, &[&crew, &dot_git])?;
        for masked in [crew, dot_git] {
            match std::fs::symlink_metadata(&masked) {
                Ok(meta) if meta.is_dir() => self.mounts.push(Mount::Tmpfs { path: masked }),
                Ok(_) => self.mounts.push(Mount::Mask { path: masked }),
                Err(_) => {}
            }
        }
        Ok(())
    }

    fn toolchain(&mut self) -> Result<(), String> {
        let host = self.host;
        let crew_bin = canonical_file(&host.crew_bin)?;
        // The binary may sit in the main checkout's target/ (a development
        // build): one read-only file, an exception to rule 2.
        let in_checkout = crew_bin.starts_with(self.request.session.workspace.join("target"));
        if !in_checkout {
            self.protected
                .check(&crew_bin)
                .map_err(|e| format!("the crew binary cannot be bound: {e}"))?;
        }
        self.bind_scanned(&crew_bin, false, &[])?;
        if let Some(cli) = self.request.cli {
            self.protected
                .validate(&install_root(cli)?)
                .and_then(|root| self.bind_scanned(&root, false, &[]))
                .map_err(|e| format!("the agent CLI cannot be bound: {e}"))?;
        }
        for leaf in [&host.cargo_bin, &host.rustup_home, &host.gitconfig] {
            if std::fs::symlink_metadata(leaf).is_err() {
                continue;
            }
            let bound = self
                .protected
                .validate(leaf)
                .and_then(|canonical| self.bind_scanned(&canonical, false, &[]));
            if let Err(e) = bound {
                self.warnings.push(format!("toolchain path left out: {e}"));
            }
        }
        Ok(())
    }

    fn path_entries(&mut self) {
        let path = self.host.path.clone();
        for entry in std::env::split_paths(&path) {
            if !entry.is_absolute() {
                self.warnings.push(format!(
                    "PATH entry {:?} left out: not absolute",
                    entry.display()
                ));
                continue;
            }
            if !entry.is_dir() {
                continue;
            }
            let bound = self.protected.validate(&entry).and_then(|canonical| {
                if canonical.to_str().is_none() {
                    return Err(format!("{} is not valid UTF-8", canonical.display()));
                }
                self.bind_scanned(&canonical, false, &[])
                    .map(|()| canonical)
            });
            match bound {
                Ok(canonical) if !self.path.contains(&canonical) => self.path.push(canonical),
                Ok(_) => {}
                Err(e) => self.warnings.push(format!("PATH entry left out: {e}")),
            }
        }
    }

    /// The git data a worktree needs: the common git directory read-only,
    /// with objects and refs writable (to commit), the worktree's own admin
    /// directory writable, and the three files that say which repository
    /// and which per-worktree config it uses read-only over it.
    fn git(&mut self, admin: &Path) -> Result<(), String> {
        let git_dir = self.request.session.git_dir;
        if admin.parent() != Some(&git_dir.join("worktrees")) {
            return Err(format!(
                "{} is not a worktree admin directory of {}",
                admin.display(),
                git_dir.display()
            ));
        }
        self.generated(git_dir, false)?;
        self.scan(git_dir, &[])?;
        for sub in ["objects", "refs", "logs", "packed-refs"] {
            let path = git_dir.join(sub);
            if std::fs::symlink_metadata(&path).is_ok() {
                self.generated(&path, true)?;
            }
        }
        self.generated(admin, true)?;
        for file in ["commondir", "gitdir", "config.worktree"] {
            let path = admin.join(file);
            let id = identify(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if id.kind != Kind::File {
                return Err(format!("{} is not a regular file", path.display()));
            }
            self.mounts.push(Mount::Bind {
                path,
                id,
                writable: false,
            });
        }
        Ok(())
    }

    /// The agent directory and the agent's shared mounts. Returns `$HOME`.
    fn agent(&mut self, agent_dir: &Path, sidecar_dir: Option<&Path>) -> Result<PathBuf, String> {
        let session = self.request.session;
        self.generated(agent_dir, false)?;
        let rw = agent_dir.join("rw");
        self.generated(&rw, true)?;
        self.generated(session.cargo_home, true)?;
        if session.daemon_socket.parent() != Some(session.runtime_dir) {
            return Err("the daemon socket must sit directly in the runtime directory".into());
        }
        let id = identify(session.daemon_socket)
            .map_err(|e| format!("{}: {e}", session.daemon_socket.display()))?;
        if id.kind != Kind::Socket {
            return Err(format!(
                "{} is not a socket",
                session.daemon_socket.display()
            ));
        }
        self.mounts.push(Mount::Bind {
            path: session.daemon_socket.to_path_buf(),
            id,
            writable: true,
        });
        if let Some(dir) = sidecar_dir {
            if !dir.starts_with(session.runtime_dir) || dir == session.runtime_dir {
                return Err(format!(
                    "the sidecar directory {} must lie inside the runtime directory",
                    dir.display()
                ));
            }
            self.generated(dir, true)?;
        }
        Ok(rw.join("home"))
    }

    /// A daemon-generated path, bound at its own path without scanning.
    fn generated(&mut self, path: &Path, writable: bool) -> Result<(), String> {
        canonical(path)?;
        self.bind(path, writable)
    }

    fn bind_scanned(
        &mut self,
        path: &Path,
        writable: bool,
        excluded: &[&Path],
    ) -> Result<(), String> {
        if !writable && self.covered(path) {
            return Ok(());
        }
        self.bind(path, writable)?;
        self.scan(path, excluded)
    }

    fn bind(&mut self, path: &Path, writable: bool) -> Result<(), String> {
        if let Some(hidden) = self.mounts.iter().find(|m| m.path().starts_with(path)) {
            return Err(format!(
                "{} would hide {}",
                path.display(),
                hidden.path().display()
            ));
        }
        let id = identify(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.mounts.push(Mount::Bind {
            path: path.to_path_buf(),
            id,
            writable,
        });
        if !writable {
            self.covered.push(path.to_path_buf());
        }
        Ok(())
    }

    fn scan(&mut self, path: &Path, excluded: &[&Path]) -> Result<(), String> {
        let found = scan_excluding(path, excluded)
            .map_err(|e| format!("scanning {} for sockets: {e}", path.display()))?;
        self.findings.sockets.extend(found.sockets);
        self.findings.unlisted.extend(found.unlisted);
        Ok(())
    }

    fn covered(&self, path: &Path) -> bool {
        self.covered.iter().any(|c| path.starts_with(c))
    }

    /// The canonical form of `path` when a read-only bind covers it.
    fn bound(&self, path: &Path) -> Option<PathBuf> {
        path.canonicalize().ok().filter(|p| self.covered(p))
    }

    fn sandbox_path(&self) -> String {
        let entries: Vec<&str> = self.path.iter().filter_map(|p| p.to_str()).collect();
        entries.join(":")
    }
}

/// Whether a system tree must be walked for sockets. The Nix store is the
/// exception, and only when the launch enforces Landlock's socket rule
/// (the spec then demands ABI 9, so an older kernel fails closed): that
/// rule refuses every socket made outside the sandbox, and walking a live
/// store takes minutes.
pub fn needs_socket_scan(tree: &Path, nix_store: &Path, landlock_abi: u32) -> bool {
    !(tree.starts_with(nix_store) && landlock_abi >= landlock::RESOLVE_UNIX_ABI)
}

/// The directory a CLI is installed in: the npm package for a Node CLI,
/// otherwise the program file itself (a native installer's single binary).
pub fn install_root(program: &Path) -> Result<PathBuf, String> {
    let resolved = program
        .canonicalize()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let components: Vec<Component> = resolved.components().collect();
    if let Some(at) = components
        .iter()
        .rposition(|c| c.as_os_str() == OsStr::new("node_modules"))
    {
        let scoped = components
            .get(at + 1)
            .is_some_and(|c| c.as_os_str().as_bytes().starts_with(b"@"));
        let end = at + if scoped { 3 } else { 2 };
        if end <= components.len() {
            return Ok(components[..end].iter().collect());
        }
    }
    Ok(resolved)
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    match path.canonicalize() {
        Ok(canonical) if canonical == path => Ok(canonical),
        Ok(canonical) => Err(format!(
            "{} is not canonical (it resolves to {})",
            path.display(),
            canonical.display()
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn canonical_file(path: &Path) -> Result<PathBuf, String> {
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match identify(&resolved) {
        Ok(Identity {
            kind: Kind::File, ..
        }) => Ok(resolved),
        _ => Err(format!("{} is not a regular file", resolved.display())),
    }
}

/// `path` with `.` and `..` resolved lexically (for symlink targets).
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
            Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

fn writable_by_operator(path: &Path) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `c_path` is a valid C string. access() checks the real uid,
    // the operator's, including ACLs and read-only mounts.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

fn utf8(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    /// A host and session laid out the way crew lays them out, in a
    /// temporary directory: a home with a toolchain and secrets, a
    /// workspace with two agents, a runtime directory with the daemon
    /// socket and token, and a cargo cache.
    struct Fixture {
        _dir: tempfile::TempDir,
        _socket: UnixListener,
        root: PathBuf,
        home: PathBuf,
        ws: PathBuf,
        git: PathBuf,
        run: PathBuf,
        socket: PathBuf,
        cargo: PathBuf,
        host: Host,
    }

    fn mkdirs(paths: &[&Path]) {
        for path in paths {
            std::fs::create_dir_all(path).unwrap();
        }
    }

    fn touch(paths: &[&Path]) {
        for path in paths {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home/u");
        let ws = root.join("ws");
        let git = ws.join(".git");
        let run = root.join("run/crew");
        let cargo = root.join("cache/crew/0123/cargo");
        mkdirs(&[
            &home.join(".cargo/bin"),
            &home.join(".cargo/registry"),
            &home.join(".rustup/toolchains/stable"),
            &home.join(".local/bin"),
            &home.join(".config/bin"),
            &home.join(".ssh"),
            &home.join("data"),
            &home.join("scratch"),
            &git.join("objects"),
            &git.join("refs/heads"),
            &git.join("logs"),
            &git.join("hooks"),
            &ws.join(".crew/agents/a/rw/home"),
            &ws.join(".crew/agents/b/rw"),
            &ws.join(".crew/worktrees/a"),
            &ws.join(".crew/worktrees/b"),
            &ws.join("src"),
            &run.join("0123/a"),
            &cargo,
        ]);
        touch(&[
            &home.join(".cargo/bin/cargo"),
            &home.join(".local/share/claude/versions/2.1.0"),
            &home.join(".gitconfig"),
            &home.join(".config/secret"),
            &home.join(".ssh/id_ed25519"),
            &home.join("notes.txt"),
            &git.join("config"),
            &git.join("HEAD"),
            &git.join("packed-refs"),
            &ws.join(".crew/crew.db"),
            &ws.join(".crew/agents/a/identity.json"),
            &ws.join(".crew/agents/b/identity.json"),
            &ws.join("target/release/crew"),
            &ws.join("secret.txt"),
            &run.join("0123.token"),
        ]);
        for id in ["a", "b", "_integration"] {
            let admin = git.join("worktrees").join(id);
            touch(&[
                &admin.join("commondir"),
                &admin.join("gitdir"),
                &admin.join("config.worktree"),
                &admin.join("HEAD"),
            ]);
        }
        mkdirs(&[&ws.join(".crew/worktrees/_integration")]);
        symlink(
            home.join(".local/share/claude/versions/2.1.0"),
            home.join(".local/bin/claude"),
        )
        .unwrap();
        let socket = run.join("0123.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut protected = Protected::new(&home);
        for path in [".claude", ".codex", ".config", ".ssh", ".local/state/crew"] {
            protected.add(&home.join(path));
        }
        protected.add(&root.join("cache/crew"));
        protected.add_except(&home.join(".cargo"), &home.join(".cargo/bin"));
        let host = Host {
            home: home.clone(),
            path: std::env::join_paths([home.join(".cargo/bin")]).unwrap(),
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: ws.join("target/release/crew"),
            cargo_bin: home.join(".cargo/bin"),
            rustup_home: home.join(".rustup"),
            gitconfig: home.join(".gitconfig"),
            system: Vec::new(),
            resolv_conf: root.join("no-resolv.conf"),
            nix_store: "/nix/store".into(),
            landlock_abi: 9,
            protected,
        };
        Fixture {
            _dir: dir,
            _socket: listener,
            root,
            home,
            ws,
            git,
            run,
            socket,
            cargo,
            host,
        }
    }

    struct Paths {
        agent: PathBuf,
        worktree: PathBuf,
        admin: PathBuf,
        sidecar: PathBuf,
    }

    impl Fixture {
        fn paths(&self, id: &str) -> Paths {
            Paths {
                agent: self.ws.join(".crew/agents").join(id),
                worktree: self.ws.join(".crew/worktrees").join(id),
                admin: self.git.join("worktrees").join(id),
                sidecar: self.run.join("0123").join(id),
            }
        }

        fn session(&self) -> Session<'_> {
            Session {
                workspace: &self.ws,
                git_dir: &self.git,
                runtime_dir: &self.run,
                daemon_socket: &self.socket,
                cargo_home: &self.cargo,
            }
        }

        fn build(&self, role: Role, extra: &Extra, pass_env: &[String]) -> Result<Built, String> {
            let cli = self.home.join(".local/bin/claude");
            let request = Request {
                session: self.session(),
                role,
                extra,
                cli: (!matches!(role, Role::Helper { .. })).then_some(cli.as_path()),
                pass_env,
                terminal: true,
            };
            build(&self.host, &request)
        }

        fn worker(&self, extra: &Extra) -> Result<Built, String> {
            let p = self.paths("a");
            self.build(
                Role::Worker {
                    agent_dir: &p.agent,
                    worktree: &p.worktree,
                    admin: &p.admin,
                    sidecar_dir: Some(&p.sidecar),
                },
                extra,
                &[
                    "ANTHROPIC_API_KEY".into(),
                    "CREW_IDENTITY".into(),
                    "HOME".into(),
                ],
            )
        }
    }

    fn binds(spec: &Spec) -> Vec<(PathBuf, bool)> {
        spec.mounts
            .iter()
            .filter_map(|m| match m {
                Mount::Bind { path, writable, .. } => Some((path.clone(), *writable)),
                _ => None,
            })
            .collect()
    }

    /// Whether `path` can be seen inside: some bind covers it and no
    /// later tmpfs or mask hides it.
    fn visible(spec: &Spec, path: &Path) -> bool {
        let mut seen = false;
        for mount in &spec.mounts {
            match mount {
                Mount::Bind { path: p, .. } if path.starts_with(p) => seen = true,
                Mount::Tmpfs { path: p } | Mount::Mask { path: p } if path.starts_with(p) => {
                    seen = false
                }
                _ => {}
            }
        }
        seen
    }

    fn env<'s>(spec: &'s Spec, name: &str) -> Option<&'s str> {
        spec.env
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_worker_on_the_host_layout_gets_exactly_the_toolchain_leaves() {
        let f = fixture();
        let built = f.worker(&Extra::default()).unwrap();
        let p = f.paths("a");
        let under = |root: &Path| -> Vec<(PathBuf, bool)> {
            binds(&built.spec)
                .into_iter()
                .filter(|(path, _)| path.starts_with(root))
                .collect()
        };
        assert_eq!(
            under(&f.home),
            vec![
                (f.home.join(".local/share/claude/versions/2.1.0"), false),
                (f.home.join(".cargo/bin"), false),
                (f.home.join(".rustup"), false),
                (f.home.join(".gitconfig"), false),
            ]
        );
        assert_eq!(
            under(&f.ws),
            vec![
                (f.ws.join("target/release/crew"), false),
                (f.git.clone(), false),
                (f.git.join("objects"), true),
                (f.git.join("refs"), true),
                (f.git.join("logs"), true),
                (f.git.join("packed-refs"), true),
                (p.admin.clone(), true),
                (p.admin.join("commondir"), false),
                (p.admin.join("gitdir"), false),
                (p.admin.join("config.worktree"), false),
                (p.worktree.clone(), true),
                (p.agent.clone(), false),
                (p.agent.join("rw"), true),
            ]
        );
        assert_eq!(
            under(&f.run),
            vec![(f.socket.clone(), true), (p.sidecar.clone(), true)]
        );
        assert_eq!(under(&f.root.join("cache")), vec![(f.cargo.clone(), true)]);
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
    }

    #[test]
    fn a_worker_never_sees_the_token_the_database_or_other_agents() {
        let f = fixture();
        let spec = f.worker(&Extra::default()).unwrap().spec;
        for hidden in [
            f.run.join("0123.token"),
            f.run.clone(),
            f.ws.join(".crew/crew.db"),
            f.ws.join(".crew/agents/b/identity.json"),
            f.ws.join(".crew/worktrees/b"),
            f.ws.join("secret.txt"),
            f.ws.join("src"),
            f.home.join(".config/secret"),
            f.home.join(".ssh/id_ed25519"),
            f.home.join(".cargo/registry"),
            f.home.join("notes.txt"),
            f.run.join("0123/b"),
        ] {
            assert!(!visible(&spec, &hidden), "{} is visible", hidden.display());
        }
        assert!(visible(&spec, &f.ws.join(".crew/agents/a/identity.json")));
    }

    #[test]
    fn a_worker_gets_crews_variables_and_passes_only_the_launch_names() {
        let f = fixture();
        let spec = f.worker(&Extra::default()).unwrap().spec;
        let p = f.paths("a");
        assert_eq!(env(&spec, "HOME"), p.agent.join("rw/home").to_str());
        assert_eq!(env(&spec, "XDG_RUNTIME_DIR"), Some("/tmp/run"));
        assert_eq!(env(&spec, "PATH"), f.home.join(".cargo/bin").to_str());
        assert_eq!(env(&spec, "CARGO_HOME"), f.cargo.to_str());
        assert_eq!(env(&spec, "RUSTUP_HOME"), f.home.join(".rustup").to_str());
        assert_eq!(
            env(&spec, "GIT_CONFIG_GLOBAL"),
            f.home.join(".gitconfig").to_str()
        );
        assert_eq!(spec.pass_env, ["ANTHROPIC_API_KEY", "CREW_IDENTITY"]);
        assert_eq!(spec.cwd, p.worktree);
        assert!(spec.network);
        assert!(!spec.new_session);
        assert_eq!(spec.allow_unix, vec![f.run.clone()]);
        assert_eq!(spec.min_landlock_abi, 9);
        assert_eq!(spec.crew_bin, f.ws.join("target/release/crew"));
    }

    #[test]
    fn a_helper_has_no_identity_socket_agent_directory_or_network() {
        let f = fixture();
        let p = f.paths("_integration");
        let role = Role::Helper {
            worktree: &p.worktree,
            admin: &p.admin,
        };
        let spec = f.build(role, &Extra::default(), &[]).unwrap().spec;
        for path in binds(&spec).iter().map(|(p, _)| p) {
            assert!(!path.starts_with(f.ws.join(".crew/agents")), "{path:?}");
            assert!(!path.starts_with(&f.run), "{path:?}");
            assert!(!path.starts_with(f.root.join("cache")), "{path:?}");
        }
        assert!(visible(&spec, &p.worktree));
        assert!(!spec.network);
        assert!(spec.new_session);
        assert!(spec.allow_unix.is_empty());
        assert!(spec.pass_env.is_empty());
        assert_eq!(env(&spec, "HOME"), f.home.to_str());
        assert_eq!(env(&spec, "CARGO_HOME"), None);
        let error = f
            .build(role, &Extra::default(), &["CREW_IDENTITY".into()])
            .unwrap_err();
        assert!(error.contains("no launch variables"), "{error}");
    }

    #[test]
    fn the_orchestrator_reads_the_checkout_with_crew_state_and_git_masked() {
        let f = fixture();
        let p = f.paths("orchestrator");
        mkdirs(&[&p.agent.join("rw")]);
        let role = Role::Orchestrator {
            agent_dir: &p.agent,
            sidecar_dir: None,
        };
        let spec = f.build(role, &Extra::default(), &[]).unwrap().spec;
        assert!(binds(&spec).contains(&(f.ws.clone(), false)));
        assert!(!binds(&spec)
            .iter()
            .any(|(path, w)| path.starts_with(&f.ws) && *w && !path.starts_with(&p.agent)));
        assert!(visible(&spec, &f.ws.join("src")));
        assert!(!visible(&spec, &f.ws.join(".crew/crew.db")));
        assert!(!visible(&spec, &f.ws.join(".crew/agents/a/identity.json")));
        assert!(!visible(&spec, &f.git.join("config")));
        assert!(visible(&spec, &p.agent));
        assert_eq!(spec.cwd, f.ws);
    }

    #[test]
    fn path_entries_into_protected_paths_or_above_earlier_mounts_are_dropped() {
        let mut f = fixture();
        symlink(f.home.join(".config/bin"), f.home.join("tools")).unwrap();
        f.host.path = std::env::join_paths([
            f.home.join(".config/bin"),
            f.home.join("tools"),
            f.home.join(".local/share"),
            PathBuf::from("relative/bin"),
            f.home.join(".local/bin"),
            f.home.join(".cargo/bin"),
        ])
        .unwrap();
        let built = f.worker(&Extra::default()).unwrap();
        assert_eq!(
            env(&built.spec, "PATH").unwrap(),
            format!(
                "{}:{}",
                f.home.join(".local/bin").display(),
                f.home.join(".cargo/bin").display()
            )
        );
        assert_eq!(built.warnings.len(), 4, "{:?}", built.warnings);
        assert!(built.warnings.iter().any(|w| w.contains("would hide")));
        assert!(!visible(&built.spec, &f.home.join(".config/secret")));
    }

    #[test]
    fn explicit_entries_that_fail_validation_are_errors() {
        let f = fixture();
        symlink(f.home.join(".ssh"), f.home.join("keys")).unwrap();
        for bad in [
            f.home.clone(),
            f.ws.join("src"),
            f.home.join("keys"),
            f.root.clone(),
        ] {
            let extra = Extra {
                read_only: vec![bad.clone()],
                ..Extra::default()
            };
            let error = f.worker(&extra).unwrap_err();
            assert!(
                error.contains("workspace.sandbox entry refused"),
                "{bad:?}: {error}"
            );
        }
        let extra = Extra {
            read_only: vec![f.home.join("data")],
            read_write: vec![f.home.join("scratch")],
        };
        let spec = f.worker(&extra).unwrap().spec;
        assert!(binds(&spec).contains(&(f.home.join("data"), false)));
        assert!(binds(&spec).contains(&(f.home.join("scratch"), true)));
    }

    #[test]
    fn sockets_and_unlistable_directories_in_bound_trees_are_hidden() {
        let f = fixture();
        let _s = UnixListener::bind(f.home.join(".rustup/agent.sock")).unwrap();
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(spec.mounts.contains(&Mount::Mask {
            path: f.home.join(".rustup/agent.sock")
        }));
        assert!(!visible(&spec, &f.home.join(".rustup/agent.sock")));
    }

    #[test]
    fn the_store_is_left_unscanned_only_when_landlock_refuses_outside_sockets() {
        let store = Path::new("/nix/store");
        assert!(!needs_socket_scan(store, store, 9));
        assert!(!needs_socket_scan(&store.join("abc-bash"), store, 10));
        assert!(needs_socket_scan(store, store, 8));
        assert!(needs_socket_scan(Path::new("/etc"), store, 9));
        let mut f = fixture();
        f.host.landlock_abi = 7;
        assert_eq!(
            f.worker(&Extra::default()).unwrap().spec.min_landlock_abi,
            6
        );
    }

    #[test]
    fn system_trees_must_be_root_owned_and_symlinks_are_recreated() {
        let mut f = fixture();
        f.host.system = vec!["/etc".into()];
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(binds(&spec).contains(&("/etc".into(), false)));
        symlink("/etc", f.root.join("sys-link")).unwrap();
        f.host.system = vec![f.root.join("sys-link")];
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(spec.mounts.contains(&Mount::Symlink {
            path: f.root.join("sys-link"),
            target: "/etc".into()
        }));
        // SAFETY: geteuid has no failure modes.
        if unsafe { libc::geteuid() } != 0 {
            mkdirs(&[&f.root.join("usr")]);
            f.host.system = vec![f.root.join("usr")];
            let error = f.worker(&Extra::default()).unwrap_err();
            assert!(error.contains("must be owned by root"), "{error}");
        }
    }

    #[test]
    fn resolv_conf_pointing_into_run_binds_only_the_target_file() {
        let mut f = fixture();
        let resolve = f.root.join("run/systemd/resolve");
        touch(&[&resolve.join("stub-resolv.conf"), &resolve.join("sentinel")]);
        let _s = UnixListener::bind(resolve.join("io.systemd.Resolve")).unwrap();
        mkdirs(&[&f.root.join("etc")]);
        symlink(
            "../run/systemd/resolve/stub-resolv.conf",
            f.root.join("etc/resolv.conf"),
        )
        .unwrap();
        f.host.resolv_conf = f.root.join("etc/resolv.conf");
        let spec = f.worker(&Extra::default()).unwrap().spec;
        let target = resolve.join("stub-resolv.conf");
        assert!(spec.mounts.contains(&Mount::Symlink {
            path: f.root.join("etc/resolv.conf"),
            target: "../run/systemd/resolve/stub-resolv.conf".into()
        }));
        assert!(binds(&spec).contains(&(target.clone(), false)));
        assert!(!visible(&spec, &resolve.join("sentinel")));
        assert!(!visible(&spec, &resolve.join("io.systemd.Resolve")));
        assert!(!binds(&spec).iter().any(|(p, _)| resolve.starts_with(p)));
    }

    #[test]
    fn resolv_conf_inside_a_bound_tree_still_gets_its_target_bound() {
        // In production /etc is bound before resolv.conf is looked at.
        let mut f = fixture();
        let resolve = f.root.join("run/systemd/resolve");
        touch(&[&resolve.join("stub-resolv.conf"), &resolve.join("sentinel")]);
        let link = f.home.join(".rustup/resolv.conf");
        symlink(resolve.join("stub-resolv.conf"), &link).unwrap();
        f.host.resolv_conf = link.clone();
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(binds(&spec).contains(&(resolve.join("stub-resolv.conf"), false)));
        let recreated = |m: &Mount| matches!(m, Mount::Symlink { path, .. } if path == &link);
        assert!(!spec.mounts.iter().any(recreated));
        assert!(!visible(&spec, &resolve.join("sentinel")));
    }

    #[test]
    fn non_canonical_or_misplaced_daemon_paths_are_refused() {
        let f = fixture();
        symlink(&f.ws, f.root.join("ws-link")).unwrap();
        let p = f.paths("a");
        let aliased = f.root.join("ws-link/.crew/worktrees/a");
        let role = Role::Worker {
            agent_dir: &p.agent,
            worktree: &aliased,
            admin: &p.admin,
            sidecar_dir: None,
        };
        let error = f.build(role, &Extra::default(), &[]).unwrap_err();
        assert!(error.contains("not canonical"), "{error}");
        let wrong_admin = f.ws.join(".crew/agents/a");
        let role = Role::Worker {
            agent_dir: &p.agent,
            worktree: &p.worktree,
            admin: &wrong_admin,
            sidecar_dir: None,
        };
        assert!(f.build(role, &Extra::default(), &[]).is_err());
        let outside = f.home.join("data");
        let role = Role::Worker {
            agent_dir: &p.agent,
            worktree: &p.worktree,
            admin: &p.admin,
            sidecar_dir: Some(&outside),
        };
        assert!(f.build(role, &Extra::default(), &[]).is_err());
    }

    #[test]
    fn a_planted_symlink_over_a_worktree_config_file_is_refused() {
        let f = fixture();
        let p = f.paths("a");
        std::fs::remove_file(p.admin.join("config.worktree")).unwrap();
        symlink(f.home.join(".gitconfig"), p.admin.join("config.worktree")).unwrap();
        assert!(f.worker(&Extra::default()).is_err());
    }

    #[test]
    fn a_toolchain_path_resolving_into_a_protected_path_is_left_out() {
        let mut f = fixture();
        mkdirs(&[&f.home.join(".config/rustup")]);
        symlink(f.home.join(".config/rustup"), f.home.join("rustup-link")).unwrap();
        f.host.rustup_home = f.home.join("rustup-link");
        let built = f.worker(&Extra::default()).unwrap();
        assert!(!visible(&built.spec, &f.home.join(".config/rustup")));
        assert_eq!(env(&built.spec, "RUSTUP_HOME"), None);
        assert!(built
            .warnings
            .iter()
            .any(|w| w.contains("toolchain path left out")));
    }

    #[test]
    fn install_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let scoped = root.join("lib/node_modules/@openai/codex");
        let plain = root.join("lib/node_modules/pi-agent");
        let native = root.join("share/claude/versions/2.1.0");
        touch(&[
            &scoped.join("bin/codex.js"),
            &plain.join("dist/cli.js"),
            &native,
        ]);
        assert_eq!(install_root(&scoped.join("bin/codex.js")).unwrap(), scoped);
        assert_eq!(install_root(&plain.join("dist/cli.js")).unwrap(), plain);
        assert_eq!(install_root(&native).unwrap(), native);
    }
}
