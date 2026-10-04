//! The jj backend against a real jj (notes/jj-design.md §tests). Without jj
//! on PATH (or `CREW_JJ`) these print `skipped: jj is not on PATH` and pass,
//! unless `CREW_TEST_VCS=jj` asks for jj, which makes a missing jj a failure.

use super::*;
use std::sync::Arc;

use crate::git::launch;

/// The jj under test, or None (and a note) when there is none.
fn real_jj() -> Option<(PathBuf, String)> {
    match crate::vcs::find_jj() {
        Ok(found) => Some(found),
        Err(why) => {
            if std::env::var("CREW_TEST_VCS").as_deref() == Ok("jj") {
                panic!("CREW_TEST_VCS=jj, but {}", why.reason());
            }
            eprintln!("skipped: jj is not on PATH ({})", why.reason());
            None
        }
    }
}

/// A plain-git project with one commit (`a.txt`, `b.txt`) on `main`, and
/// its jj backend, prepared.
struct Project {
    dir: tempfile::TempDir,
    ws: PathBuf,
    worktrees: PathBuf,
    jj: Jj,
}

fn sh_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=u", "-c", "user.email=u@example.com"])
        .args(args)
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap()
}

/// A git repository at `ws` with one commit.
fn git_project(ws: &Path) {
    std::fs::create_dir_all(ws).unwrap();
    sh_git(ws, &["init", "-q", "-b", "main"]);
    write(ws, "a.txt", "hello\ntwo\n");
    write(ws, "b.txt", "world\n");
    sh_git(ws, &["add", "-A"]);
    sh_git(ws, &["commit", "-q", "-m", "first"]);
}

fn project() -> Option<Project> {
    project_with(None)
}

/// `project`, with `program` (a wrapper) in place of the real jj.
fn project_with(program: Option<&Path>) -> Option<Project> {
    let (real, version) = real_jj()?;
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap().join("proj");
    git_project(&ws);
    let state = ws.join(".crew");
    std::fs::create_dir_all(&state).unwrap();
    let jj = Jj::new(program.unwrap_or(&real), &version, &ws, &state);
    assert!(jj.prepare(&ws).unwrap(), "a project with a commit");
    Some(Project {
        worktrees: state.join("worktrees"),
        dir,
        ws,
        jj,
    })
}

impl Project {
    fn workspace(&self, id: &str) -> Workspace {
        self.workspace_at(id, "HEAD")
    }

    fn workspace_at(&self, id: &str, base: &str) -> Workspace {
        let (workspace, populate) = self.jj.add_workspace(&self.worktrees, id, base).unwrap();
        if populate {
            self.jj.populate(&workspace).unwrap();
        }
        workspace
    }

    fn git(&self, args: &[&str]) -> String {
        sh_git(&self.ws, args)
    }

    /// Where `agents/<id>` is for plain git.
    fn branch(&self, id: &str) -> String {
        self.git(&["rev-parse", &format!("refs/heads/agents/{id}")])
    }

    /// The jj bookmark `agents/<id>`.
    fn bookmark(&self, id: &str) -> Option<String> {
        self.jj
            .bookmark(&self.jj.handle, CREW, &format!("agents/{id}"))
            .unwrap()
    }

    /// The agent's last commit (`@-` of its workspace).
    fn head(&self, w: &Workspace) -> String {
        self.jj.state(&w.path, CREW).unwrap().commit
    }

    fn clean(&self, w: &Workspace) -> bool {
        self.jj.pre(&w.path, CREW).unwrap().0
    }

    /// `jj` by hand in `dir`, as `who`, with crew's pins.
    fn jj_by_hand(&self, dir: &Path, who: &str, args: &[&str]) -> String {
        self.jj.out(dir, who, args).unwrap()
    }
}

