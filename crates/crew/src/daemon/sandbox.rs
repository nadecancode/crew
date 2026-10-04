//! Sandboxed launches, from the daemon's side (design-b3.md §3.2, §3.6): the
//! profile an agent runs in, built only from paths the daemon provisioned.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::config::{SandboxConfig, SandboxMode, TeamConfig};
use crew_core::{paths, Agent};

use super::{retire, Daemon};
use crate::agents::LaunchSpec;
use crate::git;
use crate::git::launch::{Launcher, SandboxLauncher};
use crate::persistence::NewEvent;
use crate::sandbox::probe::UntrustedNixDaemon;
use crate::sandbox::profile::{self, Built, Extra, Host, Request, Role, Session};
use crate::sandbox::spec::{launcher_env, Spec};
use crate::sandbox::{probe, run};
use crate::vcs::{IntegrationStep, Vcs, Workspace};

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

/// The sandbox something an agent runs goes in, and what to run there.
pub(crate) struct Sandboxed {
    pub spec: Spec,
    /// The program the daemon's PATH found, whose file the profile bound,
    /// by absolute path (program_to_run), so no lookup in the sandbox's own
    /// PATH can pick another one. None: the program runs as given (the
    /// crew binary, or a path).
    pub program: Option<PathBuf>,
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

/// What the daemon's PATH finds for a program.
struct Found {
    /// Found by name on PATH, and the file it resolves to, resolved once,
    /// here: what runs never depends on looking it up again.
    by_name: Option<(PathBuf, PathBuf)>,
    /// What PATH found (or the program, given as a path), if it is an
    /// agent CLI, whose install directory the profile binds: anything but
    /// the crew binary, which is bound anyway.
    cli: Option<PathBuf>,
}

impl Found {
    /// A name found on PATH whose file cannot be resolved is an error, not
    /// a program to look up again.
    fn new(program: &str, path: Option<&OsString>, crew_bin: &Path) -> Result<Self> {
        let path = find_program(program, path);
        let by_name = match path.as_ref().filter(|_| !program.contains('/')) {
            Some(found) => {
                let file = found.canonicalize().with_context(|| {
                    format!(
                        "`{program}` was found at {}, but not its file",
                        found.display()
                    )
                })?;
                Some((found.clone(), file))
            }
            None => None,
        };
        let crew_bin = crew_bin.canonicalize().ok();
        let cli = path.filter(|p| p.canonicalize().ok() != crew_bin);
        Ok(Self { by_name, cli })
    }

