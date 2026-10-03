//! Sandboxed launches, from the daemon's side (design-b3.md §3.2, §3.6): the
//! profile an agent runs in, built only from paths the daemon provisioned.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::config::{SandboxConfig, SandboxMode, TeamConfig};
use crew_core::{paths, Agent};

use super::Daemon;
use crate::agents::LaunchSpec;
use crate::git;
use crate::git::launch::{Launcher, SandboxLauncher};
use crate::persistence::NewEvent;
use crate::sandbox::probe::UntrustedNixDaemon;
use crate::sandbox::profile::{self, Built, Extra, Host, Request, Role, Session};
use crate::sandbox::spec::{launcher_env, Spec};
use crate::sandbox::{probe, run};

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

/// What runs in an agent's sandbox: its terminal session, or a helper of
/// the session (the sidecar, a queue command).
pub(crate) struct Run {
    pub agent_id: String,
    pub program: String,
    /// What it is started with; passed into the sandbox by name.
    pub env: Vec<(String, String)>,
    /// The sidecar's socket directory, mounted writable.
    pub sidecar_dir: Option<PathBuf>,
    /// The backend whose terminal it runs on. None: no terminal (a new
    /// session, stdin /dev/null).
    pub terminal: Option<&'static str>,
}

impl Run {
    /// The agent's terminal session, started by `backend`.
    pub fn session(launch: &LaunchSpec, backend: &'static str) -> Self {
        Self {
            agent_id: launch.agent_id.clone(),
            program: launch.program.clone(),
            env: launch.env.clone(),
            sidecar_dir: launch
                .sidecar
                .as_ref()
                .and_then(|s| s.socket.parent())
                .map(Path::to_path_buf),
            terminal: Some(backend),
        }
    }

    /// A helper of the agent's session: no terminal, the sidecar's socket
    /// directory mounted.
    pub fn helper(agent_id: &str, argv0: &str, env: &[(String, String)], socket: &Path) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            program: argv0.to_string(),
            env: env.to_vec(),
            sidecar_dir: socket.parent().map(Path::to_path_buf),
            terminal: None,
        }
    }
}

impl Run {
    /// The agent's own git: no terminal, and of its variables only its
    /// identity (`<A>/identity.json`).
    pub fn git(agent_id: &str, agent_dir: &Path) -> Self {
        let identity = agent_dir.join("identity.json");
        Self {
            agent_id: agent_id.to_string(),
            program: "git".to_string(),
            env: vec![(
                paths::IDENTITY_ENV.to_string(),
                identity.to_string_lossy().into_owned(),
            )],
            sidecar_dir: None,
            terminal: None,
        }
    }
}

/// `argv` as a command. In a sandbox it runs behind `crew sandbox-run` with
/// exactly the launcher's environment (`env` is passed in by name);
/// otherwise `env` is added to the daemon's.
pub(crate) fn command(
    sandbox: Option<&Spec>,
    argv: &[String],
    env: &[(String, String)],
) -> Result<tokio::process::Command> {
    let mut full: Vec<OsString> = match sandbox {
        Some(spec) => run::prefix(spec).map_err(|e| anyhow!(e))?,
        None => Vec::new(),
    };
    full.extend(argv.iter().map(OsString::from));
    let (program, args) = full
        .split_first()
        .ok_or_else(|| anyhow!("an empty command line"))?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    match sandbox {
        Some(spec) => {
            cmd.env_clear()
                .envs(launcher_env(spec, std::env::vars_os(), env));
        }
        None => {
            cmd.envs(env.iter().map(|(name, value)| (name, value)));
        }
    }
    Ok(cmd)
}

