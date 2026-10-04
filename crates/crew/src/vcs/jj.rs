//! The jj backend (notes/jj-design.md §layout, §ops, §conflicts).
//!
//! In a plain-git project the jj repository lives in `.crew/jj` (the
//! "handle"), created with `jj git init --git-repo <git>`, so its commits are
//! objects of the project's own `.git` and the user's checkout is never
//! written. A project that already has `.jj` in its root is used as it is.
//! Each agent works in a jj workspace at `.crew/worktrees/<id>`; a bookmark
//! `agents/<id>` marks its last commit and is exported to git after every
//! move, so plain git sees the agent's work as a branch.
//!
//! jj does not stop at a conflict: it records the conflict in the commit it
//! made. A cherry-pick or merge is therefore run, the commit it made is
//! inspected, and a conflicted result is taken back by undoing exactly what
//! that one operation did. A commit a bookmark points at is never abandoned,
//! and a result that landed clean is kept, even after a timeout.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::rpc::{CommitInfo, ConflictReport, GitOutcome};
use crew_core::util::is_valid_id;

use super::{IntegrationStep, Kind, Picked, Store, Vcs, Workspace};
use crate::git::{self, beneath, BRANCH_PREFIX, INTEGRATION_BRANCH, INTEGRATION_DIR};

/// The oldest jj the argvs and templates below were verified against.
pub const MIN_VERSION: (u32, u32, u32) = (0, 44, 0);

/// What every jj command crew starts is pinned to (§ops, "Invocation"). Each
/// `--config` value is TOML, so strings carry their quotes.
const PINS: &[&str] = &[
    "--no-pager",
    "--color",
    "never",
    "--quiet",
    "--config",
    "signing.behavior=drop",
    "--config",
    "signing.backend=none",
    "--config",
    "fsmonitor.backend=none",
    "--config",
    "ui.editor=\"true\"",
    "--config",
    "ui.diff-formatter=\":git\"",
    "--config",
    "git.abandon-unreachable-commits=false",
    "--config",
    "snapshot.auto-track=\"all()\"",
    "--config",
    "snapshot.max-new-file-size=\"1TiB\"",
    "--config",
    "snapshot.auto-update-stale=false",
];

/// The one command that runs in the user's own jj workspace (the native
/// layout's bootstrap) keeps their configuration: it snapshots their working
/// copy. Only what could block or run a program is pinned.
const BOOTSTRAP_PINS: &[&str] = &[
    "--no-pager",
    "--color",
    "never",
    "--quiet",
    "--config",
    "signing.behavior=drop",
    "--config",
    "signing.backend=none",
    "--config",
    "fsmonitor.backend=none",
    "--config",
    "ui.editor=\"true\"",
];

/// Variables of the daemon's environment that would point jj's git at
/// another repository.
const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// `CommitInfo` of one commit: id, change id, subject, files changed.
const INFO: &str = r#"commit_id ++ "\n" ++ change_id ++ "\n" ++ description.first_line() ++ "\n" ++ self.diff().files().len() ++ "\n""#;
/// What an operation just made: id, conflicted, empty, its bookmarks, then
/// one conflicted path per line.
const STATE: &str = r#"commit_id ++ "\n" ++ conflict ++ "\n" ++ empty ++ "\n" ++ local_bookmarks.map(|b| b.name()).join(" ") ++ "\n" ++ conflicted_files.map(|f| f.path()).join("\n") ++ "\n""#;
/// The first command of every state-changing operation: it snapshots the
/// working copy and says whether it is clean and what its parent is.
const PRE: &str = r#"empty ++ "\n" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n""#;
/// git's `cherry-pick -x` trailer, for `jj duplicate`.
const DUP: &str = r#"templates.duplicate_description='description ++ "\n(cherry picked from commit " ++ commit_id ++ ")\n"'"#;
/// The file in each agent workspace that stops plain git from walking up to
/// the user's checkout (D9). jj ignores it.
const GUARD: &[u8] = b"gitdir: /nonexistent/crew-jj-workspace\n";
const ROOT_COMMIT: &str = "0000000000000000000000000000000000000000";
const MAX_DIFF_BYTES: usize = 48 * 1024;
/// The identity of what crew does for nobody in particular.
const CREW: &str = "crew";

/// The jj backend of one workspace.
pub struct Jj {
    program: PathBuf,
    version: String,
    ws: PathBuf,
    /// `.crew/jj`: what `-R` names for repository-level commands.
    handle: PathBuf,
    /// The project root has a jj repository of its own.
    native: bool,
    dirs: OnceLock<Dirs>,
}

/// `<git>` and `<admin>` of §layout.
#[derive(Clone, Debug)]
struct Dirs {
    /// The backing git directory: objects and refs.
    git: PathBuf,
    /// The checkout's own git directory (its `HEAD`); hidden layout only.
    admin: Option<PathBuf>,
}

/// An operation that changes history, for the recovery after a timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Commit,
    CherryPick,
    Merge,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Operation::Commit => "commit",
            Operation::CherryPick => "cherry-pick",
            Operation::Merge => "merge",
        }
    }
}