#[test]
fn prepare_makes_a_hidden_store_and_leaves_the_checkout_alone() {
    let Some(p) = project() else { return };
    let handle = p.ws.join(".crew/jj");
    // Only `.jj` in the handle: the files jj checked out there are gone.
    let entries: Vec<_> = std::fs::read_dir(&handle)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    assert_eq!(entries, [".jj"]);
    assert!(!p.ws.join(".jj").exists(), "a .jj in the project root");
    assert_eq!(read(&p.ws.join(".crew"), ".gitignore"), "*\n");
    assert_eq!(
        read(&handle.join(".jj/repo/store"), "git_target").trim(),
        p.ws.join(".git").to_string_lossy()
    );
    assert_eq!(
        p.jj.store(),
        Store::Jj {
            git_dir: p.ws.join(".git"),
            repo_dir: handle.join(".jj/repo"),
        }
    );
    let head = p.git(&["rev-parse", "HEAD"]);
    let index = p.git(&["ls-files", "-s"]);
    // Idempotent.
    assert!(p.jj.prepare(&p.ws).unwrap());
    let w = p.workspace("a");
    write(&w.path, "new.txt", "x\n");
    p.jj.commit_all(&w.path, "a", "work").unwrap();
    assert_eq!(p.git(&["status", "--porcelain"]), "");
    assert_eq!(p.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(read(&p.ws.join(".git"), "HEAD"), "ref: refs/heads/main\n");
    assert_eq!(p.git(&["ls-files", "-s"]), index);
    assert!(!p.ws.join(".jj").exists());
}

#[test]
fn a_project_without_a_commit_has_nothing_to_start_from() {
    let Some((program, version)) = real_jj() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    sh_git(&ws, &["init", "-q", "-b", "main"]);
    let jj = Jj::new(&program, &version, &ws, &ws.join(".crew"));
    assert!(!jj.prepare(&ws).unwrap());
    // Not a repository at all.
    let bare = dir.path().canonicalize().unwrap().join("plain");
    std::fs::create_dir_all(&bare).unwrap();
    let jj = Jj::new(&program, &version, &bare, &bare.join(".crew"));
    let err = jj.prepare(&bare).unwrap_err();
    assert!(
        err.to_string().contains("not the root of a git repository"),
        "{err:#}"
    );
}

#[test]
fn each_agent_gets_its_own_workspace_and_bookmark() {
    let Some(p) = project() else { return };
    let base = p.git(&["rev-parse", "HEAD"]);
    let a = p.workspace("claude-1");
    let b = p.workspace("claude-2");
    assert_eq!(a.branch, "agents/claude-1");
    assert!(a.created);
    assert_ne!(a.path, b.path);
    assert_eq!(read(&a.path, "a.txt"), "hello\ntwo\n", "populated");
    assert_eq!(p.bookmark("claude-1").as_deref(), Some(base.as_str()));
    assert_eq!(p.branch("claude-1"), base, "exported for plain git");
    // The guard keeps plain git from reaching the user's checkout.
    assert_eq!(read(&a.path, ".git"), String::from_utf8_lossy(GUARD));
    let walked = Command::new("git")
        .arg("-C")
        .arg(&a.path)
        .args(["rev-parse", "--show-toplevel"])
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(
        !walked.status.success(),
        "git found a repository from the workspace"
    );
    // Reuse is idempotent: found, not made, populated again.
    let (again, populate) =
        p.jj.add_workspace(&p.worktrees, "claude-1", "HEAD")
            .unwrap();
    assert_eq!(
        (&again.path, &again.branch, again.created),
        (&a.path, &a.branch, false)
    );
    assert!(populate);
    p.jj.populate(&again).unwrap();
    // Files written by one agent are invisible to the other.
    write(&a.path, "only-a.txt", "a");
    assert!(!b.path.join("only-a.txt").exists());
    assert!(p.jj.add_workspace(&p.worktrees, "../evil", "HEAD").is_err());
}

#[test]
fn a_subagent_starts_from_its_parents_branch() {
    let Some(p) = project() else { return };
    let parent = p.workspace("lead");
    write(&parent.path, "plan.txt", "plan\n");
    let made = p.jj.commit_all(&parent.path, "lead", "plan").unwrap();
    let child = p.workspace_at("helper", "agents/lead");
    assert_eq!(p.bookmark("helper").as_deref(), Some(made.sha.as_str()));
    assert_eq!(read(&child.path, "plan.txt"), "plan\n");
}

#[test]
fn a_removed_workspace_is_added_again_and_a_foreign_directory_is_refused() {
    let Some(p) = project() else { return };
    let a = p.workspace("a");
    write(&a.path, "kept.txt", "k\n");
    let made = p.jj.commit_all(&a.path, "a", "kept").unwrap();
    std::fs::remove_dir_all(&a.path).unwrap();
    let again = p.workspace("a");
    assert!(again.created);
    assert_eq!(read(&again.path, "kept.txt"), "k\n", "back at its bookmark");
    assert_eq!(p.head(&again), made.sha);
    // A directory jj did not make, with something in it, is left alone.
    let foreign = p.worktrees.join("b");
    std::fs::create_dir_all(&foreign).unwrap();
    write(&foreign, "mine.txt", "not jj's\n");
    let err = p.jj.add_workspace(&p.worktrees, "b", "HEAD").unwrap_err();
    assert!(
        err.to_string()
            .contains(&foreign.to_string_lossy().into_owned()),
        "{err:#}"
    );
    assert_eq!(read(&foreign, "mine.txt"), "not jj's\n");
}

#[test]
fn commit_publishes_the_agents_work_to_plain_git() {
    let Some(p) = project() else { return };
    let a = p.workspace("a");
    let err = p.jj.commit_all(&a.path, "a", "nothing").unwrap_err();
    assert!(err.to_string().contains("nothing to commit"), "{err:#}");
    assert!(p.jj.commit_all(&a.path, "a", "  ").is_err());
    write(&a.path, "a.txt", "hello\nA\n");
    write(&a.path, "n.txt", "n\n");
    // Larger than jj's default limit for new files (1 MiB).
    std::fs::write(a.path.join("big.bin"), vec![0u8; 1_500_000]).unwrap();
    let commit =
        p.jj.commit_all(&a.path, "a", "-a: change a.txt\n\nbody")
            .unwrap();
    assert_eq!(commit.summary, "-a: change a.txt");
    assert_eq!(commit.branch, "agents/a");
    assert_eq!(commit.files_changed, 3);
    assert_eq!(commit.short, &commit.sha[..8]);
    let change = commit.change_id.clone().expect("a change id");
    assert!(
        change.bytes().all(|b| (b'k'..=b'z').contains(&b)),
        "{change}"
    );
    assert_eq!(p.branch("a"), commit.sha, "git sees the commit");
    assert_eq!(p.bookmark("a").as_deref(), Some(commit.sha.as_str()));
    assert_eq!(
        p.git(&["log", "-1", "--format=%an <%ae>|%cn", "agents/a"]),
        "a <a@crew.local>|a"
    );
    let files = p.git(&["ls-tree", "-r", "--name-only", "agents/a"]);
    assert!(files.lines().any(|f| f == "big.bin"), "{files}");
    assert!(
        !files.lines().any(|f| f == ".git" || f.starts_with(".jj")),
        "the guard or .jj was committed: {files}"
    );
    assert!(p.clean(&a));
    assert!(p.jj.status(&a.path).unwrap().contains("working tree clean"));
}

#[test]
fn status_and_diff_describe_the_workspace() {
    let Some(p) = project() else { return };
    let a = p.workspace("a");
    write(&a.path, "n.txt", "n\n");
    p.jj.commit_all(&a.path, "a", "add n").unwrap();
    write(&a.path, "b.txt", "world\nmore\n");
    let status = p.jj.status(&a.path).unwrap();
    let lines: Vec<&str> = status.lines().collect();
    assert_eq!(lines[0], "branch agents/a", "{status}");
    assert!(
        lines[1].starts_with("HEAD ") && lines[1].ends_with(" add n"),
        "{status}"
    );
    assert_eq!(lines[2], "M b.txt", "{status}");
    let diff = p.jj.diff(&a.path, None).unwrap();
    assert!(diff.starts_with("M b.txt"), "{diff}");
    assert!(diff.lines().any(|l| l == "+more"), "{diff}");
    let since = p.jj.diff(&a.path, Some("main")).unwrap();
    assert!(since.contains("2 files changed"), "{since}");
    assert!(since.contains("diff --git a/n.txt b/n.txt"), "{since}");
    for rev in ["HEAD", "agents/a"] {
        let diff = p.jj.diff(&a.path, Some(rev)).unwrap();
        assert!(!diff.contains("n.txt"), "{rev}: {diff}");
        assert!(diff.contains("+more"), "{rev}: {diff}");
    }
    let err = p.jj.diff(&a.path, Some("nosuchbranch")).unwrap_err();
    assert!(err.to_string().contains("unknown revision"), "{err:#}");
    let err = p.jj.diff(&a.path, Some("all()")).unwrap_err();
    assert!(err.to_string().contains("invalid revision"), "{err:#}");
}

#[test]
fn agent_revisions_are_rewritten_and_never_passed_as_revsets() {
    assert_eq!(revset("a91ec21").unwrap(), "commit_id(a91ec21)");
    assert_eq!(revset("A91EC21").unwrap(), "commit_id(a91ec21)");
    assert_eq!(revset("kzvsvsvn").unwrap(), "change_id(kzvsvsvn)");
    assert_eq!(revset("agents/claude-2").unwrap(), "\"agents/claude-2\"");
    assert_eq!(revset("HEAD").unwrap(), "@-");
    assert_eq!(revset("main").unwrap(), "\"main\"");
    assert_eq!(revset("release/1.2").unwrap(), "\"release/1.2\"");
    for bad in [
        "main~2",
        "HEAD^",
        "a|b",
        "all()",
        "a..b",
        "-r",
        "--config=x",
        "",
        "has space",
        "x\"y",
        "a::",
    ] {
        let err = revset(bad).unwrap_err();
        assert!(
            err.to_string().contains("invalid revision"),
            "{bad}: {err:#}"
        );
    }
    assert_eq!(parse_version("jj 0.44.0"), Some((0, 44, 0)));
    assert_eq!(parse_version("jj 0.45.1-abc123"), Some((0, 45, 1)));
    assert_eq!(parse_version("git version 2.0"), None);
}

#[test]
fn commit_and_cherry_pick_move_work_between_agents() {
    let Some(p) = project() else { return };
    let a = p.workspace("claude-2");
    let b = p.workspace("claude-3");
    write(&a.path, "api.rs", "pub fn api() {}\n");
    let commit =
        p.jj.commit_all(&a.path, "claude-2", "API implementation complete")
            .unwrap();
    let picked =
        p.jj.cherry_pick(&b.path, "claude-3", &commit.short)
            .unwrap();
    assert!(picked.outcome.ok, "{:?}", picked.outcome);
    assert_eq!(picked.source, commit.sha);
    assert_eq!(picked.author_email, "claude-2@crew.local");
    let applied = picked.outcome.commit.unwrap();
    assert_eq!(read(&b.path, "api.rs"), "pub fn api() {}\n");
    assert_eq!(p.branch("claude-3"), applied.sha);
    assert!(p.clean(&b));
    // git's -x trailer; the author is kept, the picker commits.
    let message = p.git(&["log", "-1", "--format=%B", "agents/claude-3"]);
    assert!(
        message.contains(&format!("(cherry picked from commit {})", commit.sha)),
        "{message}"
    );
    assert_eq!(
        p.git(&["log", "-1", "--format=%an|%cn", "agents/claude-3"]),
        "claude-2|claude-3"
    );
    // By change id too.
    let c = p.workspace("claude-4");
    let by_change =
        p.jj.cherry_pick(
            &c.path,
            "claude-4",
            &commit.change_id.clone().unwrap()[..12],
        )
        .unwrap();
    assert_eq!(by_change.source, commit.sha);
}

/// Three agents from the same commit: `a` and `d` change the same line of
/// `a.txt` differently, `b` adds a file.
fn rivals(p: &Project) -> (Workspace, Workspace, Workspace, CommitInfo, CommitInfo) {
    let a = p.workspace("a");
    let b = p.workspace("b");
    let d = p.workspace("d");
    write(&a.path, "a.txt", "hello\nA\n");
    p.jj.commit_all(&a.path, "a", "a's version").unwrap();
    write(&b.path, "bnew.txt", "b new\n");
    let b1 = p.jj.commit_all(&b.path, "b", "b: add bnew").unwrap();
    write(&d.path, "a.txt", "hello\nD\n");
    let d1 = p.jj.commit_all(&d.path, "d", "d's version").unwrap();
    (a, b, d, b1, d1)
}

#[test]
fn a_conflicting_cherry_pick_leaves_no_trace() {
    let Some(p) = project() else { return };
    let (a, _, _, b1, d1) = rivals(&p);
    let before = p.head(&a);
    let outcome = p.jj.cherry_pick(&a.path, "a", &d1.sha).unwrap().outcome;
    assert!(!outcome.ok);
    assert!(outcome.commit.is_none());
    let conflict = outcome.conflict.unwrap();
    assert_eq!(conflict.operation, "cherry-pick");
    assert_eq!(conflict.files, ["a.txt"]);
    assert!(
        conflict.detail.contains("conflicts in 1 file(s)"),
        "{}",
        conflict.detail
    );
    assert!(outcome.text.contains("the worktree is unchanged"));
    assert_eq!(p.head(&a), before);
    assert_eq!(read(&a.path, "a.txt"), "hello\nA\n");
    assert!(p.clean(&a));
    assert_eq!(p.bookmark("a").as_deref(), Some(before.as_str()));
    assert_eq!(p.branch("a"), before);
    // Already applied: empty, refused and rolled back.
    p.jj.cherry_pick(&a.path, "a", &b1.sha).unwrap();
    let applied = p.head(&a);
    let err = p.jj.cherry_pick(&a.path, "a", &b1.sha).unwrap_err();
    assert!(err.to_string().contains("is empty"), "{err:#}");
    assert_eq!(p.head(&a), applied);
    assert!(p.clean(&a));
}

#[test]
fn a_conflicting_merge_leaves_no_trace() {
    let Some(p) = project() else { return };
    let (a, b, _, _, _) = rivals(&p);
    let merged = p.jj.merge(&b.path, "b", "agents/a").unwrap();
    assert!(merged.ok, "{merged:?}");
    let commit = merged.commit.unwrap();
    assert_eq!(commit.summary, "Merge agents/a");
    assert_eq!(p.branch("b"), commit.sha);
    assert_eq!(
        p.git(&["rev-list", "--parents", "-1", "agents/b"])
            .split(' ')
            .count(),
        3,
        "a merge commit with two parents"
    );
    assert_eq!(p.git(&["log", "-1", "--format=%an", "agents/b"]), "b");
    assert_eq!(read(&b.path, "a.txt"), "hello\nA\n");
    // Nothing new: no commit is made.
    let again = p.jj.merge(&b.path, "b", "agents/a").unwrap();
    assert!(again.ok);
    assert_eq!(again.text, "Already up to date.");
    assert_eq!(p.head(&b), commit.sha);
    // A conflict is reported and taken back.
    let before = p.head(&b);
    let outcome = p.jj.merge(&b.path, "b", "agents/d").unwrap();
    assert!(!outcome.ok);
    let conflict = outcome.conflict.unwrap();
    assert_eq!(conflict.operation, "merge");
    assert_eq!(conflict.files, ["a.txt"]);
    assert_eq!(p.head(&b), before);
    assert_eq!(read(&b.path, "a.txt"), "hello\nA\n");
    assert!(p.clean(&b));
    assert_eq!(p.bookmark("b").as_deref(), Some(before.as_str()));
    assert_eq!(p.branch("b"), before);
    let _ = a;
    let err = p.jj.merge(&b.path, "b", "agents/nobody").unwrap_err();
    assert!(err.to_string().contains("unknown revision"), "{err:#}");
}

#[test]
fn operations_refuse_uncommitted_work_and_unknown_or_invalid_revisions() {
    let Some(p) = project() else { return };
    let (a, _, _, b1, _) = rivals(&p);
    write(&a.path, "wip.txt", "wip");
    let err = p.jj.cherry_pick(&a.path, "a", &b1.sha).unwrap_err();
    assert!(err.to_string().contains("uncommitted changes"), "{err:#}");
    let err = p.jj.merge(&a.path, "a", "agents/b").unwrap_err();
    assert!(err.to_string().contains("uncommitted changes"), "{err:#}");
    p.jj.commit_all(&a.path, "a", "wip").unwrap();
    let err =
        p.jj.cherry_pick(&a.path, "a", "deadbeefdeadbeef")
            .unwrap_err();
    assert!(
        err.to_string()
            .contains("unknown commit 'deadbeefdeadbeef'"),
        "{err:#}"
    );
    for bad in ["--upload-pack=evil", "a..b", "all()", "HEAD^"] {
        let err = p.jj.cherry_pick(&a.path, "a", bad).unwrap_err();
        assert!(
            err.to_string().contains("invalid revision"),
            "{bad}: {err:#}"
        );
    }
    assert!(p.jj.merge(&a.path, "a", "all()").is_err());
}

#[test]
fn integration_merges_what_merges_reports_conflicts_and_skips_the_rest() {
    let Some(p) = project() else { return };
    let (_, _, _, _, _) = rivals(&p);
    let agents: Vec<String> = ["a", "b", "d", "idle", "ghost"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // `idle` has a workspace and nothing new; `ghost` has no branch.
    p.workspace("idle");
    let (integration, populate) =
        p.jj.add_workspace(&p.worktrees, INTEGRATION_DIR, "HEAD")
            .unwrap();
    assert_eq!(integration.branch, INTEGRATION_BRANCH);
    if populate {
        p.jj.populate(&integration).unwrap();
    }
    let mut seen = Vec::new();
    let steps =
        p.jj.merge_agents(&integration, &agents, &mut |step| {
            seen.push(step.agent.clone())
        })
        .unwrap();
    let names: Vec<_> = steps.iter().map(|s| s.agent.as_str()).collect();
    assert_eq!(names, ["a", "b", "d"]);
    assert_eq!(seen, names, "each step is told as it is done");
    assert!(steps[0].outcome.ok && steps[1].outcome.ok);
    assert_eq!(steps[2].outcome.conflict.as_ref().unwrap().files, ["a.txt"]);
    assert_eq!(read(&integration.path, "a.txt"), "hello\nA\n");
    assert!(integration.path.join("bnew.txt").exists());
    let head = p.head(&integration);
    assert_eq!(p.git(&["rev-parse", "refs/heads/agents/integration"]), head);
    assert_eq!(
        p.git(&["log", "-1", "--format=%an", "agents/integration"]),
        "crew"
    );
    // A second run only retries what is still missing.
    let again =
        p.jj.merge_agents(&integration, &agents, &mut |_| {})
            .unwrap();
    let names: Vec<_> = again.iter().map(|s| s.agent.as_str()).collect();
    assert_eq!(names, ["d"]);
    assert_eq!(p.head(&integration), head);
}

#[test]
fn a_stale_workspace_is_an_error_that_says_how_to_update_it() {
    let Some(p) = project() else { return };
    let s1 = p.workspace("s1");
    let s2 = p.workspace("s2");
    write(&s1.path, "s1.txt", "s1\n");
    p.jj.commit_all(&s1.path, "s1", "s1 work").unwrap();
    // Another workspace rewrites the commit s1 sits on.
    write(&s2.path, "inj.txt", "inj\n");
    p.jj_by_hand(
        &s2.path,
        "s2",
        &["squash", "--from", "@", "--into", "\"agents/s1\"", "-u"],
    );
    let err = p.jj.status(&s1.path).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("stale"), "{text}");
    assert!(text.contains("jj workspace update-stale"), "{text}");
    // It is not repaired behind the agent's back.
    assert!(!s1.path.join("inj.txt").exists());
}

#[test]
fn every_command_goes_through_the_installed_launcher() {
    let Some(p) = project() else { return };
    let a = p.workspace("a");
    struct Recorder(std::sync::Mutex<Vec<Vec<String>>>);
    impl launch::Launcher for Recorder {
        fn launch(&self, cmd: &Command) -> std::io::Result<Command> {
            self.0.lock().unwrap().push(
                std::iter::once(cmd.get_program())
                    .chain(cmd.get_args())
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
            );
            let mut same = Command::new(cmd.get_program());
            same.args(cmd.get_args());
            if let Some(dir) = cmd.get_current_dir() {
                same.current_dir(dir);
            }
            for (k, v) in cmd.get_envs() {
                match v {
                    Some(v) => same.env(k, v),
                    None => same.env_remove(k),
                };
            }
            Ok(same)
        }
    }
    let recorder = Arc::new(Recorder(Default::default()));
    write(&a.path, "work.txt", "x\n");
    let commit = launch::with_launcher(recorder.clone(), || {
        p.jj.commit_all(&a.path, "a", "work").unwrap()
    });
    assert_eq!(commit.summary, "work");
    let seen = recorder.0.lock().unwrap().clone();
    let program = p.jj.program().to_string_lossy().into_owned();
    assert!(seen.len() >= 4, "{seen:?}");
    assert!(seen.iter().all(|argv| argv[0] == program), "{seen:?}");
    let inside = a.path.to_string_lossy().into_owned();
    assert!(
        seen.iter()
            .all(|argv| argv.windows(2).any(|w| w[0] == "-R" && w[1] == inside)),
        "a command outside the workspace: {seen:?}"
    );
    assert!(seen.iter().any(|argv| argv.contains(&"commit".to_string())));
    for pin in ["signing.behavior=drop", "snapshot.auto-update-stale=false"] {
        assert!(seen[0].iter().any(|a| a == pin), "{pin}: {:?}", seen[0]);
    }
    // A launcher that refuses stops the operation: no fallback.
    struct Refuse;
    impl launch::Launcher for Refuse {
        fn launch(&self, _: &Command) -> std::io::Result<Command> {
            Err(std::io::Error::other("no sandbox"))
        }
    }
    write(&a.path, "third.txt", "z\n");
    assert!(
        launch::with_launcher(Arc::new(Refuse), || p.jj.commit_all(&a.path, "a", "third")).is_err()
    );
    assert_eq!(p.branch("a"), commit.sha);
}

#[test]
fn a_project_that_is_a_linked_git_worktree_keeps_both_checkouts_unchanged() {
    let Some((program, version)) = real_jj() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let main = root.join("main");
    git_project(&main);
    let linked = root.join("linked");
    sh_git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            &linked.to_string_lossy(),
            "-b",
            "feature",
        ],
    );
    write(&linked, "feat.txt", "feat\n");
    sh_git(&linked, &["add", "feat.txt"]);
    sh_git(&linked, &["commit", "-q", "-m", "feature work"]);
    let feature = sh_git(&linked, &["rev-parse", "feature"]);
    let main_head = sh_git(&main, &["rev-parse", "HEAD"]);
    let jj = Jj::new(&program, &version, &linked, &linked.join(".crew"));
    assert!(jj.prepare(&linked).unwrap());
    let Store::Jj { git_dir, .. } = jj.store() else {
        panic!("not a jj store")
    };
    assert_eq!(git_dir, main.join(".git"), "the common directory");
    let (w, _) = jj
        .add_workspace(&linked.join(".crew/worktrees"), "a", "HEAD")
        .unwrap();
    jj.populate(&w).unwrap();
    assert_eq!(read(&w.path, "feat.txt"), "feat\n", "from the linked HEAD");
    write(&w.path, "w.txt", "w\n");
    let commit = jj.commit_all(&w.path, "a", "a in linked").unwrap();
    assert_eq!(
        sh_git(&main, &["rev-parse", "refs/heads/agents/a"]),
        commit.sha
    );
    assert_eq!(sh_git(&main, &["rev-parse", "agents/a~1"]), feature);
    assert_eq!(sh_git(&linked, &["status", "--porcelain"]), "");
    assert_eq!(sh_git(&linked, &["rev-parse", "HEAD"]), feature);
    assert_eq!(sh_git(&main, &["status", "--porcelain"]), "");
    assert_eq!(sh_git(&main, &["rev-parse", "HEAD"]), main_head);
    assert_eq!(read(&main.join(".git"), "HEAD"), "ref: refs/heads/main\n");
}

