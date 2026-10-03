//! Where a git command runs: the daemon's own process, or inside a sandbox.
//!
//! Everything in this module's parent starts git through one funnel. A
//! [`Launcher`] installed for the current thread with [`with_launcher`] gets
//! each command first and returns the one to run instead, so an agent's git
//! (commit, status, diff, cherry-pick, merge) and the helper work (populating
//! and integrating worktrees) can run in a sandbox without a new parameter on
//! every function. Nothing falls back: a launcher that fails fails the
//! operation.

use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::sandbox::{run, spec::Spec};

/// Turns the command git built (program, arguments, environment) into the
/// command to run.
pub trait Launcher: Send + Sync {
    fn launch(&self, cmd: &Command) -> io::Result<Command>;
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<dyn Launcher>>> = const { RefCell::new(None) };
}

/// Run `f`, a synchronous sequence of git operations, with every git command
/// started by this thread going through `launcher`.
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

/// Replace `cmd` by what the installed launcher makes of it, if any.
pub(super) fn apply(cmd: &mut Command) -> io::Result<()> {
    let Some(launcher) = CURRENT.with(|c| c.borrow().clone()) else {
        return Ok(());
    };
    *cmd = launcher.launch(cmd)?;
    Ok(())
}

/// Runs git inside the sandbox `spec` describes, through `crew sandbox-run`.
///
/// The git command's own variables (author and committer identity, the
/// prompt and editor switches) are non-secret and travel in the spec; the
/// launched process gets nothing else from the daemon's environment but the
/// names `spec::launcher_env` allows. A spec for a helper has no `pass_env`.
pub struct SandboxLauncher {
    spec: Spec,
}

impl SandboxLauncher {
    pub fn new(spec: Spec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }
}

impl Launcher for SandboxLauncher {
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
                &[],
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

/// A launcher for the helper work git does on behalf of no agent: filling a
/// new worktree in and merging every agent's branch for integration. Its
/// sandbox has the toolchain, the one worktree (writable) and the repository's
/// administrative files for it, and nothing of the daemon: no socket, no
/// state directory, no agent directory, no identity or model variable, and no
/// network. A hook or filter from any agent's branch can run in it, and has
/// no authority.
pub fn helper_launcher(
    host: &crate::sandbox::profile::Host,
    session: crate::sandbox::profile::Session<'_>,
    extra: &crate::sandbox::profile::Extra,
    worktree: &Path,
    admin: &Path,
) -> Result<(SandboxLauncher, Vec<String>), String> {
    use crate::sandbox::profile::{build, Request, Role};
    let built = build(
        host,
        &Request {
            session,
            role: Role::Helper { worktree, admin },
            extra,
            cli: None,
            pass_env: &[],
            terminal: false,
            nix_daemon: None,
        },
    )?;
    Ok((SandboxLauncher::new(built.spec), built.warnings))
}
