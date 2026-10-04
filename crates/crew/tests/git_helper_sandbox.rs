//! Git's helper work (filling a new worktree in) run through the sandbox, with
//! real bubblewrap. Skipped with a printed reason when the host lacks it;
//! `CREW_TEST_SANDBOX=require` makes that a failure.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use crew::git::{self, launch};
use crew::sandbox::probe;
use crew::sandbox::profile::{Extra, Host, Session};
use crew::vcs::Store;

const CREW: &str = env!("CARGO_BIN_EXE_crew");

fn plain_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn a_filter_from_the_repository_runs_confined_when_a_worktree_is_populated() {
    let path = std::env::var_os("PATH");
    let Ok(available) = probe::probe(Path::new(CREW), path.as_ref()) else {
        if std::env::var("CREW_TEST_SANDBOX").as_deref() == Ok("require") {
            panic!("CREW_TEST_SANDBOX=require but the host cannot run sandboxes");
        }
        eprintln!("skipped: the host cannot run sandboxes");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let ws = base.join("ws");
    git::init_repo(&ws).unwrap();
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("filter-ran");
    // A smudge filter the repository's configuration defines: it runs when files
    // are checked out, which is when a worktree is populated.
    plain_git(&ws, &["config", "filter.probe.clean", "cat"]);
    plain_git(
        &ws,
        &[
            "config",
            "filter.probe.smudge",
            &format!("sh -c 'touch {} 2>/dev/null; cat'", marker.display()),
        ],
    );
    std::fs::write(ws.join(".gitattributes"), "tracked.txt filter=probe\n").unwrap();
    std::fs::write(ws.join("tracked.txt"), "content\n").unwrap();
    git::commit_all(&ws, "setup", "tracked files").unwrap();
    let worktrees = ws.join(".crew/worktrees");

    // Positive control: populated the plain way the filter does run on the host.
    git::ensure_worktree(&ws, &worktrees, "control", "HEAD").unwrap();
    assert!(marker.exists(), "the control did not run the filter");
    std::fs::remove_file(&marker).unwrap();

    // Populated in the helper sandbox it cannot touch the host path.
    let (worktree, needs_population) = git::add_worktree(&ws, &worktrees, "a", "HEAD").unwrap();
    assert!(needs_population);
    let run = base.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let socket = run.join("daemon.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let cargo = base.join("cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    let git_dir = ws.join(".git");
    let wt = worktree.path.canonicalize().unwrap();
    let admin = git_dir.join("worktrees/a");
    let host = Host::detect(Path::new(CREW), &available.bwrap, available.landlock_abi);
    let store = Store::Git {
        git_dir: git_dir.clone(),
    };
    let session = Session {
        workspace: &ws,
        store: &store,
        runtime_dir: &run,
        daemon_socket: &socket,
        cargo_home: &cargo,
    };
    let (launcher, _warnings) =
        launch::helper_launcher(&host, session, &Extra::default(), &wt, Some(&admin), None)
            .unwrap_or_else(|e| panic!("helper profile: {e}"));
    launch::with_launcher(Arc::new(launcher), || {
        git::populate_worktree(&worktree).unwrap_or_else(|e| panic!("populate: {e:#}"));
    });
    assert_eq!(
        std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
        "content\n"
    );
    assert!(!marker.exists(), "the filter reached the host");
}