#[test]
fn a_branch_from_the_git_backend_continues_in_a_jj_workspace() {
    let Some((program, version)) = real_jj() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap().join("proj");
    git_project(&ws);
    // The git backend's worktree, committed in and removed by hand.
    let worktrees = ws.join(".crew/worktrees");
    let old = git::ensure_worktree(&ws, &worktrees, "a", "HEAD").unwrap();
    write(&old.path, "wa.txt", "a's work\n");
    let g1 = git::commit_all(&old.path, "a", "a: committed under git").unwrap();
    sh_git(&ws, &["worktree", "remove", &old.path.to_string_lossy()]);
    let jj = Jj::new(&program, &version, &ws, &ws.join(".crew"));
    assert!(jj.prepare(&ws).unwrap());
    let (w, _) = jj.add_workspace(&worktrees, "a", "HEAD").unwrap();
    jj.populate(&w).unwrap();
    assert_eq!(read(&w.path, "wa.txt"), "a's work\n");
    write(&w.path, "more.txt", "more\n");
    jj.commit_all(&w.path, "a", "a: continued under jj")
        .unwrap();
    assert_eq!(sh_git(&ws, &["rev-parse", "agents/a~1"]), g1.sha);
}

#[test]
fn back_to_git_publishes_what_a_bare_jj_commit_leaves_behind() {
    let Some(p) = project() else { return };
    let a = p.workspace("a");
    write(&a.path, "kept.txt", "kept\n");
    let j1 = p.jj.commit_all(&a.path, "a", "a: committed").unwrap().sha;
    write(&a.path, "dirty.txt", "dirty\n");
    // The steps of D2, by hand: commit, move the bookmark, export.
    p.jj_by_hand(&a.path, "u", &["commit", "--message=a: saved by hand"]);
    let saved = p.head(&a);
    assert_ne!(saved, j1);
    // The hazard: a bare commit moves neither the bookmark nor the branch.
    assert_eq!(p.bookmark("a").as_deref(), Some(j1.as_str()));
    assert_eq!(p.branch("a"), j1);
    p.jj_by_hand(&a.path, "u", &["bookmark", "set", "agents/a", "-r", "@-"]);
    p.jj_by_hand(&a.path, "u", &["git", "export"]);
    assert_eq!(p.branch("a"), saved);
    assert!(p.clean(&a));
    // Only now jj's state goes; a git worktree on the branch has the files.
    std::fs::remove_dir_all(p.ws.join(".crew/jj")).unwrap();
    std::fs::remove_dir_all(&a.path).unwrap();
    let back = p.dir.path().canonicalize().unwrap().join("back");
    p.git(&["worktree", "add", "-q", &back.to_string_lossy(), "agents/a"]);
    assert_eq!(read(&back, "dirty.txt"), "dirty\n");
    assert_eq!(read(&back, "kept.txt"), "kept\n");
    assert_eq!(sh_git(&back, &["status", "--porcelain"]), "");
}

