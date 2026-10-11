//! Git plumbing for workspace isolation.
//!
//! Each agent works in its own worktree on its own branch, so two agents can
//! never overwrite each other's files. Work moves between agents explicitly:
//! an agent commits, publishes the commit id, and a peer cherry-picks or
//! merges it. A conflicting cherry-pick/merge is aborted and reported; nothing
//! is ever force-resolved.
//!
//! Every git invocation uses an explicit argument array (no shell), and every
//! revision that originates from an agent is validated before use.

pub(crate) mod beneath;
pub mod launch;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::rpc::{CommitInfo, ConflictReport, GitOutcome};
use crew_core::util::is_valid_id;

/// Branch namespace for agent work.
pub const BRANCH_PREFIX: &str = "agents/";
/// Branch that collects everyone's work when the session is integrated.
pub const INTEGRATION_BRANCH: &str = "agents/integration";
/// The integration worktree's directory under the worktrees directory.
pub const INTEGRATION_DIR: &str = "_integration";
const MAX_DIFF_BYTES: usize = 48 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// This call made it (it was not there from an earlier run).
    pub created: bool,
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    // Commits made on behalf of agents carry the agent's identity, so signing
    // them with the user's key would be misleading (and pinentry would hang
    // the daemon).
    cmd.args(["-c", "commit.gpgsign=false"]);
    cmd.stdin(Stdio::null());
    // Never block on a credential or editor prompt.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_EDITOR", "true");
    cmd.env("GIT_MERGE_AUTOEDIT", "no");
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env_remove("GIT_INDEX_FILE");
    cmd
}

/// `git(dir)` for operations on the main repository, run by crew itself: no
/// hook, filesystem monitor or untracked cache the repository configures can
/// run, and the system configuration is ignored.
fn git_h(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ]);
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

/// How long one git command may run before it and its children are killed. A
/// hung hook or helper would otherwise hold the daemon's global git lock.
const GIT_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(5)
} else {
    Duration::from_secs(300)
};

/// `cmd.output()` with a deadline: the command leads its own process group,
/// and the group is killed when the deadline passes. Every git and jj command
/// crew runs goes through here, and through the installed [`launch::Launcher`].
pub(crate) fn output(cmd: &mut Command) -> std::io::Result<Output> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc;
    // Named before a launcher replaces the program with its sandbox runner.
    let program = Path::new(cmd.get_program())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "git".to_string());
    launch::apply(cmd)?;
    let spawned = cmd
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    launch::started(spawned.as_ref().ok().map(|c| c.id()));
    let mut child = spawned?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            let _ = tx.send(bytes);
        });
        rx
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + GIT_TIMEOUT;
    let timed_out = || {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "{program} did not finish within {}s and was killed",
                GIT_TIMEOUT.as_secs()
            ),
        )
    };
    let kill_group = |child: &mut std::process::Child| {
        // SAFETY: plain syscall; a negative pid addresses the command's group.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
    };
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            None => {
                kill_group(&mut child);
                return Err(timed_out());
            }
        }
    };
    let read = |rx: mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    };
    match (read(stdout), read(stderr)) {
        (Ok(stdout), Ok(stderr)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        _ => {
            kill_group(&mut child);
            Err(timed_out())
        }
    }
}

fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    output(git(dir).args(args))
        .with_context(|| format!("running git {}", args.first().copied().unwrap_or("")))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Run git and return stdout, turning a non-zero exit into an error.
fn out(dir: &Path, args: &[&str]) -> Result<String> {
    let output = run(dir, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            first_nonempty(&text(&output.stderr), &text(&output.stdout))
        );
    }
    Ok(text(&output.stdout))
}

/// `run` with the hardened invocation.
fn run_h(dir: &Path, args: &[&str]) -> Result<Output> {
    output(git_h(dir).args(args))
        .with_context(|| format!("running git {}", args.first().copied().unwrap_or("")))
}

/// `out` with the hardened invocation.
fn out_h(dir: &Path, args: &[&str]) -> Result<String> {
    let output = run_h(dir, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            first_nonempty(&text(&output.stderr), &text(&output.stdout))
        );
    }
    Ok(text(&output.stdout))
}

fn first_nonempty(a: &str, b: &str) -> String {
    if a.is_empty() { b } else { a }.to_string()
}

/// Identity used for commits made on behalf of an agent.
fn with_identity(cmd: &mut Command, agent: &str) {
    let email = format!("{agent}@crew.local");
    cmd.env("GIT_AUTHOR_NAME", agent)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", agent)
        .env("GIT_COMMITTER_EMAIL", &email);
}

pub fn is_repo(dir: &Path) -> bool {
    run_h(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success() && text(&o.stdout) == "true")
        .unwrap_or(false)
}

/// The top-level directory of the repository containing `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(out_h(
        dir,
        &["rev-parse", "--show-toplevel"],
    )?))
}

/// Whether `dir` is itself the root of a work tree (a repository's main
/// checkout or one of its linked worktrees).
///
/// Being merely *inside* a repository is not enough: crew stages and commits
/// whole work trees, so operating from a subdirectory of someone else's
/// repository would sweep that repository's files into an agent's commit.
pub fn is_root(dir: &Path) -> bool {
    let (Ok(top), Ok(dir)) = (toplevel(dir), dir.canonicalize()) else {
        return false;
    };
    top.canonicalize().is_ok_and(|top| top == dir)
}

fn require_root(dir: &Path) -> Result<()> {
    if is_root(dir) {
        Ok(())
    } else {
        bail!(
            "{} is not the root of a git work tree; refusing to run git there",
            dir.display()
        )
    }
}

/// Create the directory, initialise a repository rooted at it and make an
/// initial commit so worktrees have something to branch from. A directory
/// that only sits inside some other repository gets a repository of its own.
pub fn init_repo(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if !is_root(dir) {
        out_h(dir, &["init", "-q", "-b", "main"])?;
    }
    require_root(dir)?;
    if run_h(dir, &["rev-parse", "--verify", "-q", "HEAD"])?
        .status
        .success()
    {
        return Ok(());
    }
    // A first commit made here would take the operator's staged work under
    // crew's identity, and leave untracked source out of every worktree.
    // crew's own state and the team file do not count.
    let pending = out_h(dir, &["status", "--porcelain"])?;
    // Staged work is never ours: only an untracked team file is let through.
    let ours = |line: &str| {
        let path = line.get(3..).unwrap_or_default();
        path.starts_with(".crew/")
            || (line.starts_with("??") && ["crew.yaml", "crew.yml", "crew.toml"].contains(&path))
    };
    if !pending.lines().all(ours) {
        bail!(
            "the repository at {} has files but no commit yet; make the first commit yourself \
(git add -A && git commit) and run crew again",
            dir.display()
        );
    }
    // The main repository: the hardened invocation, not an agent's.
    out_h(dir, &["reset", "-q", "--", ".crew"])?;
    let mut cmd = git_h(dir);
    with_identity(&mut cmd, "crew");
    let output = output(cmd.args(["commit", "-q", "--allow-empty", "-m", "Initial commit"]))?;
    if !output.status.success() {
        bail!(
            "creating the initial commit failed: {}",
            text(&output.stderr)
        );
    }
    Ok(())
}