/// `STATE` of a commit.
#[derive(Clone, Debug, Default)]
struct State {
    commit: String,
    conflict: bool,
    empty: bool,
    bookmarks: Vec<String>,
    files: Vec<String>,
}

impl State {
    fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        let commit = lines.next().unwrap_or_default().trim().to_string();
        if commit.is_empty() {
            bail!("jj described no commit");
        }
        let flag = |l: Option<&str>| l.map(str::trim) == Some("true");
        let conflict = flag(lines.next());
        let empty = flag(lines.next());
        let bookmarks = lines
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let files = lines
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        Ok(Self {
            commit,
            conflict,
            empty,
            bookmarks,
            files,
        })
    }

    fn short(&self) -> &str {
        short(&self.commit)
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Whether `e` is (or wraps) a command that ran out of time.
fn timed_out(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut)
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// A name of a branch, bookmark or tag as an agent may pass it: no revset
/// syntax, nothing that could be read as an option.
fn is_plain_name(rev: &str) -> bool {
    !rev.is_empty()
        && rev.len() <= 100
        && !rev.starts_with('-')
        && !rev.contains("..")
        && rev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/' | b'.'))
}

/// The revset for a revision an agent passed (§ops, "Revisions an agent
/// passes"). Agent text is never passed to jj as a revset: it is matched
/// against these forms and rewritten into a lookup.
pub fn revset(rev: &str) -> Result<String> {
    let is_hex = (7..=64).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit());
    let is_change =
        (7..=32).contains(&rev.len()) && rev.bytes().all(|b| (b'k'..=b'z').contains(&b));
    if is_hex {
        return Ok(format!("commit_id({})", rev.to_ascii_lowercase()));
    }
    if is_change {
        return Ok(format!("change_id({rev})"));
    }
    if rev.strip_prefix(BRANCH_PREFIX).is_some_and(is_valid_id) {
        return Ok(format!("\"{rev}\""));
    }
    if rev == "HEAD" {
        return Ok("@-".to_string());
    }
    if is_plain_name(rev) {
        return Ok(format!("\"{rev}\""));
    }
    bail!("invalid revision '{rev}' (use a commit id, a change id or a branch name)")
}