#[test]
fn the_native_layout_uses_the_projects_own_jj_repository() {
    let Some((program, version)) = real_jj() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap().join("native");
    git_project(&ws);
    let colocate = Command::new(&program)
        .args(["--quiet", "git", "init", "--colocate"])
        .current_dir(&ws)
        .env("JJ_CONFIG", "/dev/null")
        .env("JJ_USER", "u")
        .env("JJ_EMAIL", "u@example.com")
        .status()
        .unwrap();
    assert!(colocate.success());
    write(&ws, "dirty.txt", "the user's own work\n");
    let base = sh_git(&ws, &["rev-parse", "main"]);
    let jj = Jj::new(&program, &version, &ws, &ws.join(".crew"));
    assert!(jj.is_native());
    assert!(jj.prepare(&ws).unwrap());
    let Store::Jj { git_dir, repo_dir } = jj.store() else {
        panic!("not a jj store")
    };
    assert_eq!(git_dir, ws.join(".git"));
    assert_eq!(repo_dir, ws.join(".jj/repo"));
    let (w, _) = jj
        .add_workspace(&ws.join(".crew/worktrees"), "a", "HEAD")
        .unwrap();
    jj.populate(&w).unwrap();
    assert_eq!(sh_git(&ws, &["rev-parse", "refs/heads/agents/a"]), base);
    write(&w.path, "n.txt", "n\n");
    let commit = jj.commit_all(&w.path, "a", "native work").unwrap();
    assert_eq!(
        sh_git(&ws, &["rev-parse", "refs/heads/agents/a"]),
        commit.sha
    );
    // The user's jj does not see crew's state.
    let status = Command::new(&program)
        .args(["--no-pager", "--color", "never", "status"])
        .current_dir(&ws)
        .env("JJ_CONFIG", "/dev/null")
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(!status.contains(".crew"), "{status}");
    assert!(read(&ws, "dirty.txt").contains("own work"));
}

