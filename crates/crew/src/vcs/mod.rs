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

pub mod jj;

pub use crate::git::{IntegrationStep, Kept, Picked, Worktree as Workspace};

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
    fn branch_exists(&self, branch: &str) -> bool {
        self.branch_commit(branch).is_some()
    }
    /// The commit the branch points at, if it exists. Runs on the
    /// repository, never inside a workspace.
    fn branch_commit(&self, branch: &str) -> Option<String>;

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

    fn branch_commit(&self, branch: &str) -> Option<String> {
        git::branch_commit(&self.repo, branch)
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

/// Why jj cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unusable {
    Absent,
    TooOld(String),
}

impl Unusable {
    /// "jj is not on PATH", "jj 0.41.0 is older than 0.44".
    fn reason(&self) -> String {
        match self {
            Unusable::Absent => "jj is not on PATH".to_string(),
            Unusable::TooOld(version) => format!(
                "jj {version} is older than {}.{}",
                jj::MIN_VERSION.0,
                jj::MIN_VERSION.1
            ),
        }
    }
}

/// The jj crew would run: `CREW_JJ` if it is set, else the first `jj` on
/// the daemon's PATH, absolute and canonical, with its version.
pub(crate) fn find_jj() -> Result<(PathBuf, String), Unusable> {
    find_jj_in(std::env::var_os("CREW_JJ"), std::env::var_os("PATH"))
}

/// `find_jj` with `CREW_JJ` and `PATH` given.
fn find_jj_in(
    crew_jj: Option<std::ffi::OsString>,
    path: Option<std::ffi::OsString>,
) -> Result<(PathBuf, String), Unusable> {
    let program = match crew_jj.filter(|v| !v.is_empty()) {
        Some(path) => PathBuf::from(path).canonicalize().ok(),
        None => path.and_then(|path| find_program(Some(&path), "jj")),
    }
    .ok_or(Unusable::Absent)?;
    let (shown, version) = jj::version_of(&program).ok_or(Unusable::Absent)?;
    if version < jj::MIN_VERSION {
        return Err(Unusable::TooOld(shown));
    }
    Ok((program, shown))
}