    /// What runs for `name` in the sandbox `spec` (program_to_run): the
    /// program found, CLI or crew binary alike.
    fn program(&self, name: &str, spec: &Spec) -> Option<PathBuf> {
        program_to_run(
            name,
            self.by_name
                .as_ref()
                .map(|(f, file)| (f.as_path(), file.as_path())),
            spec,
        )
    }
}

/// What runs for `name`, which the daemon's PATH found at `found`, a name
/// for `file`, which the profile bound: that file; or `found` itself, which
/// keeps the name a program may look at (bash run as `sh`), where it leads
/// to that file inside the sandbox too. A program given as a path, or not
/// found, runs as given (None).
fn program_to_run(name: &str, found: Option<(&Path, &Path)>, spec: &Spec) -> Option<PathBuf> {
    let (found, file) = found.filter(|_| !name.contains('/'))?;
    if resolve_inside(spec, found).as_deref() == Some(file) {
        Some(found.to_path_buf())
    } else {
        Some(file.to_path_buf())
    }
}

/// What a path names inside the sandbox `spec` describes, every link
/// followed, if it names a host object bound there: where bubblewrap lays
/// out the mounts in order (a later one over an earlier one), makes the
/// directories mount points need, recreates links (Mount::Symlink), and
/// shows bound trees as the host has them, their links included.
fn resolve_inside(spec: &Spec, path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut rest = path.to_path_buf();
    let mut at = PathBuf::from("/");
    let mut bound = false;
    let mut links = 0;
    loop {
        let mut components = rest.components();
        let Some(component) = components.next() else {
            break;
        };
        let after = components.as_path().to_path_buf();
        match component {
            Component::RootDir => (at, bound) = (PathBuf::from("/"), false),
            Component::CurDir => {}
            Component::ParentDir => {
                at.pop();
                bound = false;
            }
            Component::Normal(name) => {
                let next = at.join(name);
                match shown(spec, &next)? {
                    Shown::Bound => (at, bound) = (next, true),
                    Shown::Made => (at, bound) = (next, false),
                    Shown::Link(target) => {
                        links += 1;
                        if links > 40 {
                            return None;
                        }
                        // Relative to the link's directory, `at`.
                        rest = target.join(after);
                        continue;
                    }
                }
            }
            Component::Prefix(_) => return None,
        }
        rest = after;
    }
    bound.then_some(at)
}

/// What the sandbox shows at a path whose parent it shows as a directory.
enum Shown {
    /// A host object of a bound tree (not a link).
    Bound,
    /// A link, the host's in a bound tree or one the profile recreates.
    Link(PathBuf),
    /// A directory bubblewrap made: a tmpfs, an empty directory, or one a
    /// later mount point needs.
    Made,
}

fn shown(spec: &Spec, path: &Path) -> Option<Shown> {
    use crate::sandbox::spec::Mount;
    let covering = spec.mounts.iter().rposition(|m| path.starts_with(m.path()));
    // A mount point beneath, laid out after whatever covers the path.
    let needed = spec.mounts[covering.map_or(0, |i| i + 1)..]
        .iter()
        .any(|m| m.path().starts_with(path) && m.path() != path);
    let Some(i) = covering else {
        return needed.then_some(Shown::Made);
    };
    match &spec.mounts[i] {
        Mount::Symlink { path: at, target } if at == path => Some(Shown::Link(target.clone())),
        Mount::Symlink { .. } | Mount::Mask { .. } => None,
        Mount::Bind { .. } => match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                Some(Shown::Link(std::fs::read_link(path).ok()?))
            }
            Ok(_) => Some(Shown::Bound),
            Err(_) => needed.then_some(Shown::Made),
        },
        Mount::Tmpfs { path: at } | Mount::Dir { path: at } => {
            (at == path || needed).then_some(Shown::Made)
        }
    }
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
    vcs: &dyn Vcs,
    worktrees_dir: &Path,
    id: &str,
    base: &str,
    helper: Option<HelperFor>,
) -> Result<Workspace> {
    let (worktree, needs_population) = vcs.add_workspace(worktrees_dir, id, base)?;
    if needs_population {
        match helper {
            Some(helper) => {
                let launcher = helper(&worktree.path)?;
                git::launch::with_launcher(launcher, || vcs.populate(&worktree))?;
            }
            None => vcs.populate(&worktree)?,
        }
    }
    Ok(worktree)
}

/// Integration on its own worktree, made outside any launcher; it is filled
/// and every agent branch merged into it through `helper`'s launcher when
/// there is one: a hook or filter from any agent's branch can run there.
fn integrate(
    vcs: &dyn Vcs,
    worktrees_dir: &Path,
    base: &str,
    agents: &[String],
    helper: Option<HelperFor>,
    on_step: &mut dyn FnMut(&IntegrationStep),
) -> Result<(Workspace, Vec<IntegrationStep>)> {
    let (worktree, needs_population) =
        vcs.add_workspace(worktrees_dir, git::INTEGRATION_DIR, base)?;
    let mut run = || -> Result<_> {
        if needs_population {
            vcs.populate(&worktree)?;
        }
        vcs.merge_agents(&worktree, agents, on_step)
    };
    let steps = match helper {
        Some(helper) => git::launch::with_launcher(helper(&worktree.path)?, run)?,
        None => run()?,
    };
    Ok((worktree, steps))
}

/// What an agent's git runs through when the team requires sandboxes: the
/// agent's sandbox, each command in the agent's retirement ledger before it
/// starts and its init found right after (design-b3.md §3.4, D2). It
/// belongs to one blocking job, which starts its commands one at a time.
struct LedgerLauncher {
    daemon: Daemon,
    agent: String,
    spec: Spec,
    /// The git the profile bound (Sandboxed::program).
    program: Option<PathBuf>,
    env: Vec<(String, String)>,
    admin: PathBuf,
    /// The entry of the command launched last, until it is started. One
    /// slot: a launcher serves one blocking job, whose git commands start
    /// one at a time on its thread (git::launch::with_launcher). It must
    /// not be shared across concurrent commands: one command's pid and
    /// init could be recorded in another's entry, which would then count
    /// as gone when the wrong sandbox ends.
    pending: std::sync::Mutex<Option<retire::Record>>,
}