/// `(major, minor, patch)` of `jj X.Y.Z…`.
pub fn parse_version(line: &str) -> Option<(u32, u32, u32)> {
    let rest = line.trim().strip_prefix("jj ")?;
    let mut parts = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// `jj --version` of `program`: its `X.Y.Z`, or None when it does not run
/// or says something else.
pub fn version_of(program: &Path) -> Option<(String, (u32, u32, u32))> {
    let mut cmd = Command::new(program);
    cmd.arg("--version")
        .env("JJ_CONFIG", "/dev/null")
        .stdin(Stdio::null());
    let output = git::output(&mut cmd).ok()?;
    if !output.status.success() {
        return None;
    }
    let line = text(&output.stdout);
    let version = parse_version(&line)?;
    let shown = line.strip_prefix("jj ").unwrap_or(&line).to_string();
    let shown = shown
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    Some((shown, version))
}

impl Jj {
    /// The backend for `ws`, with its state under `state_dir`, running
    /// `program` (version `version`). Nothing is run.
    pub fn new(program: &Path, version: &str, ws: &Path, state_dir: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            version: version.to_string(),
            ws: ws.to_path_buf(),
            handle: state_dir.join("jj"),
            native: super::is_jj_root(ws),
            dirs: OnceLock::new(),
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn is_native(&self) -> bool {
        self.native
    }

    /// The handle: `.crew/jj`.
    pub fn handle(&self) -> &Path {
        &self.handle
    }

    /// A jj command run as `who` on the repository or workspace at `dir`,
    /// from that directory.
    fn cmd(&self, dir: &Path, who: &str) -> Command {
        let mut cmd = self.bare(PINS, who);
        cmd.arg("-R").arg(dir).current_dir(dir);
        cmd
    }

    fn bare(&self, pins: &[&str], who: &str) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(pins)
            .env("JJ_USER", who)
            .env("JJ_EMAIL", format!("{who}@crew.local"))
            .stdin(Stdio::null());
        if pins == PINS {
            // No user configuration: their aliases, templates and tools do
            // not apply.
            cmd.env("JJ_CONFIG", "/dev/null");
        }
        for name in GIT_ENV {
            cmd.env_remove(name);
        }
        cmd
    }

    fn run(cmd: &mut Command, args: &[&str]) -> Result<Output> {
        cmd.args(args);
        let sub = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .copied()
            .unwrap_or("");
        git::output(cmd).with_context(|| format!("running jj {sub}"))
    }

    /// Run `cmd args`; a non-zero exit is an error carrying jj's message.
    fn out_of(mut cmd: Command, args: &[&str]) -> Result<String> {
        let output = Self::run(&mut cmd, args)?;
        let sub = args
            .iter()
            .filter(|a| !a.starts_with('-'))
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if !output.status.success() {
            let err = text(&output.stderr);
            let err = if err.is_empty() {
                text(&output.stdout)
            } else {
                err
            };
            bail!("jj {sub} failed: {err}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `jj -R dir args` as `who`, its standard output.
    fn out(&self, dir: &Path, who: &str, args: &[&str]) -> Result<String> {
        Self::out_of(self.cmd(dir, who), args)
    }

    /// A repository-level command on the handle, which has no working copy.
    fn repo(&self, args: &[&str]) -> Result<String> {
        let mut all = vec!["--ignore-working-copy"];
        all.extend_from_slice(args);
        self.out(&self.handle, CREW, &all)
    }

    /// `<git>` and `<admin>`, found once.
    fn dirs(&self) -> Result<&Dirs> {
        if let Some(dirs) = self.dirs.get() {
            return Ok(dirs);
        }
        let dirs = if self.native {
            let root = self.repo(&["git", "root"])?;
            let git = PathBuf::from(root.trim());
            Dirs {
                git: common_dir(&git)?,
                admin: None,
            }
        } else {
            hidden_dirs(&self.ws)?
        };
        Ok(self.dirs.get_or_init(|| dirs))
    }

    fn repo_dir(&self) -> PathBuf {
        let base = if self.native { &self.ws } else { &self.handle };
        let repo = base.join(".jj").join("repo");
        repo.canonicalize().unwrap_or(repo)
    }

    /// The commit ids of `revset`, one per line.
    fn ids(&self, dir: &Path, who: &str, revset: &str) -> Result<Vec<String>> {
        let out = self.out(
            dir,
            who,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                revset,
                "-T",
                "commit_id ++ \"\\n\"",
            ],
        )?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The commit the bookmark `name` points at, if it exists.
    fn bookmark(&self, dir: &Path, who: &str, name: &str) -> Result<Option<String>> {
        let ids = self.ids(dir, who, &format!("present(bookmarks(exact:\"{name}\"))"))?;
        match ids.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            _ => bail!("the bookmark {name} is conflicted; resolve it with jj bookmark set"),
        }
    }

    /// Exactly one commit for an agent's revision, else `unknown`.
    fn resolve(&self, dir: &Path, who: &str, rev: &str, unknown: &str) -> Result<String> {
        let revset = revset(rev)?;
        match self.ids(dir, who, &revset) {
            Ok(ids) if ids.len() == 1 => Ok(ids[0].clone()),
            Ok(ids) if ids.len() > 1 => bail!("'{rev}' names more than one commit"),
            Err(e) if timed_out(&e) => Err(e),
            _ => bail!("{unknown} '{rev}'"),
        }
    }

    /// The commit `HEAD` of the project stands for (§ops, "Base").
    fn head(&self) -> Result<Option<String>> {
        if self.native {
            let ids = self.ids(&self.ws, CREW, "@-")?;
            return match ids.as_slice() {
                [] => Ok(None),
                [one] if one == ROOT_COMMIT => Ok(None),
                [one] => Ok(Some(one.clone())),
                _ => bail!(
                    "the working copy of {} has more than one parent; set workspace.base",
                    self.ws.display()
                ),
            };
        }
        // The checkout's own HEAD (for a linked worktree not the main
        // checkout's): jj cannot be asked for git's HEAD in a repository
        // that is not colocated.
        let dirs = self.dirs()?;
        let admin = dirs.admin.as_deref().unwrap_or(&dirs.git);
        let head = beneath::Root::open(admin)
            .with_context(|| format!("opening {}", admin.display()))?
            .read_text("HEAD")
            .ok_or_else(|| anyhow!("{} has no HEAD", admin.display()))?;
        let head = head.trim();
        if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
            if !is_plain_name(branch) {
                bail!(
                    "HEAD names the branch '{branch}', which crew cannot use; set workspace.base"
                );
            }
            return self.bookmark(&self.handle, CREW, branch);
        }
        if (40..=64).contains(&head.len()) && head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Some(head.to_string()));
        }
        bail!(
            "cannot read HEAD of {}; set workspace.base",
            self.ws.display()
        )
    }

    /// The commit a new agent branch starts from.
    fn base(&self, base: &str) -> Result<String> {
        if base == "HEAD" {
            return self
                .head()?
                .ok_or_else(|| anyhow!("HEAD has no commit yet: there is nothing to start from"));
        }
        self.resolve(&self.handle, CREW, base, "unknown revision")
    }

    /// The names of the workspaces jj knows.
    fn workspaces(&self) -> Result<Vec<String>> {
        Ok(self
            .repo(&["workspace", "list", "-T", "name ++ \"\\n\""])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn prepare_hidden(&self) -> Result<()> {
        let dirs = self.dirs()?.clone();
        if !self.handle.join(".jj").is_dir() {
            let git_dir = dirs.git.to_string_lossy().into_owned();
            let handle = self.handle.to_string_lossy().into_owned();
            let mut cmd = self.bare(PINS, CREW);
            cmd.current_dir(&self.ws);
            Self::out_of(cmd, &["git", "init", "--git-repo", &git_dir, &handle])?;
        }
        // `jj git init` checked the project out into the handle: its files
        // go again, and so does its working-copy commit. Both steps are
        // repeated until the workspace is gone, so a start cut short is
        // finished by the next one.
        if self.workspaces()?.iter().any(|w| w == "default") {
            self.out(&self.handle, CREW, &["sparse", "set", "--clear"])?;
            self.repo(&["workspace", "forget", "default"])?;
        }
        self.repo(&["git", "import"])?;
        Ok(())
    }

    fn prepare_native(&self) -> Result<()> {
        if !self.handle.join(".jj").is_dir() {
            // The one command in the user's own workspace: it snapshots their
            // working copy, with their own configuration.
            let mut cmd = self.bare(BOOTSTRAP_PINS, CREW);
            cmd.arg("-R").arg(&self.ws).current_dir(&self.ws);
            let handle = self.handle.to_string_lossy().into_owned();
            Self::out_of(
                cmd,
                &[
                    "workspace",
                    "add",
                    "--name",
                    CREW,
                    "--sparse-patterns",
                    "empty",
                    "-r",
                    "root()",
                    &handle,
                ],
            )?;
        }
        if self.workspaces()?.iter().any(|w| w == CREW) {
            self.repo(&["workspace", "forget", CREW])?;
        }
        Ok(())
    }

    /// The bookmark and the identity of a workspace directory's name.
    fn owner(name: &str) -> Result<(String, &str)> {
        if name == INTEGRATION_DIR {
            return Ok((INTEGRATION_BRANCH.to_string(), CREW));
        }
        if !is_valid_id(name) {
            bail!("invalid agent id '{name}'");
        }
        Ok((git::branch_for(name), name))
    }

    /// `PRE` of the working copy: (clean, parents).
    fn pre(&self, dir: &Path, who: &str) -> Result<(bool, Vec<String>)> {
        let out = self.out(dir, who, &["log", "--no-graph", "-r", "@", "-T", PRE])?;
        let mut lines = out.lines();
        let clean = lines.next().map(str::trim) == Some("true");
        let parents = lines
            .next()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        Ok((clean, parents))
    }

    fn state(&self, dir: &Path, who: &str) -> Result<State> {
        State::parse(&self.out(
            dir,
            who,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                "@-",
                "-T",
                STATE,
            ],
        )?)
    }

    fn info(&self, dir: &Path, who: &str, bookmark: &str) -> Result<CommitInfo> {
        let out = self.out(
            dir,
            who,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                "@-",
                "-T",
                INFO,
            ],
        )?;
        commit_info(&out, bookmark)
    }

    /// The publish phase: the bookmark follows the new commit, and git sees
    /// it. A ref git could not write is a warning of jj's (exit 0); the next
    /// export retries it.
    fn publish(&self, dir: &Path, who: &str, bookmark: &str) -> Result<()> {
        self.out(dir, who, &["bookmark", "set", bookmark, "-r", "@-"])?;
        self.export(dir, who)
    }

    fn export(&self, dir: &Path, who: &str) -> Result<()> {
        self.out(dir, who, &["git", "export"]).map(drop)
    }

    /// Undo what a conflicted (or empty) cherry-pick or merge did; the
    /// commit it made carries no bookmark (callers checked).
    fn roll_back(
        &self,
        dir: &Path,
        who: &str,
        op: Operation,
        made: &State,
        parent: &str,
    ) -> Result<()> {
        match op {
            Operation::CherryPick => self.out(dir, who, &["abandon", "@-"]).map(drop),
            Operation::Merge => {
                let onto = format!("commit_id({parent})");
                self.out(dir, who, &["rebase", "-r", "@", "--onto", &onto])?;
                // If this fails, a merge commit without a bookmark is left
                // over: no branch points at it and plain git does not see it.
                let merge = format!("commit_id({})", made.commit);
                if let Err(e) = self.out(dir, who, &["abandon", &merge]) {
                    tracing::warn!("leaving the merge commit {}: {e:#}", made.short());
                }
                Ok(())
            }
            Operation::Commit => Ok(()),
        }
    }

    /// After a command of `op` timed out (§conflicts, "Timeouts"): read the
    /// state and finish the phase the operation was in. `parent` is what
    /// `PRE` recorded before the mutation. Always an error, saying what was
    /// found.
    fn recover(
        &self,
        dir: &Path,
        who: &str,
        bookmark: &str,
        op: Operation,
        parent: &str,
        err: &anyhow::Error,
    ) -> anyhow::Error {
        let cause = err
            .chain()
            .find_map(|c| c.downcast_ref::<io::Error>().map(|e| e.to_string()))
            .unwrap_or_else(|| format!("{err:#}"));
        let name = op.name();
        let failed = || {
            anyhow!(
                "jj {name} timed out ({cause}) and the worktree could not be restored; check jj \
status"
            )
        };
        let _ = self.out(dir, who, &["workspace", "update-stale"]);
        let Ok(found) = self.state(dir, who) else {
            return failed();
        };
        self.remove_ref_lock(bookmark);
        if found.commit == parent {
            if self.export(dir, who).is_err() {
                return failed();
            }
            return match op {
                Operation::Commit => anyhow!(
                    "jj commit timed out ({cause}); nothing was committed, the changes are still \
in the working copy"
                ),
                _ => {
                    anyhow!("jj {name} timed out ({cause}); the worktree was restored, retry later")
                }
            };
        }
        let undo = op != Operation::Commit
            && (found.conflict || (op == Operation::CherryPick && found.empty));
        if undo {
            if !found.bookmarks.is_empty() {
                return anyhow!(
                    "jj {name} timed out ({cause}); the {name} conflicts, and {bookmark} already \
points at the result; it was left as it is, check jj status"
                );
            }
            if self.roll_back(dir, who, op, &found, parent).is_err()
                || self.export(dir, who).is_err()
            {
                return failed();
            }
            return anyhow!(
                "jj {name} timed out ({cause}); the worktree was restored, retry later"
            );
        }
        if self.publish(dir, who, bookmark).is_err() {
            return failed();
        }
        let short = found.short();
        match op {
            Operation::Commit => anyhow!(
                "jj commit timed out ({cause}) after the commit was made: {short} is on \
{bookmark}; check the history"
            ),
            _ => anyhow!(
                "jj {name} timed out ({cause}) after it completed: {short} is on {bookmark}; the \
result was kept, check the history before retrying"
            ),
        }
    }

    /// A `.lock` git export left behind when it was killed. Removed through
    /// a descriptor of `<git>`, by a path the daemon builds.
    fn remove_ref_lock(&self, bookmark: &str) {
        let Ok(dirs) = self.dirs() else { return };
        if let Ok(root) = beneath::Root::open(&dirs.git) {
            root.remove(&format!("refs/heads/{bookmark}.lock"));
        }
    }

    /// Run the steps of `op` after `PRE`; a timeout anywhere is recovered
    /// from by what the repository says.
    fn guarded<T>(
        &self,
        dir: &Path,
        who: &str,
        bookmark: &str,
        op: Operation,
        parent: &str,
        steps: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        match steps() {
            Err(e) if timed_out(&e) => Err(self.recover(dir, who, bookmark, op, parent, &e)),
            other => other,
        }
    }

    /// `PRE` for an operation that needs a clean working copy with one
    /// parent: that parent.
    fn clean_parent(&self, dir: &Path, who: &str, op: Operation) -> Result<String> {
        let (clean, parents) = self.pre(dir, who).map_err(|e| {
            if timed_out(&e) {
                anyhow!(
                    "jj {} timed out ({e:#}); nothing was changed, retry later",
                    op.name()
                )
            } else {
                e
            }
        })?;
        if !clean {
            bail!(
                "the worktree has uncommitted changes; commit them before {}",
                match op {
                    Operation::CherryPick => "cherry-picking",
                    _ => "merging",
                }
            );
        }
        match parents.as_slice() {
            [one] => Ok(one.clone()),
            _ => bail!(
                "the working copy has {} parents; crew needs exactly one (check jj log)",
                parents.len()
            ),
        }
    }

    /// Merge `branch` into the workspace at `dir`, moving `bookmark` (and
    /// exporting it when `export`). Missing: Ok(None).
    fn merge_into(
        &self,
        dir: &Path,
        who: &str,
        bookmark: &str,
        branch: &str,
        export: bool,
    ) -> Result<Option<GitOutcome>> {
        if !is_plain_name(branch) {
            bail!("invalid branch '{branch}'");
        }
        let parent = self.clean_parent(dir, who, Operation::Merge)?;
        if self.bookmark(dir, who, branch)?.is_none() {
            return Ok(None);
        }
        let ahead = self.out(
            dir,
            who,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                &format!("\"{branch}\" ~ ::@-"),
                "-T",
                "\"x\\n\"",
            ],
        )?;
        if ahead.trim().is_empty() {
            // jj would make a merge with a redundant parent.
            return Ok(Some(GitOutcome {
                ok: true,
                commit: Some(self.info(dir, who, bookmark)?),
                conflict: None,
                text: "Already up to date.".to_string(),
            }));
        }
        self.guarded(dir, who, bookmark, Operation::Merge, &parent, || {
            let message = format!("--message=Merge {branch}");
            let other = format!("\"{branch}\"");
            self.out(
                dir,
                who,
                &[
                    "new",
                    "--no-edit",
                    &message,
                    "--insert-after",
                    "@-",
                    "--insert-after",
                    &other,
                    "--insert-before",
                    "@",
                ],
            )?;
            let made = self.state(dir, who)?;
            if made.conflict {
                return self
                    .conflicted(dir, who, bookmark, Operation::Merge, &made, &parent, branch)
                    .map(Some);
            }
            if export {
                self.publish(dir, who, bookmark)?;
            } else {
                self.out(dir, who, &["bookmark", "set", bookmark, "-r", "@-"])?;
            }
            Ok(Some(GitOutcome {
                ok: true,
                commit: Some(self.info(dir, who, bookmark)?),
                conflict: None,
                text: format!("merged {branch} as {}", made.short()),
            }))
        })
    }

    /// A cherry-pick or merge whose result conflicts: rolled back, unless a
    /// bookmark already points at it.
    #[allow(clippy::too_many_arguments)]
    fn conflicted(
        &self,
        dir: &Path,
        who: &str,
        bookmark: &str,
        op: Operation,
        made: &State,
        parent: &str,
        source: &str,
    ) -> Result<GitOutcome> {
        let name = op.name();
        if !made.bookmarks.is_empty() {
            bail!(
                "{name} conflicts, and {bookmark} already points at the result; it was left as it \
is, check jj status"
            );
        }
        self.roll_back(dir, who, op, made, parent)?;
        Ok(GitOutcome {
            ok: false,
            commit: None,
            conflict: Some(ConflictReport {
                operation: name.to_string(),
                detail: format!(
                    "{name} of {source} conflicts in {} file(s)",
                    made.files.len()
                ),
                files: made.files.clone(),
            }),
            text: format!("{name} aborted because of conflicts; the worktree is unchanged"),
        })
    }

    fn truncate_diff(mut s: String) -> String {
        if s.len() > MAX_DIFF_BYTES {
            let mut cut = MAX_DIFF_BYTES;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            s.push_str("\n… (diff truncated)");
        }
        s
    }

    /// The identity a workspace directory acts as.
    fn identity_of(workspace: &Workspace) -> String {
        match workspace.path.file_name().and_then(|n| n.to_str()) {
            Some(INTEGRATION_DIR) | None => CREW.to_string(),
            Some(name) => name.to_string(),
        }
    }
}