/// The directories under `worktrees` the git backend made in an earlier
/// session: a `.git` file and no `.jj`.
fn git_worktrees_left(worktrees: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return Vec::new();
    };
    let mut left: Vec<String> = entries
        .flatten()
        .filter(|e| {
            let path = e.path();
            std::fs::symlink_metadata(path.join(".git")).is_ok_and(|m| m.is_file())
                && !path.join(".jj").exists()
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    left.sort();
    left
}

/// Choose the backend for a workspace (`workspace.vcs`; notes/jj-design.md
/// D2). jj is the default when it is usable; a workspace keeps the backend
/// it was set up with, because neither tool can adopt the other's
/// directories. Err: a refusal, whose text says what to do.
pub fn select(choice: VcsChoice, ws: &Path, state_dir: &Path) -> Result<Selection> {
    select_with(choice, ws, state_dir, find_jj())
}

/// `select`, with the jj that was found (or why none can be used).
fn select_with(
    choice: VcsChoice,
    ws: &Path,
    state_dir: &Path,
    jj: Result<(PathBuf, String), Unusable>,
) -> Result<Selection> {
    let jj_state = state_dir.join("jj").exists();
    let left = git_worktrees_left(&state_dir.join("worktrees"));
    let native = is_jj_root(ws);
    let chosen = |program: &Path, version: &str| {
        let backend = jj::Jj::new(program, version, ws, state_dir);
        let note = if native {
            format!(
                "version control: jj {version} (the project's own jj repository; agent branches \
are exported to git as agents/<id>)"
            )
        } else {
            format!(
                "version control: jj {version} (state in .crew/jj; agent branches are exported to \
git as agents/<id>)"
            )
        };
        Selection {
            vcs: Arc::new(backend),
            note,
            fallback: false,
        }
    };
    let git = |note: String, fallback: bool| Selection {
        vcs: Arc::new(Git::new(ws)),
        note,
        fallback,
    };
    let sticky_git = || {
        format!(
            "version control: git, because .crew/worktrees holds git worktrees from earlier \
sessions ({}). To move to jj: stop the team, commit what is uncommitted there, run git worktree \
remove .crew/worktrees/<id> for each and start again (README, \"Moving a workspace to jj\"). The \
agents/<id> branches are kept.",
            left.join(", ")
        )
    };
    match choice {
        VcsChoice::Auto if jj_state => match &jj {
            Ok((program, version)) => Ok(chosen(program, version)),
            Err(why) => {
                let mut text = format!(
                    "this workspace is managed with jj (.crew/jj) and {}; install jj {}.{} or \
newer.",
                    why.reason(),
                    jj::MIN_VERSION.0,
                    jj::MIN_VERSION.1
                );
                if !native {
                    text.push_str(
                        " Going back to git needs jj once more, to publish the agents' work \
(README, \"Back to git\"); removing .crew/jj before that loses what is not on a git branch.",
                    );
                }
                bail!(text)
            }
        },
        VcsChoice::Auto if !left.is_empty() => Ok(git(sticky_git(), true)),
        VcsChoice::Auto => match &jj {
            Ok((program, version)) => Ok(chosen(program, version)),
            Err(Unusable::Absent) => Ok(git(
                format!(
                    "version control: git, because jj is not on PATH. Install jj {}.{} or newer \
and start the team again to use jj; workspace.vcs: git silences this.",
                    jj::MIN_VERSION.0,
                    jj::MIN_VERSION.1
                ),
                true,
            )),
            Err(why) => Ok(git(
                format!("version control: git, because {}.", why.reason()),
                true,
            )),
        },
        VcsChoice::Jj if !left.is_empty() => bail!(
            "workspace.vcs is jj, but .crew/worktrees holds git worktrees from earlier sessions \
({}); remove them (git worktree remove) or set workspace.vcs: git.",
            left.join(", ")
        ),
        VcsChoice::Jj => match &jj {
            Ok((program, version)) => Ok(chosen(program, version)),
            Err(why) => bail!("workspace.vcs is jj, but {}", why.reason()),
        },
        VcsChoice::Git if jj_state => {
            let mut text =
                "this workspace is managed with jj (.crew/jj); workspace.vcs: git would \
fork its history."
                    .to_string();
            if !native {
                text.push_str(
                    " To go back to git, publish every agent's work first (README, \"Back to \
git\").",
                );
            }
            bail!(text)
        }
        VcsChoice::Git => Ok(git(
            "version control: git (workspace.vcs: git)".to_string(),
            false,
        )),
    }
}

/// The backend a session was set up with, again (a daemon restart): no
/// selection, no notice. Empty or unknown is git, as before jj.
pub fn restore(kind: &str, ws: &Path, state_dir: &Path) -> Arc<dyn Vcs> {
    if kind != Kind::Jj.as_str() {
        return Arc::new(Git::new(ws));
    }
    // A jj that went away since fails each operation with its own error.
    let (program, version) = find_jj().unwrap_or_else(|_| (PathBuf::from("jj"), String::new()));
    Arc::new(jj::Jj::new(&program, &version, ws, state_dir))
}

thread_local! {
    /// What the operation running on this thread wants said besides its
    /// result (an export git could not take yet).
    static WARNINGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Say `text` besides the result of the operation on this thread.
pub(crate) fn warn(text: String) {
    WARNINGS.with(|w| w.borrow_mut().push(text));
}

/// What the operations on this thread said besides their results since
/// the last call.
pub fn take_warnings() -> Vec<String> {
    WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with a state directory, `.crew/jj` when `jj_state`, and
    /// a git worktree left in `.crew/worktrees/old` when `git_left`.
    fn workspace(jj_state: bool, git_left: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        let state = ws.join(".crew");
        std::fs::create_dir_all(state.join("worktrees")).unwrap();
        if jj_state {
            std::fs::create_dir_all(state.join("jj/.jj")).unwrap();
        }
        if git_left {
            let old = state.join("worktrees/old");
            std::fs::create_dir_all(&old).unwrap();
            std::fs::write(old.join(".git"), "gitdir: /somewhere\n").unwrap();
        }
        // A jj workspace is not a git worktree left behind.
        let jj_ws = state.join("worktrees/new");
        std::fs::create_dir_all(jj_ws.join(".jj")).unwrap();
        std::fs::write(
            jj_ws.join(".git"),
            "gitdir: /nonexistent/crew-jj-workspace\n",
        )
        .unwrap();
        (dir, ws, state)
    }

    fn usable() -> Result<(PathBuf, String), Unusable> {
        Ok((PathBuf::from("/opt/jj/bin/jj"), "0.44.0".to_string()))
    }

    fn too_old() -> Result<(PathBuf, String), Unusable> {
        Err(Unusable::TooOld("0.41.0".to_string()))
    }

    /// (backend, fallback, note) or the refusal.
    fn pick(
        choice: VcsChoice,
        jj_state: bool,
        git_left: bool,
        jj: Result<(PathBuf, String), Unusable>,
    ) -> Result<(Kind, bool, String), String> {
        let (_dir, ws, state) = workspace(jj_state, git_left);
        select_with(choice, &ws, &state, jj)
            .map(|s| (s.vcs.kind(), s.fallback, s.note))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn each_row_of_the_selection_table() {
        use VcsChoice::{Auto, Git as UseGit, Jj as UseJj};
        let absent = || Err(Unusable::Absent);
        // auto, .crew/jj, jj usable: jj
        let (kind, fallback, note) = pick(Auto, true, false, usable()).unwrap();
        assert_eq!((kind, fallback), (Kind::Jj, false));
        assert_eq!(
            note,
            "version control: jj 0.44.0 (state in .crew/jj; agent branches are exported to git \
as agents/<id>)"
        );
        // auto, .crew/jj, no jj: refused, and nothing suggests removing it
        let err = pick(Auto, true, false, absent()).unwrap_err();
        assert!(err.starts_with("this workspace is managed with jj (.crew/jj) and jj is not on PATH; install jj 0.44 or newer."), "{err}");
        assert!(err.contains("Back to git"), "{err}");
        let err = pick(Auto, true, true, too_old()).unwrap_err();
        assert!(err.contains("jj 0.41.0 is older than 0.44"), "{err}");
        // auto, no .crew/jj, git worktrees left: git, with notice
        let (kind, fallback, note) = pick(Auto, false, true, usable()).unwrap();
        assert_eq!((kind, fallback), (Kind::Git, true));
        assert!(
            note.contains("holds git worktrees from earlier sessions (old)"),
            "{note}"
        );
        assert!(note.contains("Moving a workspace to jj"), "{note}");
        // auto, nothing in the way, jj usable: jj
        assert_eq!(pick(Auto, false, false, usable()).unwrap().0, Kind::Jj);
        // auto, nothing in the way, no jj: git, with notice
        let (kind, fallback, note) = pick(Auto, false, false, absent()).unwrap();
        assert_eq!((kind, fallback), (Kind::Git, true));
        assert_eq!(
            note,
            "version control: git, because jj is not on PATH. Install jj 0.44 or newer and start \
the team again to use jj; workspace.vcs: git silences this."
        );
        let (_, _, note) = pick(Auto, false, false, too_old()).unwrap();
        assert_eq!(
            note,
            "version control: git, because jj 0.41.0 is older than 0.44."
        );
        // jj, nothing in the way, usable: jj
        assert_eq!(pick(UseJj, false, false, usable()).unwrap().0, Kind::Jj);
        assert_eq!(pick(UseJj, true, false, usable()).unwrap().0, Kind::Jj);
        // jj, git worktrees left: refused
        let err = pick(UseJj, false, true, usable()).unwrap_err();
        assert!(err.contains("workspace.vcs is jj, but .crew/worktrees holds git worktrees from earlier sessions (old)"), "{err}");
        // jj, no jj: refused
        assert_eq!(
            pick(UseJj, false, false, absent()).unwrap_err(),
            "workspace.vcs is jj, but jj is not on PATH"
        );
        assert_eq!(
            pick(UseJj, false, false, too_old()).unwrap_err(),
            "workspace.vcs is jj, but jj 0.41.0 is older than 0.44"
        );
        // git, no .crew/jj: git, silent
        let (kind, fallback, _) = pick(UseGit, false, true, usable()).unwrap();
        assert_eq!((kind, fallback), (Kind::Git, false));
        // git, .crew/jj: refused
        let err = pick(UseGit, true, false, usable()).unwrap_err();
        assert!(
            err.contains("workspace.vcs: git would fork its history"),
            "{err}"
        );
        assert!(err.contains("Back to git"), "{err}");
    }

    #[test]
    fn the_native_layout_has_its_own_texts() {
        let (_dir, ws, state) = workspace(false, false);
        std::fs::create_dir_all(ws.join(".jj")).unwrap();
        let chosen = select_with(VcsChoice::Auto, &ws, &state, usable()).unwrap();
        assert!(
            chosen.note.contains("the project's own jj repository"),
            "{}",
            chosen.note
        );
        std::fs::create_dir_all(state.join("jj")).unwrap();
        let err = select_with(VcsChoice::Auto, &ws, &state, Err(Unusable::Absent))
            .err()
            .unwrap()
            .to_string();
        assert!(err.ends_with("install jj 0.44 or newer."), "{err}");
        let err = select_with(VcsChoice::Git, &ws, &state, usable())
            .err()
            .unwrap()
            .to_string();
        assert!(err.ends_with("would fork its history."), "{err}");
    }

    #[test]
    fn jj_is_found_by_crew_jj_or_on_path_and_must_be_new_enough() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, says: &str| {
            let bin = dir.path().join(name);
            std::fs::create_dir_all(&bin).unwrap();
            let path = bin.join("jj");
            std::fs::write(&path, format!("#!/bin/sh\necho '{says}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let new = fake("new", "jj 0.44.0");
        let old = fake("old", "jj 0.41.0");
        let odd = fake("odd", "something else");
        let found = find_jj_in(Some(new.clone().into()), None).unwrap();
        assert_eq!(found, (new.canonicalize().unwrap(), "0.44.0".to_string()));
        assert_eq!(
            find_jj_in(Some(old.clone().into()), None),
            Err(Unusable::TooOld("0.41.0".to_string()))
        );
        assert_eq!(find_jj_in(Some(odd.into()), None), Err(Unusable::Absent));
        assert_eq!(
            find_jj_in(Some(dir.path().join("nothing").into()), None),
            Err(Unusable::Absent)
        );
        // On PATH, the first one; CREW_JJ wins over PATH.
        let path = std::env::join_paths([old.parent().unwrap(), new.parent().unwrap()]).unwrap();
        assert!(matches!(
            find_jj_in(None, Some(path.clone())),
            Err(Unusable::TooOld(_))
        ));
        assert!(find_jj_in(Some(new.into()), Some(path)).is_ok());
        assert_eq!(find_jj_in(None, None), Err(Unusable::Absent));
    }
}
