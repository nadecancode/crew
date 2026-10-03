//! Sandboxed launches, from the daemon's side (design-b3.md §3.2, §3.6): the
//! profile an agent runs in, built only from paths the daemon provisioned.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::config::{SandboxConfig, SandboxMode};
use crew_core::{paths, Agent};

use super::Daemon;
use crate::agents::LaunchSpec;
use crate::git;
use crate::persistence::NewEvent;
use crate::sandbox::probe;
use crate::sandbox::profile::{self, Built, Extra, Host, Request, Role, Session};
use crate::sandbox::spec::Spec;

/// The daemon's paths that every profile is built from.
struct Layout {
    workspace: PathBuf,
    git_dir: PathBuf,
    runtime_dir: PathBuf,
    socket: PathBuf,
    /// `<C>/cargo`: the team's `CARGO_HOME`, never the operator's.
    cargo_home: PathBuf,
}

impl Layout {
    /// `<C>` is `<cache_home>/crew/<hash>`, named after the socket, and its
    /// `cargo` directory is made if it is missing.
    fn new(workspace: &Path, socket: &Path, cache_home: &Path) -> Result<Self> {
        let git_dir = workspace.join(".git");
        if !std::fs::symlink_metadata(&git_dir).is_ok_and(|m| m.is_dir()) {
            bail!(
                "{} is not a directory: sandboxed agents need the workspace to be the main \
checkout of its repository",
                git_dir.display()
            );
        }
        let (Some(runtime_dir), Some(hash)) = (socket.parent(), socket.file_stem()) else {
            bail!("the daemon socket {} has no directory", socket.display());
        };
        let cargo_home = cache_home.join("crew").join(hash).join("cargo");
        paths::ensure_private_dir(&cargo_home)?;
        Ok(Self {
            workspace: workspace.to_path_buf(),
            git_dir,
            runtime_dir: runtime_dir.to_path_buf(),
            socket: socket.to_path_buf(),
            cargo_home: cargo_home
                .canonicalize()
                .with_context(|| format!("resolving {}", cargo_home.display()))?,
        })
    }

    fn session(&self) -> Session<'_> {
        Session {
            workspace: &self.workspace,
            git_dir: &self.git_dir,
            runtime_dir: &self.runtime_dir,
            daemon_socket: &self.socket,
            cargo_home: &self.cargo_home,
        }
    }
}

/// Where an agent stands, as far as its profile goes.
struct Placement<'a> {
    /// `<A>`.
    agent_dir: &'a Path,
    workdir: &'a Path,
    orchestrator: bool,
    /// The team works in worktrees (otherwise its agents share the checkout).
    worktrees: bool,
}

/// The profile of an agent's terminal session: the orchestrator sees the
/// checkout read-only, a worker gets its own worktree. Agents sharing the
/// checkout cannot be confined to anything of their own.
fn terminal_profile(
    host: &Host,
    layout: &Layout,
    at: &Placement,
    config: &SandboxConfig,
    launch: &LaunchSpec,
    cli: Option<&Path>,
) -> Result<Built> {
    let sidecar_dir = launch.sidecar.as_ref().and_then(|s| s.socket.parent());
    let admin;
    let role = if at.orchestrator {
        Role::Orchestrator {
            agent_dir: at.agent_dir,
            sidecar_dir,
        }
    } else if at.worktrees {
        admin = git::worktree_admin_dir(&layout.git_dir, at.workdir)
            .ok_or_else(|| anyhow!("{} is not a worktree crew made", at.workdir.display()))?;
        Role::Worker {
            agent_dir: at.agent_dir,
            worktree: at.workdir,
            admin: &admin,
            sidecar_dir,
        }
    } else {
        bail!(
            "agents sharing the checkout cannot be sandboxed; use workspace.isolation: worktree, \
or set workspace.sandbox.mode: off"
        );
    };
    let extra = Extra {
        read_only: config.read_only.clone(),
        read_write: config.read_write.clone(),
    };
    let pass_env: Vec<String> = launch.env.iter().map(|(name, _)| name.clone()).collect();
    profile::build(
        host,
        &Request {
            session: layout.session(),
            role,
            extra: &extra,
            cli,
            pass_env: &pass_env,
            terminal: true,
            nix_daemon: None,
        },
    )
    .map_err(|e| anyhow!("{} cannot be sandboxed: {e}", launch.agent_id))
}

/// `$XDG_CACHE_HOME`, else `~/.cache`.
fn cache_home() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(var("HOME").unwrap_or_else(|| "/".into())).join(".cache"))
}