pub fn has_commits(dir: &Path) -> bool {
    run_h(dir, &["rev-parse", "--verify", "-q", "HEAD"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Take anything under `.crew/` out of the index (what is already tracked
/// stays tracked, unchanged): ignore rules do not apply to paths that are
/// staged or tracked, and the state holds agent tokens.
fn unstage_state_dir(dir: &Path) -> Result<()> {
    out(dir, &["reset", "-q", "--", ".crew"]).map(drop)
}

/// Keep crew's state directory out of `git status` without touching the
/// project's own `.gitignore`.
pub fn exclude_state_dir(repo: &Path) -> Result<()> {
    let git_dir = PathBuf::from(out_h(repo, &["rev-parse", "--git-common-dir"])?);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        repo.join(git_dir)
    };
    let info = git_dir.join("info");
    std::fs::create_dir_all(&info)?;
    let exclude = info.join("exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if !current.lines().any(|l| l.trim() == ".crew/") {
        let mut next = current;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(".crew/\n");
        std::fs::write(&exclude, next)?;
    }
    Ok(())
}

pub fn branch_for(agent: &str) -> String {
    format!("{BRANCH_PREFIX}{agent}")
}

pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    run_h(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// The commit the branch points at, if it exists.
pub fn branch_commit(repo: &Path, branch: &str) -> Option<String> {
    let rev = format!("refs/heads/{branch}^{{commit}}");
    out_h(repo, &["rev-parse", "--verify", "-q", &rev])
        .ok()
        .filter(|sha| !sha.is_empty())
}

/// The common git directory of `repo` (the main checkout's `.git`).
pub(crate) fn common_git_dir(repo: &Path) -> Result<PathBuf> {
    let dir = PathBuf::from(out_h(repo, &["rev-parse", "--git-common-dir"])?);
    Ok(if dir.is_absolute() {
        dir
    } else {
        repo.join(dir)
    })
}

/// The administrative directory git keeps for the linked worktree at `path`
/// (`<common>/worktrees/<name>`), found by the `gitdir` file git wrote there
/// rather than by trusting anything inside the worktree. Everything is read
/// through the descriptor of the common directory, never by a path walk.
fn find_admin(common: &beneath::Root, path: &Path) -> Option<String> {
    let want = path.join(".git");
    common.list("worktrees").into_iter().find(|name| {
        common
            .read_text(&format!("worktrees/{name}/gitdir"))
            .is_some_and(|g| Path::new(g.trim()) == want)
    })
}

/// The path of the worktree's administrative directory (for building a
/// sandbox, which pins it again without following symlinks).
pub fn worktree_admin_dir(common: &Path, path: &Path) -> Option<PathBuf> {
    let root = beneath::Root::open(common).ok()?;
    find_admin(&root, path).map(|name| common.join("worktrees").join(name))
}

/// Create (or reuse) the worktree and branch for an agent, and fill it in.
pub fn ensure_worktree(
    repo: &Path,
    worktrees_dir: &Path,
    agent: &str,
    base: &str,
) -> Result<Worktree> {
    let (worktree, needs_population) = add_worktree(repo, worktrees_dir, agent, base)?;
    if needs_population {
        populate_worktree(&worktree)?;
    }
    Ok(worktree)
}

/// Create (or find) the worktree and branch for an agent, without checking
/// anything out: `worktree add --no-checkout` runs no `post-checkout` hook and
/// no smudge filter. Returns whether the files still have to be put in place
/// ([`populate_worktree`]). Runs on the main repository, hardened, and runs no
/// git inside the worktree itself, whose contents an agent can write.
pub fn add_worktree(
    repo: &Path,
    worktrees_dir: &Path,
    agent: &str,
    base: &str,
) -> Result<(Worktree, bool)> {
    if !is_valid_id(agent) && agent != INTEGRATION_DIR {
        bail!("invalid agent id '{agent}'");
    }
    require_root(repo)?;
    let branch = if agent == INTEGRATION_DIR {
        INTEGRATION_BRANCH.to_string()
    } else {
        branch_for(agent)
    };
    let path = worktrees_dir.join(agent);
    if path.join(".git").exists() {
        // Left over from an earlier run: reuse it if it is on the right
        // branch, read from the repository's own record of the worktree.
        let common = beneath::Root::open(&common_git_dir(repo)?)?;
        let name = find_admin(&common, &path)
            .ok_or_else(|| anyhow!("{} is not a worktree of this repository", path.display()))?;
        let admin = format!("worktrees/{name}");
        let head = common
            .read_text(&format!("{admin}/HEAD"))
            .unwrap_or_default();
        let current = head
            .trim()
            .strip_prefix("ref: refs/heads/")
            .unwrap_or("(detached)");
        if current == branch {
            common.create_empty(&format!("{admin}/config.worktree"))?;
            // Without an index nothing was checked out yet.
            let needs_population = !common.exists(&format!("{admin}/index"));
            let worktree = Worktree {
                path,
                branch,
                created: false,
            };
            return Ok((worktree, needs_population));
        }
        bail!(
            "{} already exists but is on branch '{current}', expected '{branch}'",
            path.display()
        );
    }
    std::fs::create_dir_all(worktrees_dir)?;
    // Forget worktrees whose directories were deleted by hand.
    let _ = run_h(repo, &["worktree", "prune"]);
    let path_str = path.to_string_lossy().into_owned();
    if branch_exists(repo, &branch) {
        out_h(
            repo,
            &["worktree", "add", "-q", "--no-checkout", &path_str, &branch],
        )?;
    } else {
        validate_rev(base)?;
        out_h(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "--no-checkout",
                "-b",
                &branch,
                &path_str,
                base,
            ],
        )?;
    }
    let common = beneath::Root::open(&common_git_dir(repo)?)?;
    let name = find_admin(&common, &path)
        .ok_or_else(|| anyhow!("git did not record the worktree {}", path.display()))?;
    // An empty config.worktree: the sandbox binds it read-only over the real one,
    // so an agent cannot set per-worktree configuration (core.fsmonitor, hooks)
    // that crew's own git would then run.
    common.create_empty(&format!("worktrees/{name}/config.worktree"))?;
    let worktree = Worktree {
        path,
        branch,
        created: true,
    };
    Ok((worktree, true))
}

/// Put the branch's files in a worktree made by [`add_worktree`]. This is
/// where smudge filters and the branch's own configuration can run, so a
/// caller that has a sandbox runs it inside one (see [`launch`]).
pub fn populate_worktree(worktree: &Worktree) -> Result<()> {
    out_h(&worktree.path, &["reset", "-q", "--hard", "HEAD"]).map(drop)
}

/// An operation that timed out after its result had landed: the result was
/// kept (and, under jj, published), and this says so.
#[derive(Debug)]
pub struct Kept {
    /// The full id of the commit that was kept.
    pub commit: String,
    pub message: String,
}

impl std::fmt::Display for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Kept {}

/// Revisions supplied by agents: a hex commit id, `HEAD`, or an agent branch.
/// Anything else (in particular anything starting with `-`) is rejected so a
/// revision can never be parsed as a git option.
pub fn validate_rev(rev: &str) -> Result<()> {
    let is_sha = (7..=40).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit());
    let is_agent_branch = rev.strip_prefix(BRANCH_PREFIX).is_some_and(is_valid_id);
    let is_plain_ref = !rev.is_empty()
        && rev.len() <= 100
        && !rev.starts_with('-')
        && !rev.contains("..")
        && rev.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/' | b'.' | b'~' | b'^')
        });
    if is_sha || is_agent_branch || is_plain_ref {
        Ok(())
    } else {
        Err(anyhow!("invalid git revision '{rev}'"))
    }
}