/// A jj in front of the real one that, once, hangs on the first command
/// with `word` among its arguments: before running it (`before`), or after
/// it ran (`after`). jj has no hooks to hang in; this cuts an operation at a
/// chosen phase.
struct Hang {
    _dir: tempfile::TempDir,
    script: PathBuf,
    plan: PathBuf,
}

impl Hang {
    fn new(real: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().canonicalize().unwrap();
        let script = dir.join("jj-hang");
        let plan = dir.join("hang-plan");
        let marker = dir.join("hang-child-alive");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
plan="{plan}"
if [ -f "$plan" ]; then
  read -r when word < "$plan"
  for a in "$@"; do
    if [ "$a" = "$word" ]; then
      rm -f "$plan"
      if [ "$when" = after ]; then "{real}" "$@"; fi
      (sleep 7; touch "{marker}") &
      exec sleep 60
    fi
  done
fi
exec "{real}" "$@"
"#,
                plan = plan.display(),
                real = real.display(),
                marker = marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        runnable(&script);
        Self {
            _dir: temp,
            script,
            plan,
        }
    }

    fn arm(&self, when: &str, word: &str) {
        std::fs::write(&self.plan, format!("{when} {word}\n")).unwrap();
    }
}

/// A project whose jj hangs when told to.
fn hanging_project() -> Option<(Project, Hang)> {
    let (real, _) = real_jj()?;
    let hang = Hang::new(&real);
    let p = project_with(Some(&hang.script))?;
    Some((p, hang))
}

