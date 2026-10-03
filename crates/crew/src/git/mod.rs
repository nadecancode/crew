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

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::rpc::{CommitInfo, ConflictReport, GitOutcome};
use crew_core::util::is_valid_id;

/// Branch namespace for agent work.
pub const BRANCH_PREFIX: &str = "agents/";
/// Branch that collects everyone's work when the session is integrated.
pub const INTEGRATION_BRANCH: &str = "agents/integration";
const INTEGRATION_DIR: &str = "_integration";
const MAX_DIFF_BYTES: usize = 48 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
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

fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    git(dir)
        .args(args)
        .output()
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
    run(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success() && text(&o.stdout) == "true")
        .unwrap_or(false)
}

/// The top-level directory of the repository containing `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(out(dir, &["rev-parse", "--show-toplevel"])?))
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
        out(dir, &["init", "-q", "-b", "main"])?;
    }
    require_root(dir)?;
    if run(dir, &["rev-parse", "--verify", "-q", "HEAD"])?
        .status
        .success()
    {
        return Ok(());
    }
    let mut cmd = git(dir);
    with_identity(&mut cmd, "crew");
    let output = cmd
        .args(["commit", "-q", "--allow-empty", "-m", "Initial commit"])
        .output()?;
    if !output.status.success() {
        bail!(
            "creating the initial commit failed: {}",
            text(&output.stderr)
        );
    }
    Ok(())
}

pub fn has_commits(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--verify", "-q", "HEAD"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Keep crew's state directory out of `git status` without touching the
/// project's own `.gitignore`.
pub fn exclude_state_dir(repo: &Path) -> Result<()> {
    let git_dir = PathBuf::from(out(repo, &["rev-parse", "--git-common-dir"])?);
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
    run(
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

/// Create (or reuse) the worktree and branch for an agent.
pub fn ensure_worktree(
    repo: &Path,
    worktrees_dir: &Path,
    agent: &str,
    base: &str,
) -> Result<Worktree> {
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
        // Left over from an earlier run: reuse it if it is on the right branch.
        let current = out(&path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
        if current == branch {
            return Ok(Worktree { path, branch });
        }
        bail!(
            "{} already exists but is on branch '{current}', expected '{branch}'",
            path.display()
        );
    }
    std::fs::create_dir_all(worktrees_dir)?;
    // Forget worktrees whose directories were deleted by hand.
    let _ = run(repo, &["worktree", "prune"]);
    let path_str = path.to_string_lossy().into_owned();
    if branch_exists(repo, &branch) {
        out(repo, &["worktree", "add", "-q", &path_str, &branch])?;
    } else {
        validate_rev(base)?;
        out(
            repo,
            &["worktree", "add", "-q", "-b", &branch, &path_str, base],
        )?;
    }
    Ok(Worktree { path, branch })
}

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
    })
}

/// Stage everything in the worktree and commit it as `agent`.
pub fn commit_all(dir: &Path, agent: &str, message: &str) -> Result<CommitInfo> {
    let message = message.trim();
    if message.is_empty() {
        bail!("commit message is empty");
    }
    require_root(dir)?;
    exclude_state_dir(dir)?;
    out(dir, &["add", "-A", "--", "."])?;
    let staged = run(dir, &["diff", "--cached", "--quiet"])?;
    if staged.status.success() {
        bail!("nothing to commit: the working tree has no changes");
    }
    let mut cmd = git(dir);
    with_identity(&mut cmd, agent);
    // `-m <message>` passes the message as a single argument. The project's
    // own commit hooks still run; a failing hook is reported back to the agent.
    let output = cmd.args(["commit", "-q", "-m", message]).output()?;
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

/// Apply another agent's commit onto this worktree's branch.
pub fn cherry_pick(dir: &Path, agent: &str, commit: &str) -> Result<GitOutcome> {
    validate_rev(commit)?;
    require_root(dir)?;
    if dirty(dir)? {
        bail!("the worktree has uncommitted changes; commit them before cherry-picking");
    }
    out(
        dir,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )
    .map_err(|_| anyhow!("unknown commit '{commit}'"))?;
    let mut cmd = git(dir);
    with_identity(&mut cmd, agent);
    let output = cmd.args(["cherry-pick", "-x", commit]).output()?;
    finish(dir, "cherry-pick", &["cherry-pick", "--abort"], output)
}

/// Merge a branch (or commit) into this worktree's branch.
pub fn merge(dir: &Path, agent: &str, rev: &str) -> Result<GitOutcome> {
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
    let output = cmd
        .args(["merge", "--no-ff", "--no-edit", "-m", &message, rev])
        .output()?;
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
) -> Result<(Worktree, Vec<IntegrationStep>)> {
    let worktree = ensure_worktree(repo, worktrees_dir, INTEGRATION_DIR, base)?;
    let mut steps = Vec::new();
    for agent in agents {
        let branch = branch_for(agent);
        if !branch_exists(repo, &branch) {
            continue;
        }
        let ahead = out(
            &worktree.path,
            &["rev-list", "--count", &format!("HEAD..{branch}")],
        )
        .unwrap_or_default();
        if ahead == "0" {
            continue;
        }
        let outcome = merge(&worktree.path, "crew", &branch)?;
        steps.push(IntegrationStep {
            agent: agent.clone(),
            outcome,
        });
    }
    Ok((worktree, steps))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        init_repo(&nested).unwrap();
        assert!(is_root(&nested));
        assert!(has_commits(&nested));
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
        assert_eq!(
            ensure_worktree(&r.root, &r.worktrees, "claude-1", "HEAD").unwrap(),
            a
        );
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

        let outcome = cherry_pick(&b.path, "b", &ca.sha).unwrap();
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
        let (wt, steps) = integrate(&r.root, &r.worktrees, "HEAD", &agents).unwrap();
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
        let (_, again) = integrate(&r.root, &r.worktrees, "HEAD", &agents).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].agent, "c");
    }
}
