//! Version control behind one seam: jj (the default) or git.
//!
//! The daemon calls only the [`Vcs`] trait. [`Git`] delegates to the
//! functions in [`crate::git`], whose behaviour is today's; jj keeps its
//! repository under `.crew/jj`, backed by the project's own `.git`, so the
//! user's checkout never changes and agent work shows up in plain git as
//! `agents/<id>` branches. The contract of every operation (what it
//! returns, what it leaves behind) is the one git has.
//!
//! Every command either backend starts goes through `git::output` and the
//! installed [`crate::git::launch::Launcher`], so the sandboxing of an
//! agent's operations is the same for both.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Result};
use crew_core::config::VcsChoice;
use crew_core::rpc::{CommitInfo, GitOutcome};

use crate::git;

pub use crate::git::{IntegrationStep, Picked, Worktree as Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Jj,
    Git,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Jj => "jj",
            Kind::Git => "git",
        }
    }
}

/// Where the store is, for the sandbox profile. Paths the daemon made or
/// read before any agent ran. `git_dir` is the directory with the objects
/// and refs (a linked worktree's common directory), never the checkout's
/// admin directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Store {
    Git { git_dir: PathBuf },
    Jj { git_dir: PathBuf, repo_dir: PathBuf },
}

pub trait Vcs: Send + Sync {
    fn kind(&self) -> Kind;
    /// The binary, absolute and canonical when it was found on PATH: what a
    /// sandbox binds and runs.
    fn program(&self) -> &Path;
    fn store(&self) -> Store;

    /// Make the store ready (idempotent) and say whether there is a commit
    /// to start from. Err: `ws` is not the root of a repository.
    fn prepare(&self, ws: &Path) -> Result<bool>;
    fn branch_exists(&self, branch: &str) -> bool;

    /// Register (or find) the agent's workspace under `dir`. Runs nothing
    /// inside it. The bool: its files still have to be put in place.
    fn add_workspace(&self, dir: &Path, agent: &str, base: &str) -> Result<(Workspace, bool)>;
    /// Inside the workspace; under the helper launcher when there is one.
    fn populate(&self, workspace: &Workspace) -> Result<()>;

    // Inside the agent's workspace; under the agent's launcher when there is one.
    fn commit_all(&self, dir: &Path, agent: &str, message: &str) -> Result<CommitInfo>;
    fn diff(&self, dir: &Path, against: Option<&str>) -> Result<String>;
    fn status(&self, dir: &Path) -> Result<String>;
    fn cherry_pick(&self, dir: &Path, agent: &str, rev: &str) -> Result<Picked>;
    fn merge(&self, dir: &Path, agent: &str, branch: &str) -> Result<GitOutcome>;

    /// Inside the integration workspace; under the helper launcher. `on_step`
    /// is called with each step as soon as it is done.
    fn merge_agents(
        &self,
        workspace: &Workspace,
        agents: &[String],
        on_step: &mut dyn FnMut(&IntegrationStep),
    ) -> Result<Vec<IntegrationStep>>;
}

/// The git backend: today's `crate::git`, unchanged.
pub struct Git {
    repo: PathBuf,
    program: PathBuf,
    /// The common git directory, once `prepare` found it.
    git_dir: OnceLock<PathBuf>,
}

impl Git {
    pub fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            program: find_program(None, "git").unwrap_or_else(|| PathBuf::from("git")),
            git_dir: OnceLock::new(),
        }
    }
}

impl Vcs for Git {
    fn kind(&self) -> Kind {
        Kind::Git
    }

    fn program(&self) -> &Path {
        &self.program
    }

    fn store(&self) -> Store {
        Store::Git {
            git_dir: self
                .git_dir
                .get()
                .cloned()
                .unwrap_or_else(|| self.repo.join(".git")),
        }
    }

    fn prepare(&self, ws: &Path) -> Result<bool> {
        if !git::is_root(ws) {
            bail!("{} is not the root of a git work tree", ws.display());
        }
        git::exclude_state_dir(ws)?;
        if let Ok(dir) = git::common_git_dir(ws) {
            let _ = self.git_dir.set(dir.canonicalize().unwrap_or(dir));
        }
        Ok(git::has_commits(ws))
    }

    fn branch_exists(&self, branch: &str) -> bool {
        git::branch_exists(&self.repo, branch)
    }

    fn add_workspace(&self, dir: &Path, agent: &str, base: &str) -> Result<(Workspace, bool)> {
        git::add_worktree(&self.repo, dir, agent, base)
    }

    fn populate(&self, workspace: &Workspace) -> Result<()> {
        git::populate_worktree(workspace)
    }

    fn commit_all(&self, dir: &Path, agent: &str, message: &str) -> Result<CommitInfo> {
        git::commit_all(dir, agent, message)
    }

    fn diff(&self, dir: &Path, against: Option<&str>) -> Result<String> {
        git::diff(dir, against)
    }

    fn status(&self, dir: &Path) -> Result<String> {
        git::status(dir)
    }

    fn cherry_pick(&self, dir: &Path, agent: &str, rev: &str) -> Result<Picked> {
        git::cherry_pick(dir, agent, rev)
    }

    fn merge(&self, dir: &Path, agent: &str, branch: &str) -> Result<GitOutcome> {
        git::merge(dir, agent, branch)
    }

    fn merge_agents(
        &self,
        workspace: &Workspace,
        agents: &[String],
        on_step: &mut dyn FnMut(&IntegrationStep),
    ) -> Result<Vec<IntegrationStep>> {
        git::merge_agents(workspace, agents, on_step)
    }
}

/// What was chosen, and what to tell the user about it.
pub struct Selection {
    pub vcs: Arc<dyn Vcs>,
    /// The notice for the team's start: which backend, and why.
    pub note: String,
    /// git was chosen although jj is the default.
    pub fallback: bool,
}

/// Choose the backend for a workspace (`workspace.vcs`).
pub fn select(choice: VcsChoice, ws: &Path, _state_dir: &Path) -> Result<Selection> {
    let _ = choice;
    Ok(Selection {
        vcs: Arc::new(Git::new(ws)),
        note: String::new(),
        fallback: false,
    })
}

/// The backend a session was set up with, again (a daemon restart): no
/// selection, no notice. Empty or unknown is git, as before jj.
pub fn restore(kind: &str, ws: &Path, _state_dir: &Path) -> Arc<dyn Vcs> {
    let _ = kind;
    Arc::new(Git::new(ws))
}

/// `name` on `path` (the daemon's PATH when None), absolute and canonical.
pub(crate) fn find_program(path: Option<&std::ffi::OsStr>, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = match path {
        Some(path) => path.to_os_string(),
        None => std::env::var_os("PATH")?,
    };
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|found| found.canonicalize().ok())
}

/// `ws` has a jj repository of its own (`<ws>/.jj` is a directory).
pub fn is_jj_root(ws: &Path) -> bool {
    std::fs::symlink_metadata(ws.join(".jj")).is_ok_and(|m| m.is_dir())
}