#[test]
fn a_timed_out_jj_is_killed_with_its_children_and_a_commit_cut_short_is_not_made() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let a = p.workspace("a");
    write(&a.path, "work.txt", "x\n");
    let before = p.head(&a);
    hang.arm("before", "commit");
    let started = std::time::Instant::now();
    let err = p.jj.commit_all(&a.path, "a", "work").unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("did not finish"), "{text}");
    assert!(text.contains("nothing was committed"), "{text}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(p.head(&a), before);
    assert!(!p.clean(&a), "the changes are still in the working copy");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let marker = hang.script.parent().unwrap().join("hang-child-alive");
    assert!(!marker.exists(), "the hung jj's child outlived the timeout");
    // Retried, it goes through.
    let commit = p.jj.commit_all(&a.path, "a", "work").unwrap();
    assert_eq!(p.branch("a"), commit.sha);
}

#[test]
fn a_commit_cut_in_its_export_is_reported_as_made_and_published() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let a = p.workspace("a");
    write(&a.path, "work.txt", "x\n");
    hang.arm("before", "export");
    let err = p.jj.commit_all(&a.path, "a", "work").unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("after the commit was made"), "{text}");
    let head = p.head(&a);
    assert!(text.contains(&head[..8]), "{text}");
    assert_eq!(p.bookmark("a").as_deref(), Some(head.as_str()));
    assert_eq!(p.branch("a"), head);
}