/// The identity jj runs as in the workspace at `dir`: the agent it's
/// named after, or crew.
fn who_at(dir: &Path) -> String {
    dir.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| is_valid_id(n))
        .unwrap_or(CREW)
        .to_string()
}

/// `INFO` output as a `CommitInfo` on `branch`.
fn commit_info(out: &str, branch: &str) -> Result<CommitInfo> {
    let mut lines = out.lines();
    let sha = lines.next().unwrap_or_default().trim().to_string();
    if sha.is_empty() {
        bail!("jj described no commit");
    }
    let change = lines.next().unwrap_or_default().trim().to_string();
    let summary = lines.next().unwrap_or_default().to_string();
    let files_changed = lines
        .next()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    Ok(CommitInfo {
        short: short(&sha).to_string(),
        sha,
        branch: branch.to_string(),
        summary,
        files_changed,
        change_id: (!change.is_empty()).then_some(change),
    })
}

/// `<admin>` and `<git>` of a plain-git project (§layout): `<ws>/.git` is
/// the directory, or the `gitdir:` file of a linked git worktree, whose
/// common directory holds the objects and refs.
fn hidden_dirs(ws: &Path) -> Result<Dirs> {
    let dot_git = ws.join(".git");
    let not_root = || anyhow!("{} is not the root of a git repository", ws.display());
    let meta = std::fs::symlink_metadata(&dot_git).map_err(|_| not_root())?;
    if meta.is_dir() {
        let dir = dot_git
            .canonicalize()
            .with_context(|| format!("resolving {}", dot_git.display()))?;
        return Ok(Dirs {
            git: dir.clone(),
            admin: Some(dir),
        });
    }
    if !meta.is_file() {
        return Err(not_root());
    }
    let link = std::fs::read_to_string(&dot_git)
        .with_context(|| format!("reading {}", dot_git.display()))?;
    let gitdir = link
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .ok_or_else(not_root)?;
    let admin = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        ws.join(gitdir)
    };
    let admin = admin
        .canonicalize()
        .with_context(|| format!("resolving {}", admin.display()))?;
    Ok(Dirs {
        git: common_dir(&admin)?,
        admin: Some(admin),
    })
}