/// The profile of something an agent runs: the orchestrator sees the
/// checkout read-only, a worker gets its own worktree. Agents sharing the
/// checkout cannot be confined to anything of their own.
fn agent_profile(
    host: &Host,
    layout: &Layout,
    at: &Placement,
    config: &SandboxConfig,
    run: &Run,
    cli: Option<&Path>,
    nix_daemon: Option<&UntrustedNixDaemon>,
) -> Result<Built> {
    let sidecar_dir = run.sidecar_dir.as_deref();
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
    let extra = extra(config);
    let pass_env: Vec<String> = run.env.iter().map(|(name, _)| name.clone()).collect();
    profile::build(
        host,
        &Request {
            session: layout.session(),
            role,
            extra: &extra,
            cli,
            pass_env: &pass_env,
            terminal: run.terminal.is_some(),
            nix_daemon,
        },
    )
    .map_err(|e| anyhow!("{} cannot be sandboxed: {e}", run.agent_id))
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

/// What every refusal offers besides its own fix.
const OFF: &str = "or set workspace.sandbox.mode: off to run the agents unsandboxed";

/// What decides whether a team that requires sandboxes can start here.
struct Readiness {
    backend: &'static str,
    worktrees: bool,
    /// The host probe.
    host: Result<(), probe::Unavailable>,
    /// The Nix daemon's answer, when `workspace.sandbox.nix_daemon` is set.
    nix: Option<Result<(), String>>,
}

impl Readiness {
    /// Why the team cannot start, with what would fix it (design-b3.md
    /// §3.2 "Nix", §3.6; operator decisions 1, 2, 6, 8 and 10).
    fn refusal(&self) -> Option<String> {
        if self.backend != "pty" {
            return Some(format!(
                "the {} backend starts its own programs, so crew cannot sandbox them; use \
backend: pty, {OFF}",
                self.backend
            ));
        }
        if !self.worktrees {
            return Some(format!(
                "agents sharing the checkout cannot be sandboxed (they would write your files); \
make the workspace a git repository with a commit (or set workspace.init: true) and use \
workspace.isolation: worktree, {OFF}"
            ));
        }
        if let Err(e) = &self.host {
            // The probe's fix may already offer the opt-out (Landlock's does).
            let off = if e.fix.contains("workspace.sandbox.mode: off") {
                String::new()
            } else {
                format!(", {OFF}")
            };
            return Some(format!(
                "agents cannot be sandboxed on this host: {}; {}{off}",
                e.cause, e.fix
            ));
        }
        if let Some(Err(e)) = &self.nix {
            return Some(format!(
                "workspace.sandbox.nix_daemon is set and the Nix daemon cannot be given to agents \
({e}); set workspace.sandbox.nix_daemon: false, {OFF}"
            ));
        }
        None
    }
}

/// The Nix daemon's socket, once the daemon has said it does not trust the
/// operator (a trusted user's settings make it run code as root). Blocking.
fn untrusted_nix_daemon(path: Option<&OsString>) -> Result<UntrustedNixDaemon, String> {
    let nix = find_program("nix", path).ok_or("`nix` is not on PATH")?;
    probe::nix_daemon(&nix)
}

/// The host, probed now, and the daemon's layout. Blocking.
fn host_and_layout(crew_bin: &Path, workspace: &Path, socket: &Path) -> Result<(Host, Layout)> {
    let path = std::env::var_os("PATH");
    let available = probe::probe(crew_bin, path.as_ref())
        .map_err(|e| anyhow!("agents cannot be sandboxed on this host: {e}"))?;
    let host = Host::detect(crew_bin, &available.bwrap, available.landlock_abi);
    Ok((host, Layout::new(workspace, socket, &cache_home())?))
}

/// What the team's configuration adds to every sandbox.
fn extra(config: &SandboxConfig) -> Extra {
    Extra {
        read_only: config.read_only.clone(),
        read_write: config.read_write.clone(),
    }
}

/// A launcher made for one worktree.
type HelperFor<'a> = &'a dyn Fn(&Path) -> Result<Arc<dyn Launcher>>;

/// Make (or reuse) a member's worktree. Its files are put in place through
/// `helper`'s launcher when there is one: smudge filters and the branch's
/// own configuration can run then (design-b3.md §3.2, helper profiles).
/// Making it runs no git inside it.
fn provision_worktree(
    repo: &Path,
    worktrees_dir: &Path,
    id: &str,
    base: &str,
    helper: Option<HelperFor>,
) -> Result<git::Worktree> {
    let Some(helper) = helper else {
        return git::ensure_worktree(repo, worktrees_dir, id, base);
    };
    let (worktree, needs_population) = git::add_worktree(repo, worktrees_dir, id, base)?;
    if needs_population {
        let launcher = helper(&worktree.path)?;
        git::launch::with_launcher(launcher, || git::populate_worktree(&worktree))?;
    }
    Ok(worktree)
}

/// Integration on its own worktree, made outside any launcher; it is filled
/// and every agent branch merged into it through `helper`'s launcher when
/// there is one: a hook or filter from any agent's branch can run there.
fn integrate(
    repo: &Path,
    worktrees_dir: &Path,
    base: &str,
    agents: &[String],
    helper: Option<HelperFor>,
) -> Result<(git::Worktree, Vec<git::IntegrationStep>)> {
    let Some(helper) = helper else {
        return git::integrate(repo, worktrees_dir, base, agents);
    };
    let (worktree, needs_population) =
        git::add_worktree(repo, worktrees_dir, git::INTEGRATION_DIR, base)?;
    let launcher = helper(&worktree.path)?;
    let steps = git::launch::with_launcher(launcher, || -> Result<_> {
        if needs_population {
            git::populate_worktree(&worktree)?;
        }
        git::merge_agents(&worktree, agents)
    })?;
    Ok((worktree, steps))
}

impl Daemon {
    /// Refuse a team that requires sandboxes and cannot have them, before
    /// anything of it starts. Probes the host.
    pub(crate) async fn refuse_unsandboxable(
        &self,
        config: &SandboxConfig,
        backend: &'static str,
        worktrees: bool,
    ) -> Result<(), String> {
        if config.mode == SandboxMode::Off {
            return Ok(());
        }
        let crew_bin = self.crew_bin.clone();
        let nix_daemon = config.nix_daemon;
        let (host, nix) = tokio::task::spawn_blocking(move || {
            let path = std::env::var_os("PATH");
            let host = probe::probe(&crew_bin, path.as_ref()).map(drop);
            let nix = nix_daemon.then(|| untrusted_nix_daemon(path.as_ref()).map(drop));
            (host, nix)
        })
        .await
        .map_err(|e| e.to_string())?;
        let readiness = Readiness {
            backend,
            worktrees,
            host,
            nix,
        };
        match readiness.refusal() {
            Some(why) => Err(format!("workspace.sandbox.mode is required, but {why}")),
            None => Ok(()),
        }
    }

    /// Integration (whoever asks for it), in a helper sandbox when the team
    /// requires sandboxes. Blocking.
    pub(crate) fn integration(
        &self,
        config: &SandboxConfig,
        base: &str,
        agents: &[String],
    ) -> Result<(git::Worktree, Vec<git::IntegrationStep>)> {
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            Ok(Arc::new(self.helper_launcher(config, worktree)?))
        };
        let required = config.mode == SandboxMode::Required;
        integrate(
            &self.workspace,
            &self.worktrees_dir(),
            base,
            agents,
            required.then_some(&helper as HelperFor),
        )
    }

    /// A member's worktree, filled in a helper sandbox when the team
    /// requires sandboxes. Blocking.
    pub(crate) fn member_worktree(
        &self,
        team: &TeamConfig,
        id: &str,
        base: &str,
    ) -> Result<git::Worktree> {
        let config = &team.workspace.sandbox;
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            Ok(Arc::new(self.helper_launcher(config, worktree)?))
        };
        let required = config.mode == SandboxMode::Required;
        provision_worktree(
            &self.workspace,
            &self.worktrees_dir(),
            id,
            base,
            required.then_some(&helper as HelperFor),
        )
    }

    /// A sandbox for git working on `worktree` for nobody in particular:
    /// no identity, socket, agent directory or network. Blocking: probes
    /// the host.
    fn helper_launcher(&self, config: &SandboxConfig, worktree: &Path) -> Result<SandboxLauncher> {
        let (host, layout) = host_and_layout(&self.crew_bin, &self.workspace, &self.socket)?;
        let admin = git::worktree_admin_dir(&layout.git_dir, worktree)
            .ok_or_else(|| anyhow!("{} is not a worktree crew made", worktree.display()))?;
        let (launcher, warnings) =
            git::launch::helper_launcher(&host, layout.session(), &extra(config), worktree, &admin)
                .map_err(|e| {
                    anyhow!("{} cannot be filled in a sandbox: {e}", worktree.display())
                })?;
        // What it leaves out (PATH entries, toolchain leaves) is the same for
        // every profile, and is reported when an agent's session starts.
        drop(warnings);
        Ok(launcher)
    }

    /// Whether this team's agents run in sandboxes (and so keep their CLIs'
    /// configuration in private homes).
    pub(crate) fn sandboxed(&self) -> bool {
        self.sandboxing().is_some()
    }

    /// The team's sandbox settings, and whether it works in worktrees, if
    /// it requires sandboxes.
    fn sandboxing(&self) -> Option<(SandboxConfig, bool)> {
        let model = self.model();
        let team = model.session.team.as_ref()?;
        (team.workspace.sandbox.mode == SandboxMode::Required).then(|| {
            (
                team.workspace.sandbox.clone(),
                model.session.isolation == "worktree",
            )
        })
    }

    /// What an agent's own git (commit, status, diff, cherry-pick, merge)
    /// runs through when the team requires sandboxes: the agent's sandbox,
    /// with its identity and nothing else of its launch, so a hook on its
    /// branch runs as the agent and no one else (design-b3.md §3.4, D1).
    pub(crate) async fn agent_git_launcher(
        &self,
        agent: &Agent,
    ) -> Result<Option<Arc<dyn Launcher>>> {
        if self.sandboxing().is_none() {
            return Ok(None);
        }
        if agent.orchestrator {
            bail!(
                "the orchestrator's checkout is read-only in a sandboxed team, so it has no git \
of its own; a member commits in its worktree"
            );
        }
        let run = Run::git(&agent.id, &self.agent_dir(&agent.id));
        let env = run.env.clone();
        let Some(spec) = self.agent_sandbox(agent, run).await? else {
            return Ok(None);
        };
        let admin =
            git::worktree_admin_dir(&self.workspace.join(".git"), Path::new(&agent.workdir))
                .ok_or_else(|| anyhow!("{} is not a worktree crew made", agent.workdir))?;
        Ok(Some(Arc::new(
            SandboxLauncher::new(spec, env).with_admin_dir(admin),
        )))
    }

    /// The sandbox something an agent runs goes in: none unless the team
    /// requires sandboxes, and then it does not start without one. The host
    /// is probed every time.
    pub(crate) async fn agent_sandbox(&self, agent: &Agent, run: Run) -> Result<Option<Spec>> {
        let Some((config, worktrees)) = self.sandboxing() else {
            return Ok(None);
        };
        if let Some(backend) = run.terminal.filter(|b| *b != "pty") {
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
        // Reported once per start of the session; the agent's git, sidecar
        // and queue commands leave out the same things and say nothing more.
        let report = run.terminal.is_some();
        let built = tokio::task::spawn_blocking(move || -> Result<Built> {
            let (host, layout) = host_and_layout(&crew_bin, &workspace, &socket)?;
            let path = std::env::var_os("PATH");
            // The crew binary is bound anyway; any other program is a CLI,
            // and its install directory is bound.
            let crew_bin = crew_bin.canonicalize().ok();
            let cli = find_program(&run.program, path.as_ref())
                .filter(|p| p.canonicalize().ok() != crew_bin);
            let at = Placement {
                agent_dir: &agent_dir,
                workdir: &workdir,
                orchestrator,
                worktrees,
            };
            let nix = config
                .nix_daemon
                .then(|| untrusted_nix_daemon(path.as_ref()))
                .transpose()
                .map_err(|e| anyhow!("the Nix daemon cannot be given to {}: {e}", run.agent_id))?;
            agent_profile(
                &host,
                &layout,
                &at,
                &config,
                &run,
                cli.as_deref(),
                nix.as_ref(),
            )
        })
        .await??;
        for warning in built.warnings.iter().filter(|_| report) {
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
        /// The profile of agent `id`'s terminal session (`sh`, with
        /// `CREW_IDENTITY`).
        fn profile(&self, id: &str, orchestrator: bool, worktrees: bool) -> Result<Spec> {
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
            self.profile_for(orchestrator, worktrees, &Run::session(&launch, "pty"))
        }

        fn profile_for(&self, orchestrator: bool, worktrees: bool, run: &Run) -> Result<Spec> {
            self.profile_with(orchestrator, worktrees, run, None)
        }

        fn profile_with(
            &self,
            orchestrator: bool,
            worktrees: bool,
            run: &Run,
            nix: Option<&UntrustedNixDaemon>,
        ) -> Result<Spec> {
            let crew_bin = std::env::current_exe().unwrap();
            let host = Host::detect(&crew_bin, Path::new("/bwrap"), landlock::MIN_ABI);
            let layout = Layout::new(&self.ws, &self.socket, &self.root.join("cache"))?;
            let agent_dir = self.ws.join(".crew/agents").join(&run.agent_id);
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
            let config = SandboxConfig {
                mode: SandboxMode::Required,
                ..SandboxConfig::default()
            };
            agent_profile(&host, &layout, &at, &config, run, None, nix).map(|built| built.spec)
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

    #[test]
    fn a_sidecar_or_queue_command_gets_the_agents_profile_without_a_terminal() {
        let f = fixture();
        let sidecar_dir = f.root.join("run/codex-0123");
        std::fs::create_dir(&sidecar_dir).unwrap();
        let env = [("CODEX_HOME".to_string(), "/codex-home".to_string())];
        let run = Run::helper("a", "codex", &env, &sidecar_dir.join("1.sock"));

        let spec = f.profile_for(false, true, &run).unwrap();
        assert!(spec.new_session, "a helper keeps the agent's terminal");
        assert_eq!(spec.cwd, f.worktree);
        assert_eq!(spec.pass_env, ["CODEX_HOME"]);
        assert!(binds(&spec).contains(&(sidecar_dir.clone(), true)));
        assert!(binds(&spec).contains(&(f.socket.clone(), true)));
    }

    #[test]
    fn a_sandboxed_command_runs_behind_sandbox_run_with_only_the_launchers_environment() {
        let f = fixture();
        let spec = f.profile("a", false, true).unwrap();
        let argv = ["codex".to_string(), "queue".to_string()];
        let env = [("CODEX_HOME".to_string(), "/codex-home".to_string())];

        let sandboxed = command(Some(&spec), &argv, &env).unwrap();
        let cmd = sandboxed.as_std();
        assert_eq!(cmd.get_program(), spec.crew_bin.as_os_str());
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(args[0], "sandbox-run");
        assert_eq!(args[2..], ["--", "codex", "queue"]);
        // std shows a cleared environment as `env -i` (and only the
        // variables set after it).
        assert!(format!("{cmd:?}").starts_with("env -i "), "{cmd:?}");
        let set: Vec<(OsString, OsString)> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_os_string(), v?.to_os_string())))
            .collect();
        assert_eq!(set, launcher_env(&spec, std::env::vars_os(), &env));

        let plain = command(None, &argv, &env).unwrap();
        let cmd = plain.as_std();
        assert_eq!(cmd.get_program(), "codex");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["queue"]);
        assert!(!format!("{cmd:?}").starts_with("env -i "), "{cmd:?}");
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            [(
                std::ffi::OsStr::new("CODEX_HOME"),
                Some(std::ffi::OsStr::new("/codex-home"))
            )]
        );
    }

    #[test]
    fn an_agents_git_runs_in_its_profile_with_its_identity_only() {
        let f = fixture();
        let agent_dir = f.ws.join(".crew/agents/a");
        let run = Run::git("a", &agent_dir);
        assert_eq!(
            run.env,
            [(
                "CREW_IDENTITY".to_string(),
                agent_dir
                    .join("identity.json")
                    .to_string_lossy()
                    .into_owned()
            )]
        );

        let spec = f.profile_for(false, true, &run).unwrap();
        assert!(spec.new_session, "git has no terminal");
        assert_eq!(spec.cwd, f.worktree);
        assert_eq!(spec.pass_env, ["CREW_IDENTITY"]);
        let binds = binds(&spec);
        assert!(binds.contains(&(f.worktree.clone(), true)));
        assert!(binds.contains(&(f.ws.join(".git/worktrees/a"), true)));
        assert!(binds.contains(&(f.ws.join(".git/worktrees/a/config.worktree"), false)));
    }

    /// Passes every git command through unchanged, and keeps its arguments.
    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<Vec<String>>>);

    impl Launcher for Recorder {
        fn launch(&self, cmd: &std::process::Command) -> std::io::Result<std::process::Command> {
            let argv = std::iter::once(cmd.get_program())
                .chain(cmd.get_args())
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            self.0.lock().unwrap().push(argv);
            let mut same = std::process::Command::new(cmd.get_program());
            same.args(cmd.get_args());
            for (name, value) in cmd.get_envs() {
                match value {
                    Some(value) => same.env(name, value),
                    None => same.env_remove(name),
                };
            }
            Ok(same)
        }
    }

    #[test]
    fn a_new_worktree_is_filled_through_the_helper_and_made_without_it() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().canonicalize().unwrap().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&ws)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(ws.join("tracked.txt"), "x\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        let worktrees = ws.join(".crew/worktrees");
        let recorder = Arc::new(Recorder::default());
        let calls = std::cell::Cell::new(0);
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            calls.set(calls.get() + 1);
            assert_eq!(worktree, worktrees.join("a"));
            Ok(recorder.clone())
        };

        let made = provision_worktree(&ws, &worktrees, "a", "main", Some(&helper)).unwrap();
        assert_eq!(made.path, worktrees.join("a"));
        assert_eq!(
            std::fs::read_to_string(made.path.join("tracked.txt")).unwrap(),
            "x\n"
        );
        let seen = recorder.0.lock().unwrap().clone();
        assert!(
            seen.iter().any(|argv| argv.contains(&"reset".to_string())),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|argv| argv.contains(&"worktree".to_string())),
            "{seen:?}"
        );
        assert_eq!(calls.get(), 1);

        // Reused once filled: nothing runs in it again.
        provision_worktree(&ws, &worktrees, "a", "main", Some(&helper)).unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn integration_merges_through_the_helper_in_its_own_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().canonicalize().unwrap().join("ws");
        git::init_repo(&ws).unwrap();
        let worktrees = ws.join(".crew/worktrees");
        let a = git::ensure_worktree(&ws, &worktrees, "a", "main").unwrap();
        std::fs::write(a.path.join("from-a.txt"), "a\n").unwrap();
        git::commit_all(&a.path, "a", "work of a").unwrap();
        let recorder = Arc::new(Recorder::default());
        let calls = std::cell::Cell::new(0);
        let integration = worktrees.join(git::INTEGRATION_DIR);
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            calls.set(calls.get() + 1);
            assert_eq!(worktree, integration);
            Ok(recorder.clone())
        };

        let (made, steps) =
            integrate(&ws, &worktrees, "main", &["a".to_string()], Some(&helper)).unwrap();
        assert_eq!(made.path, integration);
        assert_eq!(steps.len(), 1);
        assert!(steps[0].outcome.ok, "{:?}", steps[0].outcome);
        assert!(integration.join("from-a.txt").exists());
        assert_eq!(calls.get(), 1);
        let seen = recorder.0.lock().unwrap().clone();
        assert!(
            seen.iter().any(|argv| argv.contains(&"merge".to_string())),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|argv| argv.contains(&"reset".to_string())),
            "{seen:?}"
        );
        let elsewhere: Vec<_> = seen
            .iter()
            .filter(|argv| argv.get(2).map(PathBuf::from) != Some(integration.clone()))
            .collect();
        assert!(
            elsewhere.is_empty(),
            "helper git outside its worktree: {elsewhere:?}"
        );
    }

    #[test]
    fn a_team_that_cannot_be_sandboxed_is_refused_with_what_would_fix_it() {
        let ready = || Readiness {
            backend: "pty",
            worktrees: true,
            host: Ok(()),
            nix: Some(Ok(())),
        };
        assert_eq!(ready().refusal(), None);
        let unavailable = probe::Unavailable {
            cause: "the kernel offers Landlock ABI 6".into(),
            fix: "run a newer kernel".into(),
        };
        for (readiness, says) in [
            (
                Readiness {
                    backend: "herdr",
                    ..ready()
                },
                "herdr backend starts its own programs",
            ),
            (
                Readiness {
                    worktrees: false,
                    ..ready()
                },
                "agents sharing the checkout cannot be sandboxed",
            ),
            (
                Readiness {
                    host: Err(unavailable),
                    ..ready()
                },
                "on this host: the kernel offers Landlock ABI 6; run a newer kernel",
            ),
            (
                Readiness {
                    nix: Some(Err("the daemon trusts you".into())),
                    ..ready()
                },
                "nix_daemon is set and the Nix daemon cannot be given to agents (the daemon \
trusts you); set workspace.sandbox.nix_daemon: false",
            ),
        ] {
            let why = readiness.refusal().expect("refused");
            assert!(why.contains(says), "{why}");
            assert!(why.contains("workspace.sandbox.mode: off"), "{why}");
        }

        // A probe whose fix already offers the opt-out is not told it twice.
        let offers_off = Readiness {
            host: Err(probe::Unavailable {
                cause: "the kernel offers Landlock ABI 6".into(),
                fix: "run a newer kernel, or set `workspace.sandbox.mode: off` to run agents \
unsandboxed"
                    .into(),
            }),
            ..ready()
        };
        let why = offers_off.refusal().expect("refused");
        assert_eq!(
            why.matches("workspace.sandbox.mode: off").count(),
            1,
            "{why}"
        );
    }

    #[test]
    fn an_agent_gets_the_nix_daemon_only_once_it_was_found_not_to_trust_the_operator() {
        let f = fixture();
        let nix_dir = f.root.join("nix-daemon");
        std::fs::create_dir(&nix_dir).unwrap();
        let socket = nix_dir.join("socket");
        let _nix = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let verified = UntrustedNixDaemon::verified_for_test(&socket);
        let session = |nix| {
            let launch = Run::git("a", &f.ws.join(".crew/agents/a"));
            f.profile_with(false, true, &launch, nix).unwrap()
        };

        let with = session(Some(&verified));
        assert!(binds(&with).contains(&(socket.clone(), true)));
        assert!(with.allow_unix.contains(&nix_dir), "{:?}", with.allow_unix);
        let without = session(None);
        assert!(!binds(&without).iter().any(|(p, _)| p == &socket));
        assert!(!without.allow_unix.contains(&nix_dir));
    }
}