/// `t1` and `t2` from the same commit; `t2` one commit ahead.
fn ahead(p: &Project) -> (Workspace, Workspace, CommitInfo) {
    let t1 = p.workspace("t1");
    let t2 = p.workspace("t2");
    write(&t2.path, "t2.txt", "t2\n");
    let c = p.jj.commit_all(&t2.path, "t2", "t2 work").unwrap();
    (t1, t2, c)
}

#[test]
fn a_merge_cut_before_it_landed_changes_nothing() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let (t1, _, _) = ahead(&p);
    let before = p.head(&t1);
    hang.arm("before", "new");
    let err = p.jj.merge(&t1.path, "t1", "agents/t2").unwrap_err();
    assert!(
        format!("{err:#}").contains("the worktree was restored"),
        "{err:#}"
    );
    assert_eq!(p.head(&t1), before);
    assert_eq!(p.branch("t1"), before);
    assert!(p.clean(&t1));
    assert!(p.jj.merge(&t1.path, "t1", "agents/t2").unwrap().ok);
}

#[test]
fn a_clean_merge_cut_in_its_publish_phase_is_kept_and_published() {
    for (when, word) in [("before", "set"), ("before", "export"), ("after", "new")] {
        let Some((p, hang)) = hanging_project() else {
            return;
        };
        let (t1, _, _) = ahead(&p);
        let before = p.head(&t1);
        hang.arm(when, word);
        let err = p.jj.merge(&t1.path, "t1", "agents/t2").unwrap_err();
        let text = format!("{err:#}");
        let head = p.head(&t1);
        assert_ne!(head, before, "{when} {word}: the merge landed");
        assert!(
            text.contains("the result was kept"),
            "{when} {word}: {text}"
        );
        assert!(text.contains(&head[..8]), "{text}");
        assert_eq!(p.bookmark("t1").as_deref(), Some(head.as_str()));
        assert_eq!(p.branch("t1"), head);
        assert!(t1.path.join("t2.txt").exists());
        assert!(p.clean(&t1));
    }
}

#[test]
fn a_conflicted_merge_cut_short_is_rolled_back() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let (a, b, _, _, _) = rivals(&p);
    let _ = b;
    let before = p.head(&a);
    hang.arm("after", "new");
    let err = p.jj.merge(&a.path, "a", "agents/d").unwrap_err();
    assert!(
        format!("{err:#}").contains("the worktree was restored"),
        "{err:#}"
    );
    assert_eq!(p.head(&a), before);
    assert_eq!(read(&a.path, "a.txt"), "hello\nA\n");
    assert!(p.clean(&a));
    assert_eq!(p.bookmark("a").as_deref(), Some(before.as_str()));
    assert_eq!(p.branch("a"), before);
    // Cut in the middle of the rollback, between rebase and abandon.
    hang.arm("after", "rebase");
    let err = p.jj.merge(&a.path, "a", "agents/d").unwrap_err();
    assert!(
        format!("{err:#}").contains("the worktree was restored"),
        "{err:#}"
    );
    assert_eq!(p.head(&a), before);
    assert!(p.clean(&a));
    assert_eq!(p.branch("a"), before);
}