fn commit_info(dir: &Path, rev: &str) -> Result<CommitInfo> {
    let sha = out(
        dir,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )?;
    let summary = out(dir, &["log", "-1", "--format=%s", &sha])?;
    let branch = out(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let files = out(dir, &["show", "--format=", "--name-only", &sha])?;
    Ok(CommitInfo {
        short: sha.chars().take(8).collect(),
        sha,
        branch,
        summary,
        files_changed: files.lines().filter(|l| !l.trim().is_empty()).count(),
        change_id: None,
    })
}

/// The paths a commit changed; none for a merge commit.
pub fn files_of(dir: &Path, rev: &str) -> Result<Vec<String>> {
    validate_rev(rev)?;
    require_root(dir)?;
    let files = out(dir, &["show", "--format=", "--name-only", rev])
        .map_err(|_| anyhow!("unknown revision '{rev}'"))?;
    Ok(files
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Stage everything in the worktree and commit it as `agent`.
pub fn commit_all(dir: &Path, agent: &str, message: &str) -> Result<CommitInfo> {
    let message = message.trim();
    if message.is_empty() {
        bail!("commit message is empty");
    }
    require_root(dir)?;
    // `.crew` is left out by pathspec, not by writing an exclude file: the path
    // of the git directory is whatever a (possibly sandboxed) git prints, and
    // the daemon must not write where that says.
    out(dir, &["add", "-A", "--", ".", ":(exclude).crew"])?;
    unstage_state_dir(dir)?;
    let staged = run(dir, &["diff", "--cached", "--quiet"])?;
    if staged.status.success() {
        bail!("nothing to commit: the working tree has no changes");
    }
    let mut cmd = git(dir);
    with_identity(&mut cmd, agent);
    // `-m <message>` passes the message as a single argument. The project's
    // own commit hooks still run; a failing hook is reported back to the agent.
    let before = out(dir, &["rev-parse", "HEAD"]).ok();
    let output = match output(cmd.args(["commit", "-q", "-m", message])) {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            clear_stale_index_lock(dir);
            let after = out(dir, &["rev-parse", "HEAD"]).ok();
            if after != before {
                return Err(Kept {
                    commit: after.unwrap_or_default(),
                    message: format!(
                        "git commit timed out ({e}) after the commit was made; check the history"
                    ),
                }
                .into());
            }
            bail!(
                "git commit timed out ({e}); nothing was committed, the changes are still staged"
            );
        }
        Err(e) => return Err(e.into()),
    };
    if !output.status.success() {
        bail!(
            "git commit failed: {}",
            first_nonempty(&text(&output.stderr), &text(&output.stdout))
        );
    }
    commit_info(dir, "HEAD")
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

/// Uncommitted changes, or the changes relative to `against`.
pub fn diff(dir: &Path, against: Option<&str>) -> Result<String> {
    require_root(dir)?;
    match against {
        Some(rev) => {
            validate_rev(rev)?;
            let stat = out(dir, &["diff", "--stat", rev, "--"])?;
            let patch = out(dir, &["diff", rev, "--"])?;
            Ok(truncate_diff(format!("{stat}\n\n{patch}")))
        }
        None => {
            let status = out(dir, &["status", "--short"])?;
            let patch = out(dir, &["diff", "HEAD", "--"]).unwrap_or_default();
            Ok(truncate_diff(format!("{status}\n\n{patch}")))
        }
    }
}

/// The text of `path` as commit `rev` holds it; `None` when the commit has
/// no such file. The path is a literal, relative to the tree.
pub fn file_at(dir: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    require_root(dir)?;
    validate_rev(rev)?;
    let path = path.trim_start_matches("./");
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p == "..") {
        bail!("'{path}' is not a path inside the tree");
    }
    let output = run(dir, &["show", &format!("{rev}:{path}")])?;
    if output.status.success() {
        return Ok(Some(text(&output.stdout)));
    }
    let why = text(&output.stderr);
    if why.contains("does not exist in") || why.contains("exists on disk, but not in") {
        return Ok(None);
    }
    bail!("git show {rev}:{path} failed: {}", why.trim());
}

pub fn status(dir: &Path) -> Result<String> {
    require_root(dir)?;
    let branch = out(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = out(dir, &["log", "-1", "--format=%h %s"]).unwrap_or_default();
    let short = out(dir, &["status", "--short"])?;
    let changes = if short.is_empty() {
        "working tree clean".to_string()
    } else {
        short
    };
    Ok(format!("branch {branch}\nHEAD {head}\n{changes}"))
}

fn conflicted_files(dir: &Path) -> Vec<String> {
    out(dir, &["diff", "--name-only", "--diff-filter=U"])
        .map(|s| {
            s.lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn dirty(dir: &Path) -> Result<bool> {
    Ok(!out(dir, &["status", "--porcelain"])?.is_empty())
}

/// Everything git started is dead once it timed out, so an index lock left
/// behind is stale.
fn clear_stale_index_lock(dir: &Path) {
    if launch::active() {
        // On an agent's behalf the git directory is the one the daemon made
        // for it, opened by descriptor; a path git printed in a sandbox is the
        // agent's to forge and is never used.
        if let Some(admin) = launch::admin_dir() {
            if let Ok(root) = beneath::Root::open(&admin) {
                root.remove("index.lock");
            }
        }
        return;
    }
    if let Ok(lock) = out_h(dir, &["rev-parse", "--git-path", "index.lock"]) {
        let _ = std::fs::remove_file(dir.join(lock));
    }
}

/// `output`, but a timeout is recovered from: the killed operation may have
/// left a stale index lock, a half-applied merge or staged files. The worktree
/// was clean before (callers checked), so restoring HEAD's state loses nothing.
/// If HEAD moved before the kill the operation did complete; that is reported,
/// not undone.
fn output_recovering(
    dir: &Path,
    cmd: &mut Command,
    operation: &str,
    abort: &[&str],
) -> Result<Output> {
    let before = out(dir, &["rev-parse", "HEAD"])?;
    let err = match output(cmd) {
        Ok(output) => return Ok(output),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => e,
        Err(e) => return Err(e.into()),
    };
    clear_stale_index_lock(dir);
    let _ = run(dir, abort);
    let after = out(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    if after != before {
        let message = format!(
            "git {operation} timed out ({err}) after HEAD moved from {before} to {after}; the \
result was kept, check the history before retrying"
        );
        return Err(Kept {
            commit: after,
            message,
        }
        .into());
    }
    let _ = run(dir, &["reset", "-q", "--hard", "HEAD"]);
    if dirty(dir).unwrap_or(true) {
        bail!("git {operation} timed out ({err}) and the worktree could not be restored; check git status");
    }
    bail!("git {operation} timed out ({err}); the worktree was restored, retry later")
}

/// Shared tail of cherry-pick and merge: on failure collect the conflicted
/// files, abort the operation so the worktree is left exactly as it was, and
/// report.
fn finish(dir: &Path, operation: &str, abort: &[&str], output: Output) -> Result<GitOutcome> {
    if output.status.success() {
        return Ok(GitOutcome {
            ok: true,
            commit: Some(commit_info(dir, "HEAD")?),
            conflict: None,
            text: text(&output.stdout),
        });
    }
    let files = conflicted_files(dir);
    let detail = first_nonempty(&text(&output.stderr), &text(&output.stdout));
    let _ = run(dir, abort);
    if files.is_empty() {
        bail!("git {operation} failed: {detail}");
    }
    Ok(GitOutcome {
        ok: false,
        commit: None,
        conflict: Some(ConflictReport {
            operation: operation.to_string(),
            files,
            detail,
        }),
        text: format!("{operation} aborted because of conflicts; the worktree is unchanged"),
    })
}

/// What a cherry-pick did, and whose commit it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub outcome: GitOutcome,
    /// The full id of the commit that was picked.
    pub source: String,
    /// Its author's address (`<agent>@crew.local` for an agent's commit).
    pub author_email: String,
}

/// Apply another agent's commit onto this worktree's branch.
pub fn cherry_pick(dir: &Path, agent: &str, commit: &str) -> Result<Picked> {
    validate_rev(commit)?;
    require_root(dir)?;
    if dirty(dir)? {
        bail!("the worktree has uncommitted changes; commit them before cherry-picking");
    }
    let source = out(
        dir,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )
    .map_err(|_| anyhow!("unknown commit '{commit}'"))?;
    let author_email = out(dir, &["log", "-1", "--format=%ae", &source]).unwrap_or_default();
    let mut cmd = git(dir);
    with_identity(&mut cmd, agent);
    let output = output_recovering(
        dir,
        cmd.args(["cherry-pick", "-x", &source]),
        "cherry-pick",
        &["cherry-pick", "--abort"],
    )?;
    let outcome = finish(dir, "cherry-pick", &["cherry-pick", "--abort"], output)?;
    Ok(Picked {
        outcome,
        source,
        author_email,
    })
}

/// Merge a branch (or commit) into this worktree's branch, as a merge
/// commit always: the record of taking a teammate's work.
pub fn merge(dir: &Path, agent: &str, rev: &str) -> Result<GitOutcome> {
    merge_with(dir, agent, rev, false)
}

/// Merge a branch into this worktree's branch, fast-forwarding when the
/// branch is simply behind: how a worktree takes the integration branch
/// before a card, so that a member's branch is not ahead of it by a merge
/// commit that says nothing.
pub fn merge_ff(dir: &Path, agent: &str, rev: &str) -> Result<GitOutcome> {
    merge_with(dir, agent, rev, true)
}

fn merge_with(dir: &Path, agent: &str, rev: &str, fast_forward: bool) -> Result<GitOutcome> {
    validate_rev(rev)?;
    require_root(dir)?;
    if dirty(dir)? {
        bail!("the worktree has uncommitted changes; commit them before merging");
    }
    out(
        dir,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .map_err(|_| anyhow!("unknown revision '{rev}'"))?;
    let mut cmd = git(dir);
    with_identity(&mut cmd, agent);
    let message = format!("Merge {rev}");
    let ff = if fast_forward { "--ff" } else { "--no-ff" };
    let output = output_recovering(
        dir,
        cmd.args(["merge", ff, "--no-edit", "-m", &message, rev]),
        "merge",
        &["merge", "--abort"],
    )?;
    finish(dir, "merge", &["merge", "--abort"], output)
}

/// Result of merging one agent branch during integration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationStep {
    pub agent: String,
    pub outcome: GitOutcome,
}

/// Merge every agent branch into the integration branch, one at a time.
/// Conflicting branches are skipped (and reported), never forced.
pub fn integrate(
    repo: &Path,
    worktrees_dir: &Path,
    base: &str,
    agents: &[String],
    on_step: &mut dyn FnMut(&IntegrationStep),
) -> Result<(Worktree, Vec<IntegrationStep>)> {
    let worktree = ensure_worktree(repo, worktrees_dir, INTEGRATION_DIR, base)?;
    let steps = merge_agents(&worktree, agents, on_step)?;
    Ok((worktree, steps))
}

/// Merge every agent branch that is ahead into the integration worktree.
/// Every git command runs in that worktree (the branches are found through
/// its shared refs), so a helper sandbox that mounts only it can run them
/// all. `on_step` is told each step as soon as it is done.
pub fn merge_agents(
    worktree: &Worktree,
    agents: &[String],
    on_step: &mut dyn FnMut(&IntegrationStep),
) -> Result<Vec<IntegrationStep>> {
    let mut steps = Vec::new();
    for agent in agents {
        let branch = branch_for(agent);
        let found = run(
            &worktree.path,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        // Exit 1 is "no such branch". Anything else that is not success
        // (a sandbox that could not start exits 125) fails the integration
        // rather than leaving the branch out.
        match found.status.code() {
            Some(0) => {}
            Some(1) => continue,
            _ => bail!(
                "looking for {branch} failed ({}): {}",
                found.status,
                first_nonempty(&text(&found.stderr), &text(&found.stdout))
            ),
        }
        let ahead = out(
            &worktree.path,
            &["rev-list", "--count", &format!("HEAD..{branch}")],
        )?;
        if ahead == "0" {
            continue;
        }
        let outcome = merge(&worktree.path, "crew", &branch)?;
        let step = IntegrationStep {
            agent: agent.clone(),
            outcome,
        };
        on_step(&step);
        steps.push(step);
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Repo {
        _dir: tempfile::TempDir,
        root: PathBuf,
        worktrees: PathBuf,
    }

    fn repo() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        init_repo(&root).unwrap();
        let worktrees = root.join(".crew").join("worktrees");
        Repo {
            _dir: dir,
            root,
            worktrees,
        }
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn init_creates_a_repository_with_a_commit() {
        let r = repo();
        assert!(is_repo(&r.root));
        assert!(has_commits(&r.root));
        // Idempotent.
        init_repo(&r.root).unwrap();
        assert!(!is_repo(Path::new("/")));
    }

    #[test]
    fn a_workspace_nested_in_another_repository_gets_its_own_repository() {
        let outer = repo();
        let nested = outer.root.join("demo-workspace");
        std::fs::create_dir_all(&nested).unwrap();
        write(
            &outer.root,
            "outer.txt",
            "belongs to the outer repository\n",
        );

        // Inside a repository, but not the root of one.
        assert!(is_repo(&nested));
        assert!(!is_root(&nested));
        // Nothing may be staged or committed from there: it would sweep the
        // outer repository's files into an agent's commit.
        write(&nested, "agent.txt", "x\n");
        let err = commit_all(&nested, "claude-1", "oops").unwrap_err();
        assert!(
            err.to_string().contains("not the root of a git work tree"),
            "{err}"
        );
        assert!(ensure_worktree(&nested, &nested.join(".crew/worktrees"), "a", "HEAD").is_err());
        assert!(cherry_pick(&nested, "a", "abcdef1").is_err());
        assert!(status(&nested).is_err());

        let outer_head = out(&outer.root, &["rev-parse", "HEAD"]).unwrap();
        // An existing project with files must make its own first commit; the
        // agent's file is written once the repository is initialised.
        assert!(init_repo(&nested).is_err());
        std::fs::remove_file(nested.join("agent.txt")).unwrap();
        std::fs::remove_dir_all(nested.join(".git")).unwrap();
        init_repo(&nested).unwrap();
        assert!(is_root(&nested));
        assert!(has_commits(&nested));
        write(&nested, "agent.txt", "x\n");
        let commit = commit_all(&nested, "claude-1", "agent work").unwrap();
        assert_eq!(commit.files_changed, 1);
        // The outer repository did not gain a commit and still has its
        // untracked file.
        assert_eq!(
            out(&outer.root, &["rev-parse", "HEAD"]).unwrap(),
            outer_head
        );
        assert!(out(&outer.root, &["status", "--porcelain"])
            .unwrap()
            .contains("outer.txt"));
        // A linked worktree is a root too.
        let wt = ensure_worktree(&nested, &nested.join(".crew/worktrees"), "a", "HEAD").unwrap();
        assert!(is_root(&wt.path));
    }

    #[test]
    fn init_refuses_a_repository_with_work_but_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        out(&root, &["init", "-q", "-b", "main"]).unwrap();
        write(&root, "staged.rs", "fn main() {}\n");
        out(&root, &["add", "staged.rs"]).unwrap();
        write(&root, "untracked.rs", "x\n");
        let err = init_repo(&root).unwrap_err();
        assert!(err.to_string().contains("no commit yet"), "{err}");
        assert!(!has_commits(&root), "an initial commit was made anyway");
        // Only crew's own state directory in the way is fine.
        let clean = dir.path().join("clean");
        std::fs::create_dir_all(clean.join(".crew")).unwrap();
        write(&clean.join(".crew"), "crew.db", "x");
        init_repo(&clean).unwrap();
        assert!(has_commits(&clean));
    }

    fn has_commit_with_subject(dir: &Path, subject: &str) -> bool {
        out(dir, &["log", "--format=%s"])
            .unwrap()
            .lines()
            .any(|l| l == subject)
    }

    #[test]
    fn a_hung_git_hook_times_out_and_is_killed() {
        use std::os::unix::fs::PermissionsExt;
        let r = repo();
        let hook = r.root.join(".git/hooks/pre-commit");
        let marker = r.root.join("hook-child-alive");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\n(sleep 7; touch {}) &\nsleep 60\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&r.root, "work.txt", "x\n");
        let started = std::time::Instant::now();
        let err = commit_all(&r.root, "a", "work").unwrap_err();
        assert!(err.to_string().contains("did not finish"), "{err:#}");
        assert!(err.to_string().contains("nothing was committed"), "{err:#}");
        assert!(!has_commit_with_subject(&r.root, "work"));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        // The hook's children died with it.
        std::thread::sleep(std::time::Duration::from_secs(4));
        assert!(!marker.exists(), "the hook's child outlived the timeout");
    }

    /// `main` clean, `topic` one commit ahead; `hook` hangs for 60s.
    fn repo_with_topic_and_hanging_hook(hook: &str) -> Repo {
        use std::os::unix::fs::PermissionsExt;
        let r = repo();
        out(&r.root, &["checkout", "-q", "-b", "topic"]).unwrap();
        write(&r.root, "topic.txt", "t\n");
        commit_all(&r.root, "t", "topic work").unwrap();
        out(&r.root, &["checkout", "-q", "main"]).unwrap();
        let path = r.root.join(".git/hooks").join(hook);
        std::fs::write(&path, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        r
    }

    fn assert_restored_and_retryable(r: &Repo, head: &str, hook: &str) {
        assert_eq!(out(&r.root, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(out(&r.root, &["status", "--porcelain"]).unwrap(), "");
        assert!(!r.root.join("topic.txt").exists());
        // With the hook gone the same operation goes through.
        std::fs::remove_file(r.root.join(".git/hooks").join(hook)).unwrap();
    }

    #[test]
    fn a_merge_that_times_out_leaves_the_worktree_as_it_was() {
        let r = repo_with_topic_and_hanging_hook("pre-merge-commit");
        let head = out(&r.root, &["rev-parse", "HEAD"]).unwrap();
        let err = merge(&r.root, "a", "topic").unwrap_err();
        assert!(err.to_string().contains("worktree was restored"), "{err:#}");
        assert_restored_and_retryable(&r, &head, "pre-merge-commit");
        assert!(merge(&r.root, "a", "topic").unwrap().ok);
        assert!(r.root.join("topic.txt").exists());
    }

    #[test]
    fn a_cherry_pick_that_times_out_leaves_the_worktree_as_it_was() {
        let r = repo_with_topic_and_hanging_hook("prepare-commit-msg");
        let head = out(&r.root, &["rev-parse", "HEAD"]).unwrap();
        let err = cherry_pick(&r.root, "a", "topic").unwrap_err();
        assert!(err.to_string().contains("worktree was restored"), "{err:#}");
        assert_restored_and_retryable(&r, &head, "prepare-commit-msg");
        assert!(cherry_pick(&r.root, "a", "topic").unwrap().outcome.ok);
    }

    #[test]
    fn a_timeout_after_head_moved_is_reported_not_undone() {
        let r = repo_with_topic_and_hanging_hook("post-merge");
        let before = out(&r.root, &["rev-parse", "HEAD"]).unwrap();
        let err = merge(&r.root, "a", "topic").unwrap_err();
        let after = out(&r.root, &["rev-parse", "HEAD"]).unwrap();
        assert_ne!(before, after, "the merge itself had completed");
        assert!(err.to_string().contains("HEAD moved"), "{err:#}");
        assert!(r.root.join("topic.txt").exists(), "the commit was kept");
    }

    #[test]
    fn main_repository_operations_run_no_hooks_and_no_configured_commands() {
        use std::os::unix::fs::PermissionsExt;
        let r = repo();
        let marker = r.root.join("hook-ran");
        let script = |name: &str| {
            let path = r.root.join(".git/hooks").join(name);
            std::fs::write(&path, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script("post-checkout");
        script("reference-transaction");
        // A configured command git would run on a status refresh.
        let monitor = r.root.join("monitor.sh");
        std::fs::write(&monitor, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&monitor, std::fs::Permissions::from_mode(0o755)).unwrap();
        out(
            &r.root,
            &["config", "core.fsmonitor", &monitor.to_string_lossy()],
        )
        .unwrap();
        assert!(is_repo(&r.root) && is_root(&r.root) && has_commits(&r.root));
        ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        assert!(!marker.exists(), "a hook or configured command ran");
        // The overrides are on the command line.
        let cmd = git_h(&r.root);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for want in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "core.untrackedCache=false",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} in {args:?}");
        }
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "GIT_CONFIG_NOSYSTEM" && v.is_some_and(|v| v == "1")));
    }

    /// Records every command it is asked to launch and runs it unchanged.
    struct Recorder(std::sync::Mutex<Vec<Vec<String>>>, Option<PathBuf>);

    impl Recorder {
        fn with_admin(mut self, admin: PathBuf) -> Self {
            self.1 = Some(admin);
            self
        }
    }

    impl launch::Launcher for Recorder {
        fn admin_dir(&self) -> Option<PathBuf> {
            self.1.clone()
        }

        fn launch(&self, cmd: &Command) -> std::io::Result<Command> {
            let argv = launch::argv(cmd)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            self.0.lock().unwrap().push(argv);
            let mut same = Command::new(cmd.get_program());
            same.args(cmd.get_args());
            for (k, v) in cmd.get_envs() {
                match v {
                    Some(v) => same.env(k, v),
                    None => same.env_remove(k),
                };
            }
            Ok(same)
        }
    }

    #[test]
    fn a_launcher_is_told_the_pid_of_each_command_it_launched_or_that_it_did_not_start() {
        /// Launches each command unchanged, or (`broken`) as a program that
        /// does not exist, and keeps what it is told.
        struct Watcher {
            broken: bool,
            launched: std::sync::Mutex<usize>,
            told: std::sync::Mutex<Vec<Option<u32>>>,
        }
        impl launch::Launcher for Watcher {
            fn launch(&self, cmd: &Command) -> std::io::Result<Command> {
                *self.launched.lock().unwrap() += 1;
                let mut same = Command::new(if self.broken {
                    std::ffi::OsStr::new("/nonexistent/git")
                } else {
                    cmd.get_program()
                });
                same.args(cmd.get_args());
                for (k, v) in cmd.get_envs() {
                    match v {
                        Some(v) => same.env(k, v),
                        None => same.env_remove(k),
                    };
                }
                Ok(same)
            }

            fn started(&self, pid: Option<u32>) {
                self.told.lock().unwrap().push(pid);
            }
        }
        let watcher = |broken| {
            Arc::new(Watcher {
                broken,
                launched: Default::default(),
                told: Default::default(),
            })
        };
        let r = repo();
        write(&r.root, "work.txt", "x\n");
        let working = watcher(false);
        launch::with_launcher(working.clone(), || {
            commit_all(&r.root, "a", "work").unwrap()
        });
        let told = working.told.lock().unwrap().clone();
        assert_eq!(told.len(), *working.launched.lock().unwrap(), "{told:?}");
        assert!(
            told.len() >= 4 && told.iter().all(Option::is_some),
            "{told:?}"
        );
        let mut pids: Vec<u32> = told.into_iter().flatten().collect();
        pids.dedup();
        assert!(pids.len() >= 4, "one process per command: {pids:?}");
        // A command that cannot be started is reported as such, and fails.
        let broken = watcher(true);
        assert!(launch::with_launcher(broken.clone(), || status(&r.root)).is_err());
        assert_eq!(*broken.told.lock().unwrap(), vec![None]);
    }

    #[test]
    fn every_git_command_goes_through_the_installed_launcher_and_only_inside_its_scope() {
        let r = repo();
        write(&r.root, "work.txt", "x\n");
        let recorder = Arc::new(Recorder(Default::default(), None));
        let commit = launch::with_launcher(recorder.clone(), || {
            commit_all(&r.root, "a", "work").unwrap()
        });
        assert_eq!(commit.summary, "work");
        let seen = recorder.0.lock().unwrap().clone();
        assert!(seen.len() >= 4, "{seen:?}");
        assert!(seen.iter().all(|argv| argv[0] == "git"), "{seen:?}");
        assert!(seen.iter().any(|argv| argv.contains(&"commit".to_string())));
        // Outside the scope nothing is recorded.
        let before = recorder.0.lock().unwrap().len();
        write(&r.root, "more.txt", "y\n");
        commit_all(&r.root, "a", "more").unwrap();
        assert_eq!(recorder.0.lock().unwrap().len(), before);
        // A launcher that refuses stops the operation: no fallback.
        struct Refuse;
        impl launch::Launcher for Refuse {
            fn launch(&self, _: &Command) -> std::io::Result<Command> {
                Err(std::io::Error::other("no sandbox"))
            }
        }
        write(&r.root, "third.txt", "z\n");
        assert!(
            launch::with_launcher(Arc::new(Refuse), || commit_all(&r.root, "a", "third")).is_err()
        );
        assert!(!has_commit_with_subject(&r.root, "third"));
    }

    #[test]
    fn the_sandbox_launcher_wraps_git_in_sandbox_run_with_a_clean_environment() {
        use crate::sandbox::spec::Spec;
        use launch::Launcher;
        let spec = Spec {
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: "/opt/crew/bin/crew".into(),
            mounts: vec![],
            env: vec![("HOME".into(), "/home/helper".into())],
            pass_env: vec![],
            cwd: "/work".into(),
            network: false,
            new_session: true,
            min_landlock_abi: crate::sandbox::landlock::MIN_ABI,
            allow_unix: vec![],
            done: None,
        };
        let mut git_cmd = git(Path::new("/work"));
        with_identity(&mut git_cmd, "agent-1");
        git_cmd.args(["status", "--porcelain"]);
        let launched = launch::SandboxLauncher::new(spec, Vec::new())
            .launch(&git_cmd)
            .unwrap();
        let argv: Vec<String> = launch::argv(&launched)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], "/opt/crew/bin/crew");
        assert_eq!(argv[1], "sandbox-run");
        assert_eq!(argv[3], "--");
        assert_eq!(&argv[4..7], ["git", "-C", "/work"]);
        assert!(argv.ends_with(&["status".into(), "--porcelain".into()]));
        // The spec carries git's non-secret variables, including the identity.
        let spec: serde_json::Value = serde_json::from_str(&argv[2]).unwrap();
        let env = spec["env"].to_string();
        assert!(
            env.contains("GIT_AUTHOR_NAME") && env.contains("agent-1"),
            "{env}"
        );
        assert!(env.contains("GIT_TERMINAL_PROMPT"), "{env}");
        // The process is started with the allow-list only: nothing of git's
        // own variables or the daemon's other variables.
        let started: Vec<String> = launched
            .get_envs()
            .filter(|(_, v)| v.is_some())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(
            !started
                .iter()
                .any(|k| k.starts_with("GIT_") || k == "SECRET_TOKEN"),
            "{started:?}"
        );
        assert!(!env.contains("sk-not-allowed") || env.contains("SECRET_TOKEN"));
    }

    #[test]
    fn reusing_a_worktree_runs_no_git_inside_it() {
        let r = repo();
        let wt = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        // Whatever the worktree holds is agent-writable: crew must read its
        // branch from the repository's own administrative files, never by
        // running git in it.
        let recorder = Arc::new(Recorder(Default::default(), None));
        let again = launch::with_launcher(recorder.clone(), || {
            ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap()
        });
        assert_eq!((&again.path, &again.branch), (&wt.path, &wt.branch));
        assert!(wt.created && !again.created);
        let seen = recorder.0.lock().unwrap().clone();
        let inside = wt.path.to_string_lossy().into_owned();
        assert!(
            !seen.iter().any(|argv| argv.contains(&inside)),
            "git ran in the worktree: {seen:?}"
        );
        // A worktree on another branch is still refused.
        out(&wt.path, &["checkout", "-q", "-b", "other"]).unwrap();
        let err = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap_err();
        assert!(err.to_string().contains("expected 'agents/a'"), "{err}");
    }

    #[test]
    fn a_new_worktree_is_added_without_a_checkout_and_populated_separately() {
        let r = repo();
        write(&r.root, "tracked.txt", "content\n");
        out(&r.root, &["add", "tracked.txt"]).unwrap();
        out(
            &r.root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "tracked",
            ],
        )
        .unwrap();
        let (wt, needs_population) = add_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        assert!(needs_population);
        assert!(
            !wt.path.join("tracked.txt").exists(),
            "added with a checkout"
        );
        // Not populated yet, so a second look still says so.
        assert!(add_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap().1);
        populate_worktree(&wt).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.path.join("tracked.txt")).unwrap(),
            "content\n"
        );
        assert!(!add_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap().1);
        assert!(status(&wt.path).unwrap().contains("working tree clean"));
    }

    #[test]
    fn the_helper_launcher_has_the_worktree_and_nothing_of_the_daemon() {
        use crate::sandbox::profile::{Extra, Host, Session};
        use crate::sandbox::spec::Mount;
        use std::os::unix::net::UnixListener;
        let r = repo();
        let (wt, _) = add_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        let root = r.root.canonicalize().unwrap();
        let run = r._dir.path().canonicalize().unwrap().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let socket = run.join("daemon.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let cargo = r._dir.path().canonicalize().unwrap().join("cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        let state = root.join(".crew");
        std::fs::create_dir_all(state.join("agents/a")).unwrap();
        std::fs::write(state.join("crew.db"), "db").unwrap();
        let admin = worktree_admin_dir(&root.join(".git"), &wt.path.canonicalize().unwrap())
            .or_else(|| worktree_admin_dir(&root.join(".git"), &wt.path))
            .expect("the worktree's admin directory");
        let crew_bin = std::env::current_exe().unwrap().canonicalize().unwrap();
        let host = Host::detect(&crew_bin, Path::new("/usr/bin/bwrap"), 9);
        let store = crate::vcs::Store::Git {
            git_dir: root.join(".git"),
        };
        let session = Session {
            workspace: &root,
            store: &store,
            runtime_dir: &run,
            daemon_socket: &socket,
            cargo_home: &cargo,
        };
        let wt_path = wt.path.canonicalize().unwrap();
        let (launcher, _warnings) = launch::helper_launcher(
            &host,
            session,
            &Extra::default(),
            &wt_path,
            Some(&admin),
            None,
        )
        .unwrap_or_else(|e| panic!("building the helper profile: {e}"));
        // Timeout recovery after a helper merge needs the trusted lock location.
        use launch::Launcher;
        assert_eq!(launcher.admin_dir().as_deref(), Some(admin.as_path()));
        let spec = launcher.spec();
        assert!(!spec.network && spec.new_session && spec.pass_env.is_empty());
        let bound = |p: &Path| {
            spec.mounts
                .iter()
                .any(|m| matches!(m, Mount::Bind { path, .. } if path.starts_with(p)))
        };
        assert!(
            spec.mounts.iter().any(|m| matches!(
                m, Mount::Bind { path, writable: true, .. } if *path == wt_path
            )),
            "the worktree is writable"
        );
        let agents = state.join("agents");
        let db = state.join("crew.db");
        assert!(
            !bound(&run) && !bound(&agents) && !bound(&db) && !bound(&socket),
            "{:?}",
            spec.mounts
        );
        assert!(
            !spec.mounts.iter().any(|m| m.path() == state),
            "the state directory is not mounted as a whole"
        );
        assert!(!spec.env.iter().any(|(k, _)| k.starts_with("CREW_")));
        // The administrative directory must hold the empty per-worktree config
        // the sandbox binds read-only.
        assert!(admin.join("config.worktree").is_file());
    }

    #[test]
    fn init_repo_runs_no_configured_command_when_it_unstages_the_state_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".crew")).unwrap();
        out(&root, &["init", "-q", "-b", "main"]).unwrap();
        write(&root.join(".crew"), "crew.db", "db");
        out(&root, &["add", "-f", ".crew/crew.db"]).unwrap();
        let marker = dir.path().join("monitor-ran");
        let monitor = dir.path().join("monitor.sh");
        std::fs::write(&monitor, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&monitor, std::fs::Permissions::from_mode(0o755)).unwrap();
        out(
            &root,
            &["config", "core.fsmonitor", &monitor.to_string_lossy()],
        )
        .unwrap();
        // Positive control: the plain invocation runs it.
        out(&root, &["status", "--porcelain"]).unwrap();
        assert!(marker.exists(), "the control did not run the monitor");
        std::fs::remove_file(&marker).unwrap();
        init_repo(&root).unwrap();
        assert!(
            !marker.exists(),
            "a configured command ran during init_repo"
        );
    }

    #[test]
    fn an_agents_launch_variables_reach_its_git_only_by_the_names_its_spec_passes() {
        use crate::sandbox::spec::Spec;
        use launch::Launcher;
        let spec = Spec {
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: "/opt/crew/bin/crew".into(),
            mounts: vec![],
            env: vec![],
            pass_env: vec!["CREW_IDENTITY".into()],
            cwd: "/work".into(),
            network: true,
            new_session: false,
            min_landlock_abi: crate::sandbox::landlock::MIN_ABI,
            allow_unix: vec![],
            done: None,
        };
        let launch_env = vec![
            (
                "CREW_IDENTITY".to_string(),
                "/state/agents/a/identity.json".to_string(),
            ),
            ("ANTHROPIC_API_KEY".to_string(), "sk-not-passed".to_string()),
        ];
        let mut cmd = git(Path::new("/work"));
        cmd.arg("status");
        let launched = launch::SandboxLauncher::new(spec.clone(), launch_env)
            .launch(&cmd)
            .unwrap();
        let env: std::collections::BTreeMap<String, String> = launched
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(
            env.get("CREW_IDENTITY").map(String::as_str),
            Some("/state/agents/a/identity.json")
        );
        assert!(!env.contains_key("ANTHROPIC_API_KEY"), "{env:?}");
        // A helper launcher carries none.
        let helper = launch::SandboxLauncher::new(spec, Vec::new())
            .launch(&cmd)
            .unwrap();
        assert!(!helper.get_envs().any(|(k, _)| k == "CREW_IDENTITY"));
    }

    #[test]
    fn a_symlinked_admin_directory_is_never_followed_or_written_through() {
        use std::os::unix::fs::symlink;
        let r = repo();
        ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        let admin = r.root.join(".git/worktrees/a");
        let outside = r._dir.path().join("outside-admin");
        std::fs::rename(&admin, &outside).unwrap();
        std::fs::remove_file(outside.join("config.worktree")).unwrap();
        symlink(&outside, &admin).unwrap();
        // Whatever the verdict, nothing is created through the link, and the
        // worktree is not accepted by way of it.
        let result = add_worktree(&r.root, &r.worktrees, "a", "HEAD");
        assert!(
            !outside.join("config.worktree").exists(),
            "config.worktree was created through a symlinked admin directory"
        );
        assert!(
            result.is_err(),
            "the worktree was accepted through a symlink"
        );
        // A symlinked HEAD inside a real admin directory is not read either.
        let r2 = repo();
        ensure_worktree(&r2.root, &r2.worktrees, "b", "HEAD").unwrap();
        let head = r2.root.join(".git/worktrees/b/HEAD");
        let elsewhere = r2._dir.path().join("fake-head");
        std::fs::write(&elsewhere, "ref: refs/heads/agents/b\n").unwrap();
        std::fs::write(&head, "ref: refs/heads/other\n").unwrap();
        std::fs::remove_file(&head).unwrap();
        symlink(&elsewhere, &head).unwrap();
        assert!(add_worktree(&r2.root, &r2.worktrees, "b", "HEAD").is_err());
    }

    #[test]
    fn nothing_the_daemon_reads_from_a_launched_git_decides_where_it_writes() {
        // Under a launcher (an agent's sandbox) git's output is the agent's to
        // forge: paths it prints must not become host file operations.
        let r = repo();
        write(&r.root, "work.txt", "x\n");
        let recorder = Arc::new(Recorder(Default::default(), None));
        launch::with_launcher(recorder.clone(), || {
            commit_all(&r.root, "a", "work").unwrap();
        });
        let seen = recorder.0.lock().unwrap().clone();
        let asked_for_a_path = |argv: &Vec<String>| {
            argv.iter()
                .any(|a| a == "--git-common-dir" || a == "--git-path")
        };
        assert!(
            !seen.iter().any(asked_for_a_path),
            "a path git printed is used on the host: {seen:?}"
        );
        // A stale lock is removed from the administrative directory the daemon
        // knows, through a descriptor, never from a printed path.
        let admin = r._dir.path().join("admin");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("index.lock"), "").unwrap();
        let launcher = Arc::new(Recorder(Default::default(), None).with_admin(admin.clone()));
        launch::with_launcher(launcher.clone(), || clear_stale_index_lock(&r.root));
        assert!(!admin.join("index.lock").exists());
        // A symlink in place of that directory is not followed.
        let real = r._dir.path().join("real-admin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("index.lock"), "").unwrap();
        let link = r._dir.path().join("link-admin");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let launcher = Arc::new(Recorder(Default::default(), None).with_admin(link));
        launch::with_launcher(launcher, || clear_stale_index_lock(&r.root));
        assert!(real.join("index.lock").exists(), "followed a symlink");
    }

    #[test]
    fn each_agent_gets_its_own_worktree_and_branch() {
        let r = repo();
        exclude_state_dir(&r.root).unwrap();
        let a = ensure_worktree(&r.root, &r.worktrees, "claude-1", "HEAD").unwrap();
        let b = ensure_worktree(&r.root, &r.worktrees, "claude-2", "HEAD").unwrap();
        assert_eq!(a.branch, "agents/claude-1");
        assert_eq!(b.branch, "agents/claude-2");
        assert_ne!(a.path, b.path);
        assert!(a.path.join(".git").exists());
        // Reuse is idempotent.
        let again = ensure_worktree(&r.root, &r.worktrees, "claude-1", "HEAD").unwrap();
        assert_eq!((&again.path, &again.branch), (&a.path, &a.branch));
        assert!(!again.created);
        // Files written by one agent are invisible to the other.
        write(&a.path, "only-a.txt", "a");
        assert!(!b.path.join("only-a.txt").exists());
        // The state directory does not show up as untracked in the main checkout.
        assert_eq!(out(&r.root, &["status", "--porcelain"]).unwrap(), "");
        assert!(ensure_worktree(&r.root, &r.worktrees, "../evil", "HEAD").is_err());
    }

    #[test]
    fn commit_all_never_stages_the_state_directory() {
        // Shared mode: the daemon never excluded `.crew/` here, so a commit
        // swept up crew.db and every agent's identity token.
        let r = repo();
        std::fs::create_dir_all(r.root.join(".crew/agents/a")).unwrap();
        write(&r.root, ".crew/crew.db", "db");
        write(&r.root, ".crew/agents/a/identity.json", "{\"token\":\"t\"}");
        write(&r.root, "work.txt", "x\n");
        commit_all(&r.root, "a", "work").unwrap();
        let files = out(&r.root, &["show", "--format=", "--name-only", "HEAD"]).unwrap();
        assert_eq!(files, "work.txt");
    }

    #[test]
    fn commit_all_leaves_a_staged_or_tracked_state_directory_out() {
        let r = repo();
        std::fs::create_dir_all(r.root.join(".crew/agents/a")).unwrap();
        // One state file already tracked, one forced into the index.
        write(&r.root, ".crew/tracked.db", "v1");
        out(&r.root, &["add", "-f", ".crew/tracked.db"]).unwrap();
        out(
            &r.root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "tracked",
            ],
        )
        .unwrap();
        write(&r.root, ".crew/tracked.db", "v2");
        write(&r.root, ".crew/agents/a/identity.json", "{\"token\":\"t\"}");
        out(&r.root, &["add", "-f", ".crew/agents/a/identity.json"]).unwrap();
        write(&r.root, "work.txt", "x\n");
        commit_all(&r.root, "a", "work").unwrap();
        let files = out(&r.root, &["show", "--format=", "--name-only", "HEAD"]).unwrap();
        assert_eq!(files, "work.txt");
        // The tracked file was not removed from the repository either.
        assert!(out(&r.root, &["ls-files", ".crew/tracked.db"])
            .unwrap()
            .contains("tracked.db"));
    }

    #[test]
    fn the_initial_commit_leaves_a_staged_state_directory_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".crew")).unwrap();
        out(&root, &["init", "-q", "-b", "main"]).unwrap();
        write(&root.join(".crew"), "crew.db", "db");
        out(&root, &["add", "-f", ".crew/crew.db"]).unwrap();
        init_repo(&root).unwrap();
        assert_eq!(
            out(&root, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            ""
        );
    }

    #[test]
    fn init_does_not_commit_a_staged_team_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        out(&root, &["init", "-q", "-b", "main"]).unwrap();
        write(&root, "crew.yaml", "project: x\n");
        // Untracked, it is crew's own business and is left alone.
        init_repo(&root).unwrap();
        assert_eq!(
            out(&root, &["show", "--format=", "--name-only", "HEAD"]).unwrap(),
            ""
        );
        let staged = dir.path().join("staged");
        std::fs::create_dir_all(&staged).unwrap();
        out(&staged, &["init", "-q", "-b", "main"]).unwrap();
        write(&staged, "crew.yaml", "project: x\n");
        out(&staged, &["add", "crew.yaml"]).unwrap();
        assert!(init_repo(&staged).is_err());
        assert!(!has_commits(&staged));
    }

    #[test]
    fn commit_and_cherry_pick_move_work_between_agents() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "claude-2", "HEAD").unwrap();
        let b = ensure_worktree(&r.root, &r.worktrees, "claude-3", "HEAD").unwrap();

        assert!(commit_all(&a.path, "claude-2", "empty").is_err());
        write(&a.path, "api.rs", "pub fn api() {}\n");
        let commit = commit_all(&a.path, "claude-2", "API implementation complete").unwrap();
        assert_eq!(commit.branch, "agents/claude-2");
        assert_eq!(commit.files_changed, 1);
        assert_eq!(commit.summary, "API implementation complete");
        assert_eq!(
            out(&a.path, &["log", "-1", "--format=%an <%ae>"]).unwrap(),
            "claude-2 <claude-2@crew.local>"
        );

        let picked = cherry_pick(&b.path, "claude-3", &commit.short).unwrap();
        assert_eq!(picked.source, commit.sha);
        assert_eq!(picked.author_email, "claude-2@crew.local");
        let picked = picked.outcome;
        assert!(picked.ok);
        assert!(b.path.join("api.rs").exists());
        assert!(status(&b.path).unwrap().contains("working tree clean"));
    }

    #[test]
    fn conflicting_cherry_pick_is_aborted_and_reported() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        let b = ensure_worktree(&r.root, &r.worktrees, "b", "HEAD").unwrap();
        write(&a.path, "shared.txt", "from a\n");
        let ca = commit_all(&a.path, "a", "a's version").unwrap();
        write(&b.path, "shared.txt", "from b\n");
        commit_all(&b.path, "b", "b's version").unwrap();

        let outcome = cherry_pick(&b.path, "b", &ca.sha).unwrap().outcome;
        assert!(!outcome.ok);
        let conflict = outcome.conflict.unwrap();
        assert_eq!(conflict.operation, "cherry-pick");
        assert_eq!(conflict.files, ["shared.txt"]);
        // b's file was not overwritten and the worktree is clean again.
        assert_eq!(
            std::fs::read_to_string(b.path.join("shared.txt")).unwrap(),
            "from b\n"
        );
        assert!(!dirty(&b.path).unwrap());
    }

    #[test]
    fn cherry_pick_refuses_a_dirty_worktree_and_bad_revisions() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        write(&a.path, "wip.txt", "wip");
        assert!(cherry_pick(&a.path, "a", "abcdef1").is_err());
        assert!(validate_rev("--upload-pack=evil").is_err());
        assert!(validate_rev("a..b").is_err());
        assert!(validate_rev("has space").is_err());
        assert!(validate_rev("").is_err());
        assert!(validate_rev("a91ec21").is_ok());
        assert!(validate_rev("agents/claude-2").is_ok());
        assert!(validate_rev("HEAD").is_ok());
    }

    #[test]
    fn integration_merges_clean_branches_and_reports_conflicts() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        let b = ensure_worktree(&r.root, &r.worktrees, "b", "HEAD").unwrap();
        let c = ensure_worktree(&r.root, &r.worktrees, "c", "HEAD").unwrap();
        write(&a.path, "a.txt", "a\n");
        commit_all(&a.path, "a", "add a").unwrap();
        write(&b.path, "shared.txt", "b\n");
        commit_all(&b.path, "b", "b shared").unwrap();
        write(&c.path, "shared.txt", "c\n");
        commit_all(&c.path, "c", "c shared").unwrap();

        let agents: Vec<String> = ["a", "b", "c", "idle"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (wt, steps) = integrate(&r.root, &r.worktrees, "HEAD", &agents, &mut |_| {}).unwrap();
        assert_eq!(wt.branch, INTEGRATION_BRANCH);
        assert_eq!(steps.len(), 3);
        assert!(steps[0].outcome.ok && steps[1].outcome.ok);
        assert!(!steps[2].outcome.ok);
        assert_eq!(
            steps[2].outcome.conflict.as_ref().unwrap().files,
            ["shared.txt"]
        );
        assert!(wt.path.join("a.txt").exists());
        assert_eq!(
            std::fs::read_to_string(wt.path.join("shared.txt")).unwrap(),
            "b\n"
        );
        // Running it again only retries what is still missing.
        let (_, again) = integrate(&r.root, &r.worktrees, "HEAD", &agents, &mut |_| {}).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].agent, "c");
    }

    /// Runs only git started in one directory (`-C <dir>`), as a helper
    /// sandbox that mounts only that worktree would, and records it.
    struct ConfinedTo(PathBuf, std::sync::Mutex<Vec<Vec<String>>>);

    impl launch::Launcher for ConfinedTo {
        fn launch(&self, cmd: &Command) -> std::io::Result<Command> {
            let argv: Vec<String> = launch::argv(cmd)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let inside = argv.get(1).map(String::as_str) == Some("-C")
                && argv.get(2).map(PathBuf::from) == Some(self.0.clone());
            self.1.lock().unwrap().push(argv);
            if !inside {
                return Err(std::io::Error::other("outside the worktree"));
            }
            let mut same = Command::new(cmd.get_program());
            same.args(cmd.get_args());
            for (k, v) in cmd.get_envs() {
                match v {
                    Some(v) => same.env(k, v),
                    None => same.env_remove(k),
                };
            }
            Ok(same)
        }
    }

    #[test]
    fn merging_the_agents_runs_git_only_in_the_integration_worktree() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        write(&a.path, "from-a.txt", "a\n");
        commit_all(&a.path, "a", "work of a").unwrap();
        let (worktree, needs_population) =
            add_worktree(&r.root, &r.worktrees, INTEGRATION_DIR, "HEAD").unwrap();
        assert!(needs_population);
        populate_worktree(&worktree).unwrap();

        let confined = Arc::new(ConfinedTo(worktree.path.clone(), Default::default()));
        let steps = launch::with_launcher(confined.clone(), || {
            merge_agents(
                &worktree,
                &["a".to_string(), "ghost".to_string()],
                &mut |_| {},
            )
        })
        .unwrap();
        let seen = confined.1.lock().unwrap().clone();
        assert_eq!(steps.len(), 1, "{seen:?}");
        assert!(steps[0].outcome.ok, "{:?}", steps[0].outcome);
        assert!(worktree.path.join("from-a.txt").exists());
        let outside: Vec<_> = seen
            .iter()
            .filter(|argv| argv.get(2).map(PathBuf::from) != Some(worktree.path.clone()))
            .collect();
        assert!(outside.is_empty(), "git outside the worktree: {outside:?}");
    }

    /// Fails every git command the way a sandbox that cannot start does:
    /// the launcher refuses, or the command exits 125 (sandbox-exec).
    enum Broken {
        Refuses,
        Exits125,
    }

    impl launch::Launcher for Broken {
        fn launch(&self, _: &Command) -> std::io::Result<Command> {
            match self {
                Broken::Refuses => Err(std::io::Error::other("no sandbox")),
                Broken::Exits125 => {
                    let mut cmd = Command::new("sh");
                    cmd.args(["-c", "exit 125"]);
                    Ok(cmd)
                }
            }
        }
    }

    #[test]
    fn merging_the_agents_fails_when_its_git_cannot_run_rather_than_finding_no_branches() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        write(&a.path, "from-a.txt", "a\n");
        commit_all(&a.path, "a", "work of a").unwrap();
        let (worktree, _) = add_worktree(&r.root, &r.worktrees, INTEGRATION_DIR, "HEAD").unwrap();
        populate_worktree(&worktree).unwrap();
        for broken in [Broken::Refuses, Broken::Exits125] {
            let result = launch::with_launcher(Arc::new(broken), || {
                merge_agents(&worktree, &["a".to_string()], &mut |_| {})
            });
            assert!(result.is_err(), "{result:?}");
        }
    }

    /// Runs every git command but `rev-list`, which it refuses, and keeps
    /// the arguments of what it was asked to run.
    #[derive(Default)]
    struct NoRevList(std::sync::Mutex<Vec<Vec<String>>>);

    impl launch::Launcher for NoRevList {
        fn launch(&self, cmd: &Command) -> std::io::Result<Command> {
            let argv: Vec<String> = launch::argv(cmd)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let refused = argv.iter().any(|a| a == "rev-list");
            self.0.lock().unwrap().push(argv);
            if refused {
                return Err(std::io::Error::other("no sandbox"));
            }
            let mut same = Command::new(cmd.get_program());
            same.args(cmd.get_args());
            for (k, v) in cmd.get_envs() {
                match v {
                    Some(v) => same.env(k, v),
                    None => same.env_remove(k),
                };
            }
            Ok(same)
        }
    }

    #[test]
    fn a_refused_ahead_count_stops_the_integration_before_any_merge() {
        let r = repo();
        let a = ensure_worktree(&r.root, &r.worktrees, "a", "HEAD").unwrap();
        write(&a.path, "from-a.txt", "a\n");
        commit_all(&a.path, "a", "work of a").unwrap();
        let (worktree, _) = add_worktree(&r.root, &r.worktrees, INTEGRATION_DIR, "HEAD").unwrap();
        populate_worktree(&worktree).unwrap();
        let launcher = Arc::new(NoRevList::default());
        let result = launch::with_launcher(launcher.clone(), || {
            merge_agents(&worktree, &["a".to_string()], &mut |_| {})
        });
        let seen = launcher.0.lock().unwrap().clone();
        assert!(result.is_err(), "{result:?}");
        assert!(
            !seen.iter().any(|argv| argv.iter().any(|a| a == "merge")),
            "a merge was attempted: {seen:?}"
        );
        assert!(!worktree.path.join("from-a.txt").exists());
    }
}