/// The common directory of a git directory: what its `commondir` file
/// names, or the directory itself.
fn common_dir(git_dir: &Path) -> Result<PathBuf> {
    let root =
        beneath::Root::open(git_dir).with_context(|| format!("opening {}", git_dir.display()))?;
    let Some(common) = root.read_text("commondir") else {
        return Ok(git_dir
            .canonicalize()
            .unwrap_or_else(|_| git_dir.to_path_buf()));
    };
    let common = common.trim();
    let path = if Path::new(common).is_absolute() {
        PathBuf::from(common)
    } else {
        git_dir.join(common)
    };
    path.canonicalize()
        .with_context(|| format!("resolving {}", path.display()))
}

impl Vcs for Jj {
    fn kind(&self) -> Kind {
        Kind::Jj
    }

    fn program(&self) -> &Path {
        &self.program
    }

    fn store(&self) -> Store {
        let git_dir = self
            .dirs()
            .map(|d| d.git.clone())
            .unwrap_or_else(|_| self.ws.join(".git"));
        Store::Jj {
            git_dir,
            repo_dir: self.repo_dir(),
        }
    }

    fn prepare(&self, ws: &Path) -> Result<bool> {
        if ws != self.ws {
            bail!("this backend belongs to {}", self.ws.display());
        }
        if !self.native {
            // Before anything: the root or a linked worktree's `.git`.
            hidden_dirs(ws)?;
        }
        // Keeps .crew out of git status and out of every jj snapshot,
        // the user's own included, without writing under .git.
        let state = self
            .handle
            .parent()
            .ok_or_else(|| anyhow!("{} has no parent", self.handle.display()))?;
        std::fs::create_dir_all(state)?;
        let ignore = state.join(".gitignore");
        if std::fs::read_to_string(&ignore).ok().as_deref() != Some("*\n") {
            std::fs::write(&ignore, "*\n")
                .with_context(|| format!("writing {}", ignore.display()))?;
        }
        if self.native {
            self.prepare_native()?;
        } else {
            self.prepare_hidden()?;
        }
        self.dirs()?;
        Ok(self.head()?.is_some())
    }

