//! Building a launch [`Spec`] from the daemon's paths (design-b3 §3.2).
//!
//! A sandbox starts empty: a private `/tmp` (with `/tmp/run` as
//! `XDG_RUNTIME_DIR`), `$HOME` as an empty tmpfs, then only what is listed
//! here. Three profiles:
//! - a **worker** gets its worktree and the store data it needs to commit
//!   (git's, or jj's: notes/jj-design.md §sandbox), its agent directory
//!   (read-only, with `rw/` writable), the team's cargo cache, the daemon
//!   socket and its sidecar directory;
//! - the **orchestrator** gets the checkout read-only with `.crew`, `.git`
//!   and `.jj` masked, and the same agent mounts, but no worktree;
//! - a **helper** (git or jj on behalf of no agent) gets one worktree and
//!   the store data: no identity, socket, agent directory, credentials or
//!   network.
//!
//! Every profile gets the system trees, the toolchain leaves and the PATH
//! entries that pass validation, all read-only, and the crew binary: the
//! daemon's copy of it in `.crew/bin` (daemon::binary), bound by the
//! identity it was made with, or else a binary outside every protected path.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use super::landlock;
use super::pin::{identify, Identity, Kind};
use super::probe::UntrustedNixDaemon;
use super::scan::{scan_excluding, Findings};
use super::spec::{check_name, Mount, Spec};
use super::validate::{host_members, Protected};
use crate::vcs::Store;

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

/// The directories of a jj repository that jj writes as it works in a
/// workspace; everything else in it stays read-only.
const JJ_APPENDED: [&str; 4] = ["op_store", "op_heads", "index", "store/extra"];