#[test]
fn a_cherry_pick_cut_short_is_rolled_back_or_kept_by_what_landed() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let (a, _, _, b1, d1) = rivals(&p);
    let before = p.head(&a);
    hang.arm("after", "duplicate");
    let err = p.jj.cherry_pick(&a.path, "a", &d1.sha).unwrap_err();
    assert!(
        format!("{err:#}").contains("the worktree was restored"),
        "{err:#}"
    );
    assert_eq!(p.head(&a), before);
    assert_eq!(p.bookmark("a").as_deref(), Some(before.as_str()));
    assert!(p.clean(&a));
    hang.arm("after", "duplicate");
    let err = p.jj.cherry_pick(&a.path, "a", &b1.sha).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("the result was kept"), "{text}");
    let head = p.head(&a);
    assert_ne!(head, before);
    assert_eq!(p.branch("a"), head);
    assert!(a.path.join("bnew.txt").exists());
}

#[test]
fn a_conflicted_result_that_already_carries_the_bookmark_is_not_abandoned() {
    let Some(p) = project() else { return };
    let (a, _, _, _, d1) = rivals(&p);
    let parent = p.head(&a);
    // A conflicted copy, with the agent's bookmark moved onto it by hand.
    let copy = format!("commit_id({})", d1.sha);
    p.jj_by_hand(&a.path, "a", &["duplicate", &copy, "--insert-before", "@"]);
    p.jj_by_hand(&a.path, "a", &["bookmark", "set", "agents/a", "-r", "@-"]);
    let made = p.head(&a);
    let timeout = anyhow::Error::from(io::Error::new(io::ErrorKind::TimedOut, "cut"));
    let err = p.jj.recover(
        &a.path,
        "a",
        "agents/a",
        Operation::CherryPick,
        &parent,
        &timeout,
    );
    assert!(err.to_string().contains("left as it is"), "{err:#}");
    assert_eq!(p.head(&a), made);
    assert_eq!(p.bookmark("a").as_deref(), Some(made.as_str()));
}

#[test]
fn an_integration_cut_in_its_export_is_kept_and_published() {
    let Some((p, hang)) = hanging_project() else {
        return;
    };
    let (_, _, _, _, _) = rivals(&p);
    let (integration, _) =
        p.jj.add_workspace(&p.worktrees, INTEGRATION_DIR, "HEAD")
            .unwrap();
    p.jj.populate(&integration).unwrap();
    let before = p.git(&["rev-parse", "refs/heads/agents/integration"]);
    // An export killed earlier left git's lock behind: a plain export would
    // skip the ref with a warning and exit 0.
    let lock = p.ws.join(".git/refs/heads/agents/integration.lock");
    std::fs::write(&lock, "").unwrap();
    hang.arm("before", "export");
    let agents: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
    let err =
        p.jj.merge_agents(&integration, &agents, &mut |_| {})
            .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("the result was kept"), "{text}");
    let head = p.head(&integration);
    assert_ne!(head, before);
    assert!(text.contains(&head[..8]), "{text}");
    let kept = err.downcast_ref::<git::Kept>().expect("the kept commit");
    assert_eq!(kept.commit, head);
    assert!(!lock.exists(), "the stale lock is still there");
    assert_eq!(p.git(&["rev-parse", "refs/heads/agents/integration"]), head);
    assert!(integration.path.join("bnew.txt").exists());
}

#[test]
fn an_export_git_refuses_is_said_by_every_publisher() {
    let Some(p) = project() else { return };
    let (t1, t2, _) = ahead(&p);
    crate::vcs::take_warnings();
    let lock = |id: &str| p.ws.join(format!(".git/refs/heads/agents/{id}.lock"));
    let said = |bookmark: &str| {
        let warnings = crate::vcs::take_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with(&format!("{bookmark} is not exported to git"))),
            "{bookmark}: {warnings:?}"
        );
    };
    // Merge.
    let before = p.branch("t1");
    std::fs::write(lock("t1"), "").unwrap();
    let merged = p.jj.merge(&t1.path, "t1", "agents/t2").unwrap();
    assert!(merged.ok);
    assert_eq!(p.branch("t1"), before, "git took the ref after all");
    said("agents/t1");
    // Cherry-pick, and commit.
    write(&t2.path, "more.txt", "more\n");
    let c = p.jj.commit_all(&t2.path, "t2", "more").unwrap();
    assert!(crate::vcs::take_warnings().is_empty());
    assert!(p.jj.cherry_pick(&t1.path, "t1", &c.sha).unwrap().outcome.ok);
    said("agents/t1");
    write(&t1.path, "own.txt", "own\n");
    p.jj.commit_all(&t1.path, "t1", "own").unwrap();
    said("agents/t1");
    // Integration.
    let (integration, _) =
        p.jj.add_workspace(&p.worktrees, INTEGRATION_DIR, "HEAD")
            .unwrap();
    p.jj.populate(&integration).unwrap();
    crate::vcs::take_warnings();
    std::fs::write(lock("integration"), "").unwrap();
    let steps =
        p.jj.merge_agents(&integration, &["t2".to_string()], &mut |_| {})
            .unwrap();
    assert_eq!(steps.len(), 1);
    said("agents/integration");
    // With the locks gone the next export catches up, and says nothing.
    std::fs::remove_file(lock("t1")).unwrap();
    write(&t1.path, "last.txt", "last\n");
    let last = p.jj.commit_all(&t1.path, "t1", "last").unwrap();
    assert_eq!(p.branch("t1"), last.sha);
    assert!(crate::vcs::take_warnings().is_empty());
}

/// Wait until the script at `path`, just written, can be run: a process
/// another test thread forked while the file was open for writing keeps it
/// busy (ETXTBSY) until that process runs its own program.
pub(crate) fn runnable(path: &Path) {
    for _ in 0..500 {
        match Command::new(path).arg("--version").output() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => return,
        }
    }
    panic!("{} stayed busy", path.display());
}