/// Where `program` is, as the daemon's `PATH` finds it.
fn find_program(program: &str, path: Option<&OsString>) -> Option<PathBuf> {
    if program.contains('/') {
        return Some(PathBuf::from(program));
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

impl Daemon {
    /// The sandbox an agent's terminal session runs in: none unless the
    /// team requires sandboxes, and then the agent does not start without
    /// one. The host is probed at every launch.
    pub(crate) async fn terminal_sandbox(
        &self,
        agent: &Agent,
        launch: &LaunchSpec,
        backend: &str,
    ) -> Result<Option<Spec>> {
        let (config, worktrees) = {
            let model = self.model();
            let Some(team) = &model.session.team else {
                return Ok(None);
            };
            (
                team.workspace.sandbox.clone(),
                model.session.isolation == "worktree",
            )
        };
        if config.mode == SandboxMode::Off {
            return Ok(None);
        }
        if backend != "pty" {
            bail!(
                "the {backend} backend cannot start sandboxed agents; use backend: pty, or set \
workspace.sandbox.mode: off"
            );
        }
        let (workspace, socket, crew_bin) = (
            self.workspace.clone(),
            self.socket.clone(),
            self.crew_bin.clone(),
        );
        let agent_dir = self.agent_dir(&agent.id);
        let workdir = PathBuf::from(&agent.workdir);
        let orchestrator = agent.orchestrator;
        let launch = launch.clone();
        let built = tokio::task::spawn_blocking(move || -> Result<Built> {
            let path = std::env::var_os("PATH");
            let available = probe::probe(&crew_bin, path.as_ref())
                .map_err(|e| anyhow!("agents cannot be sandboxed on this host: {e}"))?;
            let host = Host::detect(&crew_bin, &available.bwrap, available.landlock_abi);
            let layout = Layout::new(&workspace, &socket, &cache_home())?;
            // The crew binary is bound anyway; any other program is a CLI,
            // and its install directory is bound.
            let crew_bin = crew_bin.canonicalize().ok();
            let cli = find_program(&launch.program, path.as_ref())
                .filter(|p| p.canonicalize().ok() != crew_bin);
            let at = Placement {
                agent_dir: &agent_dir,
                workdir: &workdir,
                orchestrator,
                worktrees,
            };
            terminal_profile(&host, &layout, &at, &config, &launch, cli.as_deref())
        })
        .await??;
        for warning in &built.warnings {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "sandbox.warning",
                    format!("{}: {warning}", agent.id),
                )
                .agent(&agent.id)
                .warn(),
            );
        }
        Ok(Some(built.spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::landlock;
    use crate::sandbox::spec::Mount;
    use crew_core::RuntimeKind;

    /// A workspace the way the daemon leaves it before a launch: a
    /// repository with a worktree crew made for `a`, the agents'
    /// directories, the bound socket and a cache directory.
    struct Fixture {
        _dir: tempfile::TempDir,
        _listener: std::os::unix::net::UnixListener,
        root: PathBuf,
        ws: PathBuf,
        worktree: PathBuf,
        socket: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let ws = root.join("ws");
        git::init_repo(&ws).unwrap();
        let (worktree, _) =
            git::add_worktree(&ws, &ws.join(".crew/worktrees"), "a", "main").unwrap();
        for id in ["a", "orchestrator"] {
            std::fs::create_dir_all(ws.join(".crew/agents").join(id).join("rw/home")).unwrap();
        }
        std::fs::create_dir(root.join("run")).unwrap();
        let socket = root.join("run/0123.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        Fixture {
            _dir: dir,
            _listener: listener,
            ws,
            worktree: worktree.path,
            socket,
            root,
        }
    }

    impl Fixture {
        fn profile(&self, id: &str, orchestrator: bool, worktrees: bool) -> Result<Spec> {
            let crew_bin = std::env::current_exe().unwrap();
            let host = Host::detect(&crew_bin, Path::new("/bwrap"), landlock::MIN_ABI);
            let layout = Layout::new(&self.ws, &self.socket, &self.root.join("cache"))?;
            let agent_dir = self.ws.join(".crew/agents").join(id);
            let at = Placement {
                agent_dir: &agent_dir,
                workdir: if orchestrator {
                    &self.ws
                } else {
                    &self.worktree
                },
                orchestrator,
                worktrees,
            };
            let launch = LaunchSpec {
                agent_id: id.into(),
                kind: RuntimeKind::Shell,
                program: "sh".into(),
                args: Vec::new(),
                env: vec![("CREW_IDENTITY".into(), "identity.json".into())],
                cwd: self.ws.clone(),
                prompt: None,
                prompt_as_arg: false,
                sidecar: None,
                sandbox: None,
            };
            let config = SandboxConfig {
                mode: SandboxMode::Required,
                ..SandboxConfig::default()
            };
            terminal_profile(&host, &layout, &at, &config, &launch, None).map(|built| built.spec)
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

    fn env<'a>(spec: &'a Spec, name: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_workers_and_the_orchestrators_profiles_come_from_the_daemons_paths() {
        let f = fixture();
        let agent_dir = f.ws.join(".crew/agents/a");
        let cargo = f.root.join("cache/crew/0123/cargo");
        let admin = f.ws.join(".git/worktrees/a");

        let worker = f.profile("a", false, true).unwrap();
        assert_eq!(worker.cwd, f.worktree);
        assert_eq!(env(&worker, "HOME"), agent_dir.join("rw/home").to_str());
        assert_eq!(env(&worker, "CARGO_HOME"), cargo.to_str());
        assert!(cargo.is_dir(), "<C>/cargo was not made");
        assert_eq!(worker.pass_env, ["CREW_IDENTITY"]);
        let worker_binds = binds(&worker);
        for expected in [
            (agent_dir.clone(), false),
            (agent_dir.join("rw"), true),
            (cargo.clone(), true),
            (f.worktree.clone(), true),
            (f.socket.clone(), true),
            (admin.clone(), true),
            (admin.join("config.worktree"), false),
            (f.ws.join(".git"), false),
        ] {
            assert!(
                worker_binds.contains(&expected),
                "{expected:?} missing: {worker_binds:#?}"
            );
        }
        assert!(
            !worker_binds.iter().any(|(p, _)| p == &f.ws),
            "a worker sees the checkout"
        );

        let orchestrator = f.profile("orchestrator", true, true).unwrap();
        assert_eq!(orchestrator.cwd, f.ws);
        assert!(binds(&orchestrator).contains(&(f.ws.clone(), false)));
        assert!(orchestrator.mounts.contains(&Mount::Tmpfs {
            path: f.ws.join(".crew")
        }));

        let shared = f.profile("a", false, false).unwrap_err().to_string();
        assert!(shared.contains("workspace.sandbox.mode: off"), "{shared}");
    }
}