/// `cmd` with `program` in place of its own.
fn with_program(cmd: &std::process::Command, program: &Path) -> std::process::Command {
    let mut with = std::process::Command::new(program);
    with.args(cmd.get_args());
    for (name, value) in cmd.get_envs() {
        match value {
            Some(value) => with.env(name, value),
            None => with.env_remove(name),
        };
    }
    if let Some(dir) = cmd.get_current_dir() {
        with.current_dir(dir);
    }
    with
}

impl Launcher for LedgerLauncher {
    fn launch(&self, cmd: &std::process::Command) -> std::io::Result<std::process::Command> {
        let record = self
            .daemon
            .ledger
            .start(&self.agent, "git")
            .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        let mut spec = self.spec.clone();
        spec.done = Some(record.done().to_path_buf());
        let cmd = match &self.program {
            Some(program) => with_program(cmd, program),
            None => with_program(cmd, Path::new(cmd.get_program())),
        };
        let launched = SandboxLauncher::new(spec, self.env.clone()).launch(&cmd);
        match launched {
            Ok(_) => *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(record),
            Err(_) => self.daemon.ledger.never_started(&record),
        }
        launched
    }

    fn admin_dir(&self) -> Option<PathBuf> {
        Some(self.admin.clone())
    }

    fn started(&self, pid: Option<u32>) {
        let Some(record) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        else {
            return;
        };
        let Some(pid) = pid else {
            self.daemon.ledger.never_started(&record);
            return;
        };
        record.update(|r| r.pid = Some(pid));
        // Its init, before a timeout can kill it: git::output holds the
        // child, unreaped, until this returns, so the number is its own. One
        // that ends first says so through its done marker.
        let Some(wrapper) = retire::Proc::of(pid) else {
            return;
        };
        let deadline = std::time::Instant::now() + retire::DISCOVER_WITHIN;
        let init = loop {
            if let Some(init) = retire::sandbox_init(wrapper) {
                break Some(init);
            }
            let ended = !wrapper.alive();
            if ended || record.done().exists() || std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        record.update(|r| r.init = init);
    }
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
        on_step: &mut dyn FnMut(&IntegrationStep),
    ) -> Result<(Workspace, Vec<IntegrationStep>)> {
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            Ok(Arc::new(self.helper_launcher(config, worktree)?))
        };
        let required = config.mode == SandboxMode::Required;
        integrate(
            self.vcs().as_ref(),
            &self.worktrees_dir(),
            base,
            agents,
            required.then_some(&helper as HelperFor),
            on_step,
        )
    }

    /// A member's worktree, filled in a helper sandbox when the team
    /// requires sandboxes. Blocking.
    pub(crate) fn member_worktree(
        &self,
        team: &TeamConfig,
        id: &str,
        base: &str,
    ) -> Result<Workspace> {
        let config = &team.workspace.sandbox;
        let helper = |worktree: &Path| -> Result<Arc<dyn Launcher>> {
            Ok(Arc::new(self.helper_launcher(config, worktree)?))
        };
        let required = config.mode == SandboxMode::Required;
        provision_worktree(
            self.vcs().as_ref(),
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
        let Some(sandboxed) = self.agent_sandbox(agent, run).await? else {
            return Ok(None);
        };
        let admin =
            git::worktree_admin_dir(&self.workspace.join(".git"), Path::new(&agent.workdir))
                .ok_or_else(|| anyhow!("{} is not a worktree crew made", agent.workdir))?;
        Ok(Some(Arc::new(LedgerLauncher {
            daemon: self.clone(),
            agent: agent.id.clone(),
            spec: sandboxed.spec,
            program: sandboxed.program,
            env,
            admin,
            pending: std::sync::Mutex::new(None),
        })))
    }

    /// The sandbox something an agent runs goes in: none unless the team
    /// requires sandboxes, and then it does not start without one. The host
    /// is probed every time.
    pub(crate) async fn agent_sandbox(&self, agent: &Agent, run: Run) -> Result<Option<Sandboxed>> {
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
        let (built, program) = tokio::task::spawn_blocking(move || -> Result<_> {
            let (host, layout) = host_and_layout(&crew_bin, &workspace, &socket)?;
            let path = std::env::var_os("PATH");
            let found = Found::new(&run.program, path.as_ref(), &crew_bin)?;
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
            let built = agent_profile(
                &host,
                &layout,
                &at,
                &config,
                &run,
                found.cli.as_deref(),
                nix.as_ref(),
            )?;
            let program = found.program(&run.program, &built.spec);
            Ok((built, program))
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
        Ok(Some(Sandboxed {
            spec: built.spec,
            program,
        }))
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

        let made = provision_worktree(
            &crate::vcs::Git::new(&ws),
            &worktrees,
            "a",
            "main",
            Some(&helper),
        )
        .unwrap();
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
        provision_worktree(
            &crate::vcs::Git::new(&ws),
            &worktrees,
            "a",
            "main",
            Some(&helper),
        )
        .unwrap();
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

        let (made, steps) = integrate(
            &crate::vcs::Git::new(&ws),
            &worktrees,
            "main",
            &["a".to_string()],
            Some(&helper),
            &mut |_| {},
        )
        .unwrap();
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

    #[tokio::test]
    async fn an_agents_git_command_is_in_its_ledger_before_it_starts_and_reports_there() {
        let fx = fixture();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: fx.ws.clone(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let agent_dir = fx.ws.join(".crew/agents/a");
        let run = Run::git("a", &agent_dir);
        let env = run.env.clone();
        let launcher = LedgerLauncher {
            daemon: daemon.clone(),
            agent: "a".into(),
            spec: fx.profile_for(false, true, &run).unwrap(),
            program: None,
            env,
            admin: PathBuf::from("/admin"),
            pending: std::sync::Mutex::new(None),
        };
        let mut git = std::process::Command::new("git");
        git.arg("status");
        let short = std::time::Duration::from_millis(100);

        // Before it is started its entry is on disk, and its sandbox is told
        // where to report.
        let launched = launcher.launch(&git).unwrap();
        let entries = daemon.ledger.of("a");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].get().pid, None);
        let args: Vec<String> = launched
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "sandbox-run");
        let spec: Spec = serde_json::from_str(&args[1]).unwrap();
        assert_eq!(spec.done.as_deref(), Some(entries[0].done()));
        // Not started: no process, no entry.
        launcher.started(None);
        assert!(daemon.ledger.of("a").is_empty());
        assert!(daemon.ledger.retire("a", short).await.is_ok());

        // Started: its pid is kept, and it holds the agent until its sandbox
        // reports that it is gone.
        launcher.launch(&git).unwrap();
        let mut ended = std::process::Command::new("true").spawn().unwrap();
        let pid = ended.id();
        ended.wait().unwrap();
        launcher.started(Some(pid));
        let entry = daemon.ledger.of("a").pop().unwrap();
        assert_eq!(entry.get().pid, Some(pid));
        let refused = format!("{:#}", daemon.ledger.retire("a", short).await.unwrap_err());
        assert!(refused.contains("its git sandbox"), "{refused}");
        std::fs::write(entry.done(), "exited 0\n").unwrap();
        daemon.ledger.retire("a", short).await.unwrap();
    }

    #[test]
    fn the_cli_that_runs_is_the_one_the_daemons_path_found_by_its_absolute_path() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture();
        let root = &fx.root;
        let program = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // The first of two flat installations on the daemon's PATH.
        let (first, second) = (root.join("first/codex"), root.join("second/codex"));
        program(&first);
        program(&second);
        let path = std::env::join_paths([root.join("first"), root.join("second")]).unwrap();
        let found = find_program("codex", Some(&path)).unwrap();
        assert_eq!(found, first);
        let bound = |spec: &mut Spec, path: &Path| {
            spec.mounts.push(Mount::Bind {
                path: path.to_path_buf(),
                id: crate::sandbox::pin::identify(path).unwrap(),
                writable: false,
            })
        };
        // Where the sandbox shows that path (its directory bound, or the
        // file alone when the directory was left out), it runs by it.
        let mut spec = fx.profile("a", false, true).unwrap();
        bound(&mut spec, &first);
        assert_eq!(
            program_to_run(
                "codex",
                Some((&found, &found.canonicalize().unwrap())),
                &spec
            ),
            Some(first.clone())
        );
        // Installed behind a link in a directory the sandbox does not show:
        // the file it resolves to.
        let target = root.join("lib/node_modules/@openai/codex/bin/codex.js");
        program(&target);
        std::fs::create_dir_all(root.join("links")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("links/codex")).unwrap();
        let mut spec = fx.profile("a", false, true).unwrap();
        bound(&mut spec, &root.join("lib/node_modules/@openai/codex"));
        let linked = root.join("links/codex");
        assert_eq!(
            program_to_run(
                "codex",
                Some((&linked, &linked.canonicalize().unwrap())),
                &spec
            ),
            Some(target)
        );
        // ... and by the link where its directory is shown, so a program
        // that looks at its name (busybox's sh) still sees it.
        bound(&mut spec, &root.join("links"));
        assert_eq!(
            program_to_run(
                "codex",
                Some((&linked, &linked.canonicalize().unwrap())),
                &spec
            ),
            Some(linked)
        );
        // A program given as a path, or not found, runs as given.
        assert_eq!(
            program_to_run(
                "./codex",
                Some((&first, &first.canonicalize().unwrap())),
                &spec
            ),
            None
        );
        assert_eq!(program_to_run("codex", None, &spec), None);
    }

    #[test]
    fn the_crew_binary_by_bare_name_runs_as_found_too_though_it_is_no_cli() {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture();
        let root = &fx.root;
        let crew = root.join("build/crew");
        std::fs::create_dir_all(crew.parent().unwrap()).unwrap();
        std::fs::write(&crew, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&crew, std::fs::Permissions::from_mode(0o755)).unwrap();
        // On PATH through a link, as `~/.local/bin/crew` would be.
        std::fs::create_dir_all(root.join("links")).unwrap();
        std::os::unix::fs::symlink(&crew, root.join("links/crew")).unwrap();
        let path = std::env::join_paths([root.join("links")]).unwrap();
        let found = Found::new("crew", Some(&path), &crew).unwrap();
        // Bound anyway, so no CLI install directory is added for it...
        assert_eq!(found.cli, None);
        // ... but what runs is the binary found, not a second lookup.
        let spec = fx.profile("a", false, true).unwrap();
        assert_eq!(found.program("crew", &spec), Some(crew.clone()));
    }

    #[test]
    fn a_program_found_on_path_keeps_its_file_if_the_name_goes_before_the_launch() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fx = fixture();
        let root = &fx.root;
        let file = root.join("install/codex.js");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        symlink(&file, root.join("bin/codex")).unwrap();
        let path = std::env::join_paths([root.join("bin")]).unwrap();
        let found = Found::new("codex", Some(&path), &root.join("crew")).unwrap();
        // The link goes after the profile was built: what runs is still
        // the file found, never the bare name for another lookup.
        std::fs::remove_file(root.join("bin/codex")).unwrap();
        let spec = fx.profile("a", false, true).unwrap();
        assert_eq!(found.program("codex", &spec), Some(file));
    }

    /// A bind of `path` as the profile makes it.
    fn bind(path: &Path) -> Mount {
        Mount::Bind {
            path: path.to_path_buf(),
            id: crate::sandbox::pin::identify(path).unwrap(),
            writable: false,
        }
    }

    /// A spec holding exactly `mounts`.
    fn spec_with(fx: &Fixture, mounts: Vec<Mount>) -> Spec {
        let mut spec = fx.profile("a", false, true).unwrap();
        spec.mounts = mounts;
        spec
    }

    #[test]
    fn a_name_found_on_path_runs_by_it_only_where_it_leads_to_the_bound_file_inside() {
        use std::os::unix::fs::symlink;
        let fx = fixture();
        let root = &fx.root;
        let (visible, hidden, target) = (
            root.join("visible"),
            root.join("hidden"),
            root.join("target"),
        );
        for dir in [&visible, &hidden, &target] {
            std::fs::create_dir(dir).unwrap();
        }
        let file = target.join("tool-body");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();

        // Through a link whose middle hop the sandbox does not show: the
        // name would not resolve inside, so the bound file runs.
        symlink(&file, hidden.join("hop")).unwrap();
        let through_hidden = visible.join("cli");
        symlink(hidden.join("hop"), &through_hidden).unwrap();
        let spec = spec_with(&fx, vec![bind(&visible), bind(&target)]);
        assert_eq!(resolve_inside(&spec, &through_hidden), None);
        assert_eq!(
            program_to_run(
                "cli",
                Some((&through_hidden, &through_hidden.canonicalize().unwrap())),
                &spec
            ),
            Some(file.clone())
        );

        // Through a host link the profile recreates (a Symlink mount, as
        // for /run/current-system): the name resolves inside and runs by it.
        let system = root.join("system");
        symlink(&target, &system).unwrap();
        let recreated = spec_with(
            &fx,
            vec![
                Mount::Symlink {
                    path: system.clone(),
                    target: target.clone(),
                },
                bind(&target),
            ],
        );
        let alias = system.join("tool-body");
        assert_eq!(resolve_inside(&recreated, &alias), Some(file.clone()));
        assert_eq!(
            program_to_run(
                "tool-body",
                Some((&alias, &alias.canonicalize().unwrap())),
                &recreated
            ),
            Some(alias)
        );

        // Directly visible, and through a relative link in a visible
        // directory: by the name.
        symlink("../target/tool-body", visible.join("rel")).unwrap();
        assert_eq!(
            resolve_inside(&spec, &visible.join("rel")),
            Some(file.clone())
        );
        assert_eq!(
            program_to_run(
                "rel",
                Some((
                    &visible.join("rel"),
                    &visible.join("rel").canonicalize().unwrap()
                )),
                &spec
            ),
            Some(visible.join("rel"))
        );

        // Masked, or under a tmpfs laid over its bind: not the bound file.
        let masked = spec_with(&fx, vec![bind(&target), Mount::Mask { path: file.clone() }]);
        assert_eq!(resolve_inside(&masked, &file), None);
        let covered = spec_with(
            &fx,
            vec![
                bind(&target),
                Mount::Tmpfs {
                    path: target.clone(),
                },
            ],
        );
        assert_eq!(resolve_inside(&covered, &file), None);
        // A link loop resolves to nothing.
        symlink(visible.join("loop"), visible.join("loop")).unwrap();
        assert_eq!(resolve_inside(&spec, &visible.join("loop")), None);
    }

    /// The host as the daemon would see it, for launches through real
    /// bubblewrap and the crew binary (skipped, or failing under
    /// CREW_TEST_SANDBOX=require, where they are missing).
    fn real_host() -> Option<Host> {
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let crew = target.join("debug/crew").canonicalize().ok();
        let crew = crate::sandbox::test_support::host_has(crew.is_some(), "a built crew binary")
            .then(|| crew.unwrap())?;
        let available = crate::sandbox::probe::probe(&crew, std::env::var_os("PATH").as_ref());
        let available =
            crate::sandbox::test_support::host_has(available.is_ok(), "a usable sandbox")
                .then(|| available.unwrap())?;
        Some(Host::detect(
            &crew,
            &available.bwrap,
            available.landlock_abi,
        ))
    }

    /// The profile of a no-terminal launch of `name`, found at `found`.
    fn cli_profile(fx: &Fixture, host: &Host, name: &str, found: &Path) -> Spec {
        let layout = Layout::new(&fx.ws, &fx.socket, &fx.root.join("cache")).unwrap();
        let agent = fx.ws.join(".crew/agents/a");
        let at = Placement {
            agent_dir: &agent,
            workdir: &fx.worktree,
            orchestrator: false,
            worktrees: true,
        };
        let config = SandboxConfig {
            mode: SandboxMode::Required,
            ..SandboxConfig::default()
        };
        let run = Run {
            agent_id: "a".into(),
            program: name.into(),
            env: Vec::new(),
            sidecar_dir: None,
            terminal: None,
        };
        agent_profile(host, &layout, &at, &config, &run, Some(found), None)
            .unwrap()
            .spec
    }

    fn run_in(spec: &Spec, program: &Path, args: &[&str]) -> std::process::Output {
        let prefix = crate::sandbox::run::prefix(spec).unwrap();
        std::process::Command::new(&prefix[0])
            .args(&prefix[1..])
            .arg(program)
            .args(args)
            .env_clear()
            .envs(launcher_env(spec, std::env::vars_os(), &[]))
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    // rev-runtime's probes (m-e5bc8f41851b), through real bubblewrap.
    #[test]
    fn a_name_whose_link_crosses_what_the_sandbox_hides_runs_by_its_bound_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let Some(mut host) = real_host() else { return };
        let fx = fixture();
        let install = tempfile::tempdir_in(host.crew_bin.parent().unwrap()).unwrap();
        let base = install.path().canonicalize().unwrap();
        let (visible, hidden, target) = (
            base.join("visible"),
            base.join("hidden"),
            base.join("target"),
        );
        for dir in [&visible, &hidden, &target] {
            std::fs::create_dir(dir).unwrap();
        }
        let file = target.join("tool-body");
        let shell = std::fs::canonicalize("/bin/sh").unwrap();
        std::fs::write(&file, format!("#!{}\nprintf selected\n", shell.display())).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&file, hidden.join("hop")).unwrap();
        let found = visible.join("review-cli");
        symlink(hidden.join("hop"), &found).unwrap();
        host.path = std::env::join_paths([&visible]).unwrap();
        let spec = cli_profile(&fx, &host, "review-cli", &found);
        let control = run_in(&spec, &file, &[]);
        assert!(control.status.success(), "control: {control:?}");
        let chosen = program_to_run(
            "review-cli",
            Some((&found, &found.canonicalize().unwrap())),
            &spec,
        )
        .unwrap();
        let out = run_in(&spec, &chosen, &[]);
        assert!(
            out.status.success(),
            "{chosen:?} does not run inside: {out:?}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "selected");
    }

    #[test]
    fn a_name_through_a_recreated_system_link_keeps_its_name_inside() {
        let found = PathBuf::from("/run/current-system/sw/bin/sh");
        if !found.is_file() {
            eprintln!("skipped: this host has no {}", found.display());
            return;
        }
        let Some(mut host) = real_host() else { return };
        let fx = fixture();
        host.path = OsString::from("/run/current-system/sw/bin");
        let spec = cli_profile(&fx, &host, "sh", &found);
        let args = [
            "-c",
            "case \"$0\" in */sh|sh) printf sh;; *) printf changed;; esac",
        ];
        let control = run_in(&spec, &found, &args);
        assert_eq!(
            String::from_utf8_lossy(&control.stdout),
            "sh",
            "control: {control:?}"
        );
        let chosen =
            program_to_run("sh", Some((&found, &found.canonicalize().unwrap())), &spec).unwrap();
        let out = run_in(&spec, &chosen, &args);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "sh",
            "the name was changed to {chosen:?}: {out:?}"
        );
    }

    #[tokio::test]
    async fn an_agents_git_runs_the_git_its_profile_bound() {
        let fx = fixture();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: fx.ws.clone(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let run = Run::git("a", &fx.ws.join(".crew/agents/a"));
        let launcher = LedgerLauncher {
            daemon: running.daemon.clone(),
            agent: "a".into(),
            spec: fx.profile_for(false, true, &run).unwrap(),
            program: Some(PathBuf::from("/bound/bin/git")),
            env: run.env.clone(),
            admin: PathBuf::from("/admin"),
            pending: std::sync::Mutex::new(None),
        };
        let mut git = std::process::Command::new("git");
        git.args(["-C", "/w", "status"]).env("GIT_AUTHOR_NAME", "a");
        let launched = launcher.launch(&git).unwrap();
        launcher.started(None);
        let args: Vec<String> = launched
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = args.iter().position(|a| a == "--").unwrap() + 1;
        assert_eq!(args[after..], ["/bound/bin/git", "-C", "/w", "status"]);
        let spec: Spec = serde_json::from_str(&args[1]).unwrap();
        assert!(spec
            .env
            .contains(&("GIT_AUTHOR_NAME".to_string(), "a".to_string())));
    }
}