/// Facts about the host every profile uses.
#[derive(Clone, Debug)]
pub struct Host {
    pub home: PathBuf,
    /// `PATH` as the daemon sees it.
    pub path: OsString,
    pub bwrap: PathBuf,
    pub crew_bin: PathBuf,
    /// `crew_bin` is the daemon's copy of its binary (daemon::binary), and
    /// this was its identity when it was made: it is bound as a path of the
    /// session, exactly that file.
    pub crew_pin: Option<Identity>,
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
            crew_pin: None,
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
    /// The repository's store: its common git directory (usually
    /// `<ws>/.git`) and, under jj, jj's repository.
    pub store: &'a Store,
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
        /// `<git_dir>/worktrees/<id>`: git only. A jj workspace keeps what
        /// says which repository it is in its own `.jj`.
        admin: Option<&'a Path>,
        sidecar_dir: Option<&'a Path>,
    },
    Orchestrator {
        agent_dir: &'a Path,
        sidecar_dir: Option<&'a Path>,
    },
    Helper {
        worktree: &'a Path,
        /// As for a worker: git only.
        admin: Option<&'a Path>,
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
    /// The agent CLI's program, or the jj an agent's version control and
    /// the helper run; its install root is bound.
    pub cli: Option<&'a Path>,
    /// Names of the runtime's launch variables (their values reach
    /// `crew sandbox-run` separately). Helpers get none.
    pub pass_env: &'a [String],
    /// The process runs on a PTY (otherwise it gets a new session).
    pub terminal: bool,
    /// The Nix daemon socket, for agents, when `workspace.sandbox.nix_daemon`
    /// is set and the daemon was found not to trust the operator.
    pub nix_daemon: Option<&'a UntrustedNixDaemon>,
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
        if host.landlock_abi < landlock::MIN_ABI {
            return Err(format!(
                "the kernel offers Landlock ABI {}, and sandboxes need ABI {}",
                host.landlock_abi,
                landlock::MIN_ABI
            ));
        }
        let s = &request.session;
        let (git_dir, repo_dir) = match s.store {
            Store::Git { git_dir } => (git_dir.as_path(), None),
            Store::Jj { git_dir, repo_dir } => (git_dir.as_path(), Some(repo_dir.as_path())),
        };
        let mut generated = vec![
            s.workspace,
            git_dir,
            s.runtime_dir,
            s.daemon_socket,
            s.cargo_home,
        ];
        generated.extend(repo_dir);
        let admin = match request.role {
            Role::Worker {
                agent_dir,
                worktree,
                admin,
                sidecar_dir,
            } => {
                generated.extend([agent_dir, worktree]);
                generated.extend(sidecar_dir);
                Some(admin)
            }
            Role::Orchestrator {
                agent_dir,
                sidecar_dir,
            } => {
                generated.push(agent_dir);
                generated.extend(sidecar_dir);
                None
            }
            Role::Helper { worktree, admin } => {
                generated.push(worktree);
                if !request.pass_env.is_empty() {
                    return Err("a helper sandbox gets no launch variables".into());
                }
                Some(admin)
            }
        };
        match (s.store, admin) {
            (Store::Git { .. }, Some(None)) => {
                return Err("a git worktree needs its admin directory".into())
            }
            (Store::Jj { .. }, Some(Some(_))) => {
                return Err("a jj workspace has no git admin directory".into())
            }
            (_, Some(Some(admin))) => generated.push(admin),
            _ => {}
        }
        for path in generated {
            canonical(path)?;
        }
        let mut protected = host.protected.clone();
        protected.add(s.workspace);
        protected.add(git_dir);
        if let Some(repo) = git_dir.parent() {
            protected.add(repo);
        }
        if let Some(repo) = repo_dir {
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
        let mut read_write = Vec::new();
        for (paths, writable) in [
            (&request.extra.read_only, false),
            (&request.extra.read_write, true),
        ] {
            for path in paths {
                let canonical = self
                    .protected
                    .validate(path)
                    .and_then(|canonical| {
                        self.bind_scanned(&canonical, writable, &[])
                            .map(|()| canonical)
                    })
                    .map_err(|e| format!("workspace.sandbox entry refused: {e}"))?;
                if writable {
                    read_write.push(canonical);
                }
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
                self.store(worktree, admin)?;
                self.agent(agent_dir, sidecar_dir)?
            }
            Role::Orchestrator {
                agent_dir,
                sidecar_dir,
            } => self.agent(agent_dir, sidecar_dir)?,
            Role::Helper { worktree, admin } => {
                self.store(worktree, admin)?;
                host.home.clone()
            }
        };
        self.no_writable_overlap(&read_write)?;
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
        let spec = Spec {
            bwrap: host.bwrap.clone(),
            crew_bin: canonical_file(&host.crew_bin)?,
            mounts: self.mounts,
            env,
            pass_env,
            cwd: cwd.to_path_buf(),
            network: agent,
            new_session: !agent || !request.terminal,
            min_landlock_abi: landlock::MIN_ABI,
            allow_unix: if agent {
                let nix = request.nix_daemon.and_then(|n| n.socket().parent());
                std::iter::once(session.runtime_dir)
                    .chain(nix)
                    .map(Path::to_path_buf)
                    .collect()
            } else {
                Vec::new()
            },
            done: None,
        };
        spec.check()?;
        Ok(Built {
            spec,
            warnings: self.warnings,
        })
    }

    /// Design-b3 §3.3 rule 6: an explicit `read_write` entry may not equal,
    /// lie inside or contain any read-only mount (system trees, toolchain
    /// leaves, PATH entries, the read-only git data, `<A>`). Otherwise a
    /// writable bind over part of the operator's toolchain or config would
    /// let the agent change what unsandboxed tools later run or load.
    fn no_writable_overlap(&self, read_write: &[PathBuf]) -> Result<(), String> {
        for rw in read_write {
            for mount in &self.mounts {
                let Mount::Bind {
                    path,
                    writable: false,
                    ..
                } = mount
                else {
                    continue;
                };
                if path.starts_with(rw) || rw.starts_with(path) {
                    return Err(format!(
                        "workspace.sandbox entry refused: read_write {} overlaps the read-only {}",
                        rw.display(),
                        path.display()
                    ));
                }
            }
        }
        Ok(())
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
        if canonical.starts_with(&self.host.nix_store) {
            // Not walked: the Landlock socket rule every launch demands
            // refuses every socket made outside the sandbox, and walking a
            // live store takes minutes.
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

    /// The orchestrator's view of the checkout: read-only, with `.crew`,
    /// `.git` and `.jj` (a project's own jj repository) masked.
    fn checkout_read_only(&mut self) -> Result<(), String> {
        let ws = self.request.session.workspace;
        let crew = ws.join(crew_core::paths::STATE_DIR_NAME);
        let dot_git = ws.join(".git");
        let dot_jj = ws.join(".jj");
        self.generated(ws, false)?;
        self.scan(ws, &[&crew, &dot_git, &dot_jj])?;
        for masked in [crew, dot_git, dot_jj] {
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
        match host.crew_pin {
            Some(id) => self.crew_pin(&host.crew_bin, id)?,
            None => {
                let crew_bin = canonical_file(&host.crew_bin)?;
                self.protected
                    .check(&crew_bin)
                    .map_err(|e| format!("the crew binary cannot be bound: {e}"))?;
                self.bind_scanned(&crew_bin, false, &[])?;
            }
        }
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

    /// The daemon's copy of its binary: a path of the session, not a
    /// toolchain path, bound read-only at its own path (beneath the masked
    /// `.crew` where there is one). It must be exactly the file the daemon
    /// made: a regular file directly in `<state>/bin`, the operator's, not
    /// writable by group or others, with the identity it had when it was
    /// made, which the mount is checked against too.
    fn crew_pin(&mut self, pin: &Path, id: Identity) -> Result<(), String> {
        let refused =
            |why: &str| format!("the crew binary cannot be bound: {} {why}", pin.display());
        let dir = self
            .request
            .session
            .workspace
            .join(crew_core::paths::STATE_DIR_NAME)
            .join(crate::daemon::binary::BIN_DIR);
        if pin.parent() != Some(dir.as_path()) {
            return Err(refused(&format!("is not in {}", dir.display())));
        }
        canonical(pin)?;
        let meta = std::fs::symlink_metadata(pin).map_err(|e| refused(&e.to_string()))?;
        let found = Identity {
            dev: meta.dev(),
            ino: meta.ino(),
            kind: Kind::File,
        };
        if !meta.file_type().is_file() || id.kind != Kind::File || found != id {
            return Err(refused("is not the copy the daemon made"));
        }
        if meta.uid() != crew_core::paths::uid() || meta.mode() & 0o022 != 0 {
            return Err(refused("may be written by someone else"));
        }
        self.mounts.push(Mount::Bind {
            path: pin.to_path_buf(),
            id,
            writable: false,
        });
        self.covered.push(pin.to_path_buf());
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

    /// The worktree, writable, and the store data it needs: git's, or jj's.
    /// Builder::new checked that a git worktree has its admin directory and
    /// a jj workspace none.
    fn store(&mut self, worktree: &Path, admin: Option<&Path>) -> Result<(), String> {
        match (self.request.session.store, admin) {
            (Store::Git { git_dir }, Some(admin)) => {
                self.git(git_dir, admin)?;
                self.generated(worktree, true)
            }
            (Store::Jj { git_dir, repo_dir }, None) => self.jj(git_dir, repo_dir, worktree),
            _ => Err("the worktree does not match the repository's store".into()),
        }
    }

    /// The common git directory read-only, with objects and refs writable
    /// (to commit).
    fn git_data(&mut self, git_dir: &Path) -> Result<(), String> {
        self.generated(git_dir, false)?;
        self.scan(git_dir, &[])?;
        for sub in ["objects", "refs", "logs", "packed-refs"] {
            let path = git_dir.join(sub);
            if std::fs::symlink_metadata(&path).is_ok() {
                self.generated(&path, true)?;
            }
        }
        Ok(())
    }

    /// The git data a worktree needs: the common git data, the worktree's
    /// own admin directory writable, and the three files that say which
    /// repository and which per-worktree config it uses read-only over it.
    fn git(&mut self, git_dir: &Path, admin: &Path) -> Result<(), String> {
        if admin.parent() != Some(&git_dir.join("worktrees")) {
            return Err(format!(
                "{} is not a worktree admin directory of {}",
                admin.display(),
                git_dir.display()
            ));
        }
        self.git_data(git_dir)?;
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

    /// What jj needs to work in the workspace `worktree`
    /// (notes/jj-design.md §sandbox): the common git data as for git; jj's
    /// repository read-only, with the directories jj appends to (operations,
    /// views, index, change ids) writable; and the workspace writable, with
    /// its `.jj` read-only over it and the working-copy state in that
    /// writable. Read-only: what names the repository (`store/git_target`,
    /// the workspace's `.jj/repo`) and where configuration would be planted
    /// (`config.toml`, `workspace-config.toml`), which jj would apply when it
    /// next runs outside a sandbox. jj's repository goes first: in a
    /// project's own jj repository that is not colocated, the git data lies
    /// inside it.
    fn jj(&mut self, git_dir: &Path, repo_dir: &Path, worktree: &Path) -> Result<(), String> {
        self.generated(repo_dir, false)?;
        self.scan(repo_dir, &[])?;
        for sub in JJ_APPENDED {
            self.generated(&repo_dir.join(sub), true)?;
        }
        self.git_data(git_dir)?;
        self.generated(worktree, true)?;
        let dot_jj = worktree.join(".jj");
        self.generated(&dot_jj, false)?;
        self.generated(&dot_jj.join("working_copy"), true)
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
        if let Some(nix) = self.request.nix_daemon {
            let socket = nix.socket();
            let id = identify(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
            if id.kind != Kind::Socket {
                return Err(format!("{} is not a socket", socket.display()));
            }
            self.mounts.push(Mount::Bind {
                path: socket.to_path_buf(),
                id,
                writable: true,
            });
        }
        // File toolboxes are daemon-produced, content-addressed snapshots.
        // They are shared between members but never writable from a session.
        let toolbox_store = session.workspace.join(".crew/toolbox/store");
        if std::fs::symlink_metadata(&toolbox_store).is_ok() {
            self.generated(&toolbox_store, false)?;
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
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;

    /// The daemon's copy of its binary, in the fixture's workspace.
    const PIN: &str = ".crew/bin/crew-0123456789abcdef";

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
        store: Store,
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
            &ws.join(PIN),
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
        std::fs::set_permissions(ws.join(PIN), std::fs::Permissions::from_mode(0o500)).unwrap();
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
            crew_bin: ws.join(PIN),
            crew_pin: Some(identify(&ws.join(PIN)).unwrap()),
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
            store: Store::Git {
                git_dir: git.clone(),
            },
            git,
            run,
            socket,
            cargo,
            host,
        }
    }

    /// `fixture()` as the jj backend lays it out: the repository in the
    /// handle `.crew/jj`, backed by `.git`, and each worktree a jj
    /// workspace naming it.
    fn jj_fixture() -> Fixture {
        let mut f = fixture();
        let repo = f.ws.join(".crew/jj/.jj/repo");
        for sub in JJ_APPENDED.iter().chain(&["workspace_store"]) {
            mkdirs(&[&repo.join(sub)]);
        }
        touch(&[&repo.join("store/git_target"), &repo.join("store/type")]);
        for id in ["a", "b", "_integration"] {
            let dot_jj = f.ws.join(".crew/worktrees").join(id).join(".jj");
            mkdirs(&[&dot_jj.join("working_copy")]);
            touch(&[&dot_jj.join("repo")]);
        }
        f.store = Store::Jj {
            git_dir: f.git.clone(),
            repo_dir: repo,
        };
        f
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
                store: &self.store,
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
                nix_daemon: None,
            };
            build(&self.host, &request)
        }

        /// Agent `a`'s profile; its worktree has an admin directory only
        /// under git.
        fn worker(&self, extra: &Extra) -> Result<Built, String> {
            let p = self.paths("a");
            let admin = matches!(self.store, Store::Git { .. }).then_some(p.admin.as_path());
            self.build(
                Role::Worker {
                    agent_dir: &p.agent,
                    worktree: &p.worktree,
                    admin,
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
                (f.ws.join(PIN), false),
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
            crew_core::paths::operator_token_path(&f.socket),
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
            // The dashboard server's files (notes/web-multi.md §multi-security).
            f.run.join("web.sock"),
            f.run.join("web.token"),
            f.run.join("web.json"),
            f.run.join("web.lock"),
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
        assert_eq!(spec.crew_bin, f.ws.join(PIN));
    }

    #[test]
    fn a_helper_has_no_identity_socket_agent_directory_or_network() {
        let f = fixture();
        let p = f.paths("_integration");
        let role = Role::Helper {
            worktree: &p.worktree,
            admin: Some(&p.admin),
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
    fn the_daemons_copy_of_crew_is_bound_beneath_the_masked_state_exactly_as_made() {
        let f = fixture();
        let pin = f.ws.join(PIN);
        let p = f.paths("orchestrator");
        mkdirs(&[&p.agent.join("rw")]);
        let role = Role::Orchestrator {
            agent_dir: &p.agent,
            sidecar_dir: None,
        };
        let spec = f.build(role, &Extra::default(), &[]).unwrap().spec;
        assert!(visible(&spec, &pin), "the orchestrator cannot run crew");
        assert!(!visible(&spec, &f.ws.join(".crew/crew.db")));
        let bound = spec.mounts.iter().find(|m| m.path() == pin).unwrap();
        assert!(
            matches!(bound, Mount::Bind { id, writable: false, .. } if Some(*id) == f.host.crew_pin),
            "{bound:?}"
        );
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(visible(&spec, &pin), "a worker cannot run crew");
        assert!(!visible(&spec, &f.ws.join(".crew/crew.db")));

        // Another file under its name: not the copy that was made.
        let planted = f.ws.join(".crew/bin/planted");
        touch(&[&planted]);
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::rename(&planted, &pin).unwrap();
        let error = f.worker(&Extra::default()).unwrap_err();
        assert!(error.contains("is not the copy the daemon made"), "{error}");

        // The copy as made, but writable by others.
        let mut f = fixture();
        let pin = f.ws.join(PIN);
        std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o520)).unwrap();
        let error = f.worker(&Extra::default()).unwrap_err();
        assert!(error.contains("may be written by someone else"), "{error}");

        // Anywhere but the daemon's directory, it is not a copy of the daemon's.
        let elsewhere = f.ws.join("target/release/crew");
        f.host.crew_bin = elsewhere.clone();
        f.host.crew_pin = Some(identify(&elsewhere).unwrap());
        let error = f.worker(&Extra::default()).unwrap_err();
        assert!(error.contains("is not in"), "{error}");
    }

    #[test]
    fn a_crew_binary_that_is_no_copy_is_a_toolchain_path_with_no_exception_for_target() {
        let mut f = fixture();
        f.host.crew_pin = None;
        f.host.crew_bin = f.ws.join("target/release/crew");
        let error = f.worker(&Extra::default()).unwrap_err();
        assert!(
            error.starts_with("the crew binary cannot be bound"),
            "{error}"
        );
        // Outside every protected path, it is bound as it is.
        let installed = f.root.join("opt/crew");
        touch(&[&installed]);
        f.host.crew_bin = installed.clone();
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(binds(&spec).contains(&(installed.clone(), false)));
        assert_eq!(spec.crew_bin, installed);
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
    fn a_read_write_entry_may_not_overlap_a_read_only_mount() {
        let f = fixture();
        touch(&[
            &f.home.join(".rustup/operator-tool"),
            &f.home.join(".cargo/bin/rustc"),
        ]);
        symlink(f.home.join(".rustup"), f.home.join("rustup-alias")).unwrap();
        symlink(f.home.join(".gitconfig"), f.home.join("gitconfig-alias")).unwrap();
        for bad in [
            // inside a toolchain leaf, a PATH entry, the CLI's install root
            f.home.join(".rustup/operator-tool"),
            f.home.join(".rustup/toolchains"),
            f.home.join(".cargo/bin/rustc"),
            f.home.join(".local/share/claude/versions/2.1.0"),
            // equal to a leaf, also through an alias
            f.home.join(".gitconfig"),
            f.home.join("gitconfig-alias"),
            f.home.join("rustup-alias/operator-tool"),
            // containing one
            f.home.join(".local/share/claude"),
        ] {
            let extra = Extra {
                read_write: vec![bad.clone()],
                ..Extra::default()
            };
            let error = f.worker(&extra).unwrap_err();
            assert!(
                error.contains("workspace.sandbox entry refused"),
                "{bad:?}: {error}"
            );
        }
        // Read-only entries inside a read-only mount stay allowed.
        let extra = Extra {
            read_only: vec![f.home.join(".rustup/operator-tool")],
            read_write: vec![f.home.join("scratch")],
        };
        let spec = f.worker(&extra).unwrap().spec;
        assert!(binds(&spec).contains(&(f.home.join("scratch"), true)));
        assert!(!binds(&spec)
            .iter()
            .any(|(p, w)| *w && p.starts_with(f.home.join(".rustup"))));
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
    fn every_spec_demands_landlock_abi_9_and_an_older_host_gets_none() {
        let mut f = fixture();
        assert_eq!(
            f.worker(&Extra::default()).unwrap().spec.min_landlock_abi,
            9
        );
        for abi in [6, 8] {
            f.host.landlock_abi = abi;
            let error = f.worker(&Extra::default()).unwrap_err();
            assert!(error.contains(&format!("Landlock ABI {abi},")), "{error}");
        }
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
            admin: Some(&p.admin),
            sidecar_dir: None,
        };
        let error = f.build(role, &Extra::default(), &[]).unwrap_err();
        assert!(error.contains("not canonical"), "{error}");
        let wrong_admin = f.ws.join(".crew/agents/a");
        let role = Role::Worker {
            agent_dir: &p.agent,
            worktree: &p.worktree,
            admin: Some(&wrong_admin),
            sidecar_dir: None,
        };
        assert!(f.build(role, &Extra::default(), &[]).is_err());
        let outside = f.home.join("data");
        let role = Role::Worker {
            agent_dir: &p.agent,
            worktree: &p.worktree,
            admin: Some(&p.admin),
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
    fn an_untrusted_nix_daemon_socket_is_bound_exactly_for_agents_only() {
        let f = fixture();
        let nix_dir = f.root.join("nix/var/nix/daemon-socket");
        mkdirs(&[&nix_dir]);
        let _nix = UnixListener::bind(nix_dir.join("socket")).unwrap();
        let verified = UntrustedNixDaemon::verified_for_test(&nix_dir.join("socket"));
        let p = f.paths("a");
        let request = Request {
            session: f.session(),
            role: Role::Worker {
                agent_dir: &p.agent,
                worktree: &p.worktree,
                admin: Some(&p.admin),
                sidecar_dir: None,
            },
            extra: &Extra::default(),
            cli: None,
            pass_env: &[],
            terminal: true,
            nix_daemon: Some(&verified),
        };
        let spec = build(&f.host, &request).unwrap().spec;
        assert!(binds(&spec).contains(&(nix_dir.join("socket"), true)));
        assert!(!binds(&spec).iter().any(|(p, _)| p == &nix_dir));
        assert_eq!(spec.allow_unix, vec![f.run.clone(), nix_dir.clone()]);
        let helper = f.paths("_integration");
        let request = Request {
            role: Role::Helper {
                worktree: &helper.worktree,
                admin: Some(&helper.admin),
            },
            ..request
        };
        let spec = build(&f.host, &request).unwrap().spec;
        assert!(!visible(&spec, &nix_dir.join("socket")));
        assert!(spec.allow_unix.is_empty());
    }

    #[test]
    fn other_sessions_cargo_caches_are_never_visible_or_mountable() {
        let f = fixture();
        let other = f.root.join("cache/crew/4567/cargo");
        mkdirs(&[&other]);
        let spec = f.worker(&Extra::default()).unwrap().spec;
        assert!(
            visible(&spec, &f.cargo),
            "the team's cargo cache is mounted"
        );
        assert!(!visible(&spec, &other));
        for ancestor in [f.root.join("cache"), f.root.join("cache/crew")] {
            let extra = Extra {
                read_write: vec![ancestor.clone()],
                ..Extra::default()
            };
            assert!(f.worker(&extra).is_err(), "{ancestor:?} was accepted");
        }
    }

    /// The binds under `root`, in mount order.
    fn binds_under(spec: &Spec, root: &Path) -> Vec<(PathBuf, bool)> {
        binds(spec)
            .into_iter()
            .filter(|(path, _)| path.starts_with(root))
            .collect()
    }

    /// The mount table of notes/jj-design.md §sandbox, in order: jj's
    /// repository read-only with what jj appends to writable, the git data
    /// as for git, the workspace writable with its `.jj` read-only and the
    /// working-copy state writable.
    fn jj_store_table(f: &Fixture, worktree: &Path) -> Vec<(PathBuf, bool)> {
        let repo = f.ws.join(".crew/jj/.jj/repo");
        vec![
            (repo.clone(), false),
            (repo.join("op_store"), true),
            (repo.join("op_heads"), true),
            (repo.join("index"), true),
            (repo.join("store/extra"), true),
            (f.git.clone(), false),
            (f.git.join("objects"), true),
            (f.git.join("refs"), true),
            (f.git.join("logs"), true),
            (f.git.join("packed-refs"), true),
            (worktree.to_path_buf(), true),
            (worktree.join(".jj"), false),
            (worktree.join(".jj/working_copy"), true),
        ]
    }

    #[test]
    fn a_jj_worker_gets_the_store_and_its_workspace_and_no_git_admin_directory() {
        let f = jj_fixture();
        let built = f.worker(&Extra::default()).unwrap();
        let p = f.paths("a");
        let mut expected = vec![(f.ws.join(PIN), false)];
        expected.extend(jj_store_table(&f, &p.worktree));
        expected.extend([(p.agent.clone(), false), (p.agent.join("rw"), true)]);
        assert_eq!(binds_under(&built.spec, &f.ws), expected);
        assert!(!binds(&built.spec)
            .iter()
            .any(|(path, _)| path.starts_with(f.git.join("worktrees"))));
        assert!(!visible(&built.spec, &f.ws.join(".crew/worktrees/b")));
        assert!(!visible(
            &built.spec,
            &f.ws.join(".crew/jj/.jj/working_copy")
        ));
        assert_eq!(built.spec.cwd, p.worktree);
        assert!(built.warnings.is_empty(), "{:?}", built.warnings);
    }

    #[test]
    fn a_jj_helper_gets_the_same_store_table_and_nothing_of_an_agent() {
        let f = jj_fixture();
        let p = f.paths("_integration");
        let role = Role::Helper {
            worktree: &p.worktree,
            admin: None,
        };
        let jj = f.home.join(".cargo/bin/jj");
        touch(&[&jj]);
        let request = Request {
            session: f.session(),
            role,
            extra: &Extra::default(),
            cli: Some(&jj),
            pass_env: &[],
            terminal: false,
            nix_daemon: None,
        };
        let spec = build(&f.host, &request).unwrap().spec;
        let mut expected = vec![(f.ws.join(PIN), false)];
        expected.extend(jj_store_table(&f, &p.worktree));
        assert_eq!(binds_under(&spec, &f.ws), expected);
        assert!(visible(&spec, &jj), "the jj it runs");
        assert!(!spec.network && spec.new_session && spec.pass_env.is_empty());
        assert!(!visible(&spec, &f.ws.join(".crew/agents")));
    }

    #[test]
    fn the_admin_directory_goes_with_git_and_never_with_jj() {
        let git = fixture();
        let p = git.paths("a");
        let role = Role::Helper {
            worktree: &p.worktree,
            admin: None,
        };
        let error = git.build(role, &Extra::default(), &[]).unwrap_err();
        assert!(error.contains("needs its admin directory"), "{error}");
        let jj = jj_fixture();
        let p = jj.paths("a");
        let role = Role::Helper {
            worktree: &p.worktree,
            admin: Some(&p.admin),
        };
        let error = jj.build(role, &Extra::default(), &[]).unwrap_err();
        assert!(error.contains("no git admin directory"), "{error}");
    }

    #[test]
    fn a_planted_link_in_a_jj_workspace_is_refused() {
        // `.jj` or its working-copy state replaced by a link: the profile
        // would bind what it points at.
        for planted in [".jj", ".jj/working_copy"] {
            let f = jj_fixture();
            let p = f.paths("a");
            let at = p.worktree.join(planted);
            std::fs::remove_dir_all(&at).unwrap();
            symlink(f.home.join("data"), &at).unwrap();
            let error = f.worker(&Extra::default()).unwrap_err();
            assert!(error.contains("not canonical"), "{planted}: {error}");
        }
        // A workspace without its `.jj` is no jj workspace.
        let f = jj_fixture();
        std::fs::remove_dir_all(f.paths("a").worktree.join(".jj")).unwrap();
        assert!(f.worker(&Extra::default()).is_err());
    }

    #[test]
    fn the_projects_own_jj_repository_is_bound_with_its_git_data_inside() {
        // The native layout, not colocated: the git data lies inside jj's
        // repository, whose read-only bind must come first.
        let mut f = fixture();
        let repo = f.ws.join(".jj/repo");
        let git = repo.join("store/git");
        for sub in JJ_APPENDED {
            mkdirs(&[&repo.join(sub)]);
        }
        mkdirs(&[&git.join("objects"), &git.join("refs")]);
        let worktree = f.paths("a").worktree;
        mkdirs(&[&worktree.join(".jj/working_copy")]);
        f.store = Store::Jj {
            git_dir: git.clone(),
            repo_dir: repo.clone(),
        };
        let spec = f.worker(&Extra::default()).unwrap().spec;
        let store: Vec<_> = binds_under(&spec, &f.ws.join(".jj"));
        assert_eq!(
            store,
            [
                (repo.clone(), false),
                (repo.join("op_store"), true),
                (repo.join("op_heads"), true),
                (repo.join("index"), true),
                (repo.join("store/extra"), true),
                (git.clone(), false),
                (git.join("objects"), true),
                (git.join("refs"), true),
            ]
        );
        assert!(binds(&spec).contains(&(worktree.join(".jj/working_copy"), true)));
    }

    #[test]
    fn the_orchestrator_does_not_see_a_projects_own_jj_repository() {
        let mut f = jj_fixture();
        mkdirs(&[&f.ws.join(".jj/repo")]);
        touch(&[&f.ws.join(".jj/repo/config.toml")]);
        f.store = Store::Jj {
            git_dir: f.git.clone(),
            repo_dir: f.ws.join(".jj/repo"),
        };
        let p = f.paths("orchestrator");
        mkdirs(&[&p.agent.join("rw")]);
        let role = Role::Orchestrator {
            agent_dir: &p.agent,
            sidecar_dir: None,
        };
        let spec = f.build(role, &Extra::default(), &[]).unwrap().spec;
        assert!(visible(&spec, &f.ws.join("src")));
        assert!(spec.mounts.contains(&Mount::Tmpfs {
            path: f.ws.join(".jj")
        }));
        assert!(!visible(&spec, &f.ws.join(".jj/repo/config.toml")));
        assert!(!visible(&spec, &f.ws.join(".crew/jj")));
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
