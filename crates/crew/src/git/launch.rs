//! Where a git or jj command runs: the daemon's own process, or inside a
//! sandbox.
//!
//! Both backends start their commands through one funnel
//! ([`super::output`]). A [`Launcher`] installed for the current thread with
//! [`with_launcher`] gets each command first and returns the one to run
//! instead, so an agent's version control (commit, status, diff,
//! cherry-pick, merge) and the helper work (populating and integrating
//! worktrees) can run in a sandbox without a new parameter on every
//! function. Nothing falls back: a launcher that fails fails the operation.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::sandbox::{run, spec::Spec};

/// Turns the command a backend built (program, arguments, environment) into
/// the command to run.
pub trait Launcher: Send + Sync {
    fn launch(&self, cmd: &Command) -> io::Result<Command>;

    /// The administrative directory (`<common>/worktrees/<name>`) of the
    /// git worktree this launcher works in, as the daemon created it, if it
    /// is known. After a timeout the daemon clears a stale `index.lock`
    /// there: a path git printed inside a sandbox is the agent's to forge,
    /// so it is never used for host file operations. None under jj, whose
    /// lock after a timeout is a ref lock of the store the daemon knows.
    fn admin_dir(&self) -> Option<PathBuf> {
        None
    }

    /// Told, right after the command `launch` returned was started, its
    /// pid, or None when it could not be started. The daemon keeps each
    /// sandbox an agent's git starts in its retirement ledger this way
    /// (design-b3.md §3.4, D2), for as long as the job runs, whoever awaits
    /// it.
    fn started(&self, _pid: Option<u32>) {}
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<dyn Launcher>>> = const { RefCell::new(None) };
}

/// Run `f`, a synchronous sequence of git or jj operations, with every
/// command started by this thread going through `launcher`.
pub fn with_launcher<T>(launcher: Arc<dyn Launcher>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<dyn Launcher>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.borrow_mut().replace(launcher)));
    f()
}

/// The administrative directory of the installed launcher, if any.
pub(super) fn admin_dir() -> Option<PathBuf> {
    CURRENT.with(|c| c.borrow().as_ref().and_then(|l| l.admin_dir()))
}

/// Whether a launcher is installed (git is running on an agent's behalf).
pub(super) fn active() -> bool {
    CURRENT.with(|c| c.borrow().is_some())
}

/// Tell the installed launcher, if any, that the command it launched was
/// started as `pid` (None: it was not).
pub(super) fn started(pid: Option<u32>) {
    if let Some(launcher) = CURRENT.with(|c| c.borrow().clone()) {
        launcher.started(pid);
    }
}

/// Replace `cmd` by what the installed launcher makes of it, if any.
pub(super) fn apply(cmd: &mut Command) -> io::Result<()> {
    let Some(launcher) = CURRENT.with(|c| c.borrow().clone()) else {
        return Ok(());
    };
    *cmd = launcher.launch(cmd)?;
    Ok(())
}

/// Runs git or jj inside the sandbox `spec` describes, through `crew
/// sandbox-run`. The command keeps its program: git by name, found on the
/// sandbox's PATH, or jj by the absolute path the profile bound.
///
/// The command's own variables (git's author and committer identity, jj's
/// `JJ_USER`, `JJ_EMAIL` and `JJ_CONFIG`, the prompt and editor switches)
/// are non-secret and travel in the spec; the
/// launched process gets nothing else from the daemon's environment but the
/// names `spec::launcher_env` allows. A spec for a helper has no `pass_env`.
pub struct SandboxLauncher {
    spec: Spec,
    /// The agent's own launch variables (its `LaunchSpec.env`: identity, endpoint
    /// keys) for operations on that agent's behalf; only the names the spec
    /// passes get through. Empty for helpers.
    launch_env: Vec<(String, String)>,
    admin: Option<PathBuf>,
}

impl SandboxLauncher {
    pub fn new(spec: Spec, launch_env: Vec<(String, String)>) -> Self {
        Self {
            spec,
            launch_env,
            admin: None,
        }
    }

    /// The git worktree's administrative directory, for clearing a stale
    /// lock.
    pub fn with_admin_dir(mut self, admin: PathBuf) -> Self {
        self.admin = Some(admin);
        self
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }
}

impl Launcher for SandboxLauncher {
    fn admin_dir(&self) -> Option<PathBuf> {
        self.admin.clone()
    }

    fn launch(&self, cmd: &Command) -> io::Result<Command> {
        let invalid = |e: String| io::Error::new(io::ErrorKind::InvalidInput, e);
        let mut spec = self.spec.clone();
        for (name, value) in cmd.get_envs() {
            let (Some(value), Some(name)) = (value, name.to_str()) else {
                continue;
            };
            let value = value
                .to_str()
                .ok_or_else(|| invalid(format!("the value of {name} is not UTF-8")))?;
            spec.env.retain(|(n, _)| n != name);
            spec.env.push((name.to_string(), value.to_string()));
        }
        let prefix = run::prefix(&spec).map_err(invalid)?;
        let mut launched = Command::new(&prefix[0]);
        launched
            .args(&prefix[1..])
            .arg(cmd.get_program())
            .args(cmd.get_args())
            .env_clear()
            .envs(crate::sandbox::spec::launcher_env(
                &spec,
                std::env::vars_os(),
                &self.launch_env,
            ))
            .stdin(Stdio::null());
        Ok(launched)
    }
}

/// The argument vector of `cmd` as strings, for tests and logs.
#[cfg(test)]
pub(super) fn argv(cmd: &Command) -> Vec<std::ffi::OsString> {
    std::iter::once(cmd.get_program().to_owned())
        .chain(cmd.get_args().map(|a| a.to_owned()))
        .collect()
}

/// A launcher for the helper work git or jj does on behalf of no agent:
/// filling a new worktree in and merging every agent's branch for
/// integration. Its sandbox has the toolchain, the one worktree (writable)
/// and the store data for it (git's, with the worktree's `admin` directory;
/// or jj's), and nothing of the daemon: no socket, no state directory, no
/// agent directory, no identity or model variable, and no network. A hook or
/// filter from any agent's branch can run in it, and has no authority.
/// `program` is the jj the helper runs, bound by its path (git is found on
/// the sandbox's PATH).
pub fn helper_launcher(
    host: &crate::sandbox::profile::Host,
    session: crate::sandbox::profile::Session<'_>,
    extra: &crate::sandbox::profile::Extra,
    worktree: &Path,
    admin: Option<&Path>,
    program: Option<&Path>,
) -> Result<(SandboxLauncher, Vec<String>), String> {
    use crate::sandbox::profile::{build, Request, Role};
    let built = build(
        host,
        &Request {
            session,
            role: Role::Helper { worktree, admin },
            extra,
            cli: program,
            pass_env: &[],
            terminal: false,
            nix_daemon: None,
        },
    )?;
    let mut launcher = SandboxLauncher::new(built.spec, Vec::new());
    if let Some(admin) = admin {
        launcher = launcher.with_admin_dir(admin.to_path_buf());
    }
    Ok((launcher, built.warnings))
}