    fn branch_exists(&self, branch: &str) -> bool {
        is_plain_name(branch)
            && self
                .bookmark(&self.handle, CREW, branch)
                .is_ok_and(|found| found.is_some())
    }

    fn add_workspace(&self, dir: &Path, agent: &str, base: &str) -> Result<(Workspace, bool)> {
        let (branch, who) = Self::owner(agent)?;
        let path = dir.join(agent);
        let registered = self.workspaces()?.iter().any(|w| w == agent);
        let has_jj = path.join(".jj").is_dir();
        if registered && has_jj {
            let workspace = Workspace {
                path,
                branch,
                created: false,
            };
            return Ok((workspace, true));
        }
        if path.exists() {
            let empty = std::fs::read_dir(&path)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                bail!(
                    "{} already exists and is not a jj workspace of this repository; move it away \
(or, if it is a git worktree from an earlier session, see \"Moving a workspace to jj\" in the \
README)",
                    path.display()
                );
            }
        }
        if registered {
            // Its directory was deleted by hand.
            self.repo(&["workspace", "forget", agent])?;
        }
        if self.bookmark(&self.handle, CREW, &branch)?.is_none() {
            let start = self.base(base)?;
            self.repo(&[
                "bookmark",
                "create",
                &branch,
                "-r",
                &format!("commit_id({start})"),
            ])?;
        }
        std::fs::create_dir_all(dir)?;
        let target = path.to_string_lossy().into_owned();
        // Not with --ignore-working-copy: it would fail and still register
        // the workspace.
        self.out(
            &self.handle,
            who,
            &[
                "workspace",
                "add",
                "--name",
                agent,
                "--sparse-patterns",
                "empty",
                "-r",
                &format!("\"{branch}\""),
                &target,
            ],
        )?;
        beneath::Root::open(&path)
            .and_then(|root| root.write_new(".git", GUARD))
            .with_context(|| format!("writing the git guard in {}", path.display()))?;
        self.repo(&["git", "export"])?;
        let workspace = Workspace {
            path,
            branch,
            created: true,
        };
        Ok((workspace, true))
    }

    fn populate(&self, workspace: &Workspace) -> Result<()> {
        let who = Self::identity_of(workspace);
        self.out(&workspace.path, &who, &["sparse", "reset"])
            .map(drop)
    }

    fn commit_all(&self, dir: &Path, agent: &str, message: &str) -> Result<CommitInfo> {
        let message = message.trim();
        if message.is_empty() {
            bail!("commit message is empty");
        }
        let bookmark = git::branch_for(agent);
        let (clean, parents) = self.pre(dir, agent).map_err(|e| {
            if timed_out(&e) {
                anyhow!(
                    "jj commit timed out ({e:#}); nothing was committed, the changes are still in \
the working copy"
                )
            } else {
                e
            }
        })?;
        if clean {
            bail!("nothing to commit: the working tree has no changes");
        }
        let parent = parents.first().cloned().unwrap_or_default();
        self.guarded(dir, agent, &bookmark, Operation::Commit, &parent, || {
            self.out(dir, agent, &["commit", &format!("--message={message}")])?;
            if let Err(e) = self.out(dir, agent, &["bookmark", "set", &bookmark, "-r", "@-"]) {
                if timed_out(&e) {
                    return Err(e);
                }
                let made = self.info(dir, agent, &bookmark)?;
                bail!(
                    "committed {}, but {bookmark} could not be moved to it ({e:#}); the history \
was rearranged by hand, check jj log",
                    made.short
                );
            }
            self.export(dir, agent)?;
            let template = format!(
                "{INFO} ++ if(self.contained_in(\"remote_bookmarks(exact:\\\"{bookmark}\\\", \
exact:\\\"git\\\")\"), \"exported\", \"pending\") ++ \"\\n\""
            );
            let out = self.out(
                dir,
                agent,
                &[
                    "--ignore-working-copy",
                    "log",
                    "--no-graph",
                    "-r",
                    "@-",
                    "-T",
                    &template,
                ],
            )?;
            if out.lines().nth(4).map(str::trim) == Some("pending") {
                super::warn(format!(
                    "{bookmark} is not exported to git yet; the next export retries it"
                ));
            }
            commit_info(&out, &bookmark)
        })
    }

    fn diff(&self, dir: &Path, against: Option<&str>) -> Result<String> {
        match against {
            Some(rev) => {
                let who = who_at(dir);
                let commit = self.resolve(dir, &who, rev, "unknown revision")?;
                let from = format!("commit_id({commit})");
                let stat =
                    self.out(dir, &who, &["diff", "--stat", "--from", &from, "--to", "@"])?;
                let patch =
                    self.out(dir, &who, &["diff", "--git", "--from", &from, "--to", "@"])?;
                Ok(Self::truncate_diff(format!(
                    "{}\n\n{}",
                    stat.trim_end(),
                    patch.trim_end()
                )))
            }
            None => {
                let who = who_at(dir);
                let summary = self.out(dir, &who, &["diff", "--summary"])?;
                let patch = self.out(dir, &who, &["diff", "--git"])?;
                Ok(Self::truncate_diff(format!(
                    "{}\n\n{}",
                    summary.trim_end(),
                    patch.trim_end()
                )))
            }
        }
    }

    fn status(&self, dir: &Path) -> Result<String> {
        let who = who_at(dir);
        let changes = self.out(dir, &who, &["diff", "--summary"])?;
        let head = self.out(
            dir,
            &who,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                "@-",
                "-T",
                "bookmarks ++ \"\\n\" ++ commit_id.short(8) ++ \" \" ++ description.first_line() ++ \"\\n\"",
            ],
        )?;
        let mut lines = head.lines();
        let bookmarks = lines.next().unwrap_or_default().trim();
        let head = lines.next().unwrap_or_default().trim_end();
        let changes = changes.trim_end();
        Ok(format!(
            "branch {}\nHEAD {head}\n{}",
            if bookmarks.is_empty() {
                "(no bookmark)"
            } else {
                bookmarks
            },
            if changes.is_empty() {
                "working tree clean"
            } else {
                changes
            }
        ))
    }

    fn cherry_pick(&self, dir: &Path, agent: &str, rev: &str) -> Result<Picked> {
        let revset = revset(rev)?;
        let bookmark = git::branch_for(agent);
        let parent = self.clean_parent(dir, agent, Operation::CherryPick)?;
        let found = self.out(
            dir,
            agent,
            &[
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "-r",
                &revset,
                "-T",
                "commit_id ++ \" \" ++ author.email() ++ \"\\n\"",
            ],
        );
        let found = match found {
            Ok(found) => found,
            Err(e) if timed_out(&e) => return Err(e),
            Err(_) => bail!("unknown commit '{rev}'"),
        };
        let lines: Vec<&str> = found.lines().filter(|l| !l.trim().is_empty()).collect();
        let [line] = lines.as_slice() else {
            bail!("unknown commit '{rev}'");
        };
        let (source, author_email) = line.split_once(' ').unwrap_or((line, ""));
        let (source, author_email) = (source.trim().to_string(), author_email.trim().to_string());
        let outcome = self.guarded(
            dir,
            agent,
            &bookmark,
            Operation::CherryPick,
            &parent,
            || {
                let copy = format!("commit_id({source})");
                self.out(
                    dir,
                    agent,
                    &["--config", DUP, "duplicate", &copy, "--insert-before", "@"],
                )?;
                let made = self.state(dir, agent)?;
                if made.conflict {
                    return self.conflicted(
                        dir,
                        agent,
                        &bookmark,
                        Operation::CherryPick,
                        &made,
                        &parent,
                        short(&source),
                    );
                }
                if made.empty {
                    if made.bookmarks.is_empty() {
                        self.roll_back(dir, agent, Operation::CherryPick, &made, &parent)?;
                    }
                    bail!(
                        "cherry-pick of {} is empty: its changes are already in {bookmark}",
                        short(&source)
                    );
                }
                self.publish(dir, agent, &bookmark)?;
                let info = self.info(dir, agent, &bookmark)?;
                Ok(GitOutcome {
                    ok: true,
                    text: format!("applied {} as {} on {bookmark}", short(&source), info.short),
                    commit: Some(info),
                    conflict: None,
                })
            },
        )?;
        Ok(Picked {
            outcome,
            source,
            author_email,
        })
    }

    fn merge(&self, dir: &Path, agent: &str, branch: &str) -> Result<GitOutcome> {
        let bookmark = git::branch_for(agent);
        self.merge_into(dir, agent, &bookmark, branch, true)?
            .ok_or_else(|| anyhow!("unknown revision '{branch}'"))
    }

    fn merge_agents(
        &self,
        workspace: &Workspace,
        agents: &[String],
        on_step: &mut dyn FnMut(&IntegrationStep),
    ) -> Result<Vec<IntegrationStep>> {
        let dir = &workspace.path;
        let mut steps = Vec::new();
        for agent in agents {
            let branch = git::branch_for(agent);
            let Some(outcome) = self.merge_into(dir, CREW, INTEGRATION_BRANCH, &branch, false)?
            else {
                continue;
            };
            if outcome.ok && outcome.text == "Already up to date." {
                continue;
            }
            let step = IntegrationStep {
                agent: agent.clone(),
                outcome,
            };
            on_step(&step);
            steps.push(step);
        }
        self.export(dir, CREW)?;
        Ok(steps)
    }
}

#[cfg(test)]
mod tests;
