//! A team runs the crew binary its daemon runs (notes/binary-pinning.md):
//! the daemon keeps a copy of its own binary in `<state>/bin` and hands
//! out that copy, so what becomes of the file it was started from (a
//! rebuild replacing it, a clean removing it) never reaches its agents.

mod common;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use common::{agent, env_of, find_agent, runs, task, Session};
use crew_core::EventLevel;

/// An agent that says `up` to the operator each time it starts, through
/// its crew tools.
fn says_up(id: &str, more: &[&str]) -> crew_core::config::AgentSpec {
    let mut steps = vec!["--on-start=send operator status up"];
    steps.extend_from_slice(more);
    agent(id, "", &steps)
}

/// How many times agent `id` said it is up.
fn ups(s: &Session, id: &str) -> usize {
    s.messages()
        .iter()
        .filter(|m| m.from == id && m.body == "up")
        .count()
}

/// Restart agent `id` (running as `old`) and wait until it says it is up
/// for the `n`th time. Its new pid.
fn restarted(s: &Session, id: &str, old: u32, n: usize) -> u32 {
    s.agent_cmd(&["restart", id]);
    s.wait_for(&format!("{id} to be up again"), || ups(s, id) == n);
    let snap = s.wait(&format!("{id} to run again"), |snap| {
        find_agent(snap, id).pid.is_some_and(|p| p != old)
    });
    find_agent(&snap, id).pid.unwrap()
}

/// Replace `path` the way a build does: another file renamed over it.
fn replace(path: &Path) {
    let new = path.with_extension("new");
    std::fs::write(&new, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&new, path).unwrap();
}

fn kill_daemon(s: &Session) {
    let pid = s.snapshot().session.daemon_pid;
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    s.wait_for("the daemon to die", || !crew_core::paths::pid_alive(pid));
}

fn start_from(s: &Session, crew: &Path) {
    let out = s
        .command_of(crew)
        .args(["start", "-C", &s.workspace.to_string_lossy()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn events_of(s: &Session, event_type: &str) -> Vec<crew_core::Event> {
    s.events()
        .into_iter()
        .filter(|e| e.event_type == event_type)
        .collect()
}

#[test]
fn an_agent_restarted_after_the_daemons_file_is_replaced_or_removed_runs_the_daemons_copy() {
    let s = Session::new();
    let crew = s.own_crew("crew");
    let team = s.team("pinned", vec![says_up("a", &[])], vec![]);
    s.run_from(&crew, &team);
    s.wait_for("a to be up", || ups(&s, "a") == 1);
    let copy = s.crew_copy();
    let name = copy.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("crew-") && name.len() == 21, "{name}");
    assert_eq!(
        std::fs::metadata(&copy).unwrap().len(),
        std::fs::metadata(&crew).unwrap().len()
    );
    assert_eq!(std::fs::metadata(&copy).unwrap().mode() & 0o7777, 0o500);
    let pid = find_agent(&s.snapshot(), "a").pid.unwrap();
    assert!(runs(pid, &copy), "a does not run the daemon's copy");
    assert_eq!(env_of(pid, "CREW_BIN"), Some(copy.display().to_string()));

    // The file the daemon was started from is replaced, as a build does.
    replace(&crew);
    let pid = restarted(&s, "a", pid, 2);
    assert!(runs(pid, &copy), "after a rebuild, a does not run the copy");

    // ...and removed.
    std::fs::remove_file(&crew).unwrap();
    let pid = restarted(&s, "a", pid, 3);
    assert!(runs(pid, &copy), "after a clean, a does not run the copy");
    assert_eq!(env_of(pid, "CREW_BIN"), Some(copy.display().to_string()));
    assert_eq!(s.crew_copies(), vec![copy]);
    assert!(events_of(&s, "daemon.repinned").is_empty());
}

#[test]
fn the_same_build_started_again_makes_no_copy_and_another_builds_copy_goes_after_recovery() {
    let s = Session::new();
    let crew = s.own_crew("crew");
    let team = s.team(
        "pinned",
        vec![says_up("a", &["--on-task=wait-message operator"])],
        vec![task("t", "a", &[])],
    );
    s.run_from(&crew, &team);
    s.wait_for("a to be up", || ups(&s, "a") == 1);
    let first = s.crew_copy();
    let made = std::fs::metadata(&first).unwrap();

    // The same file started again: the same copy, not made again.
    kill_daemon(&s);
    start_from(&s, &crew);
    s.wait_for("a to be up after the restart", || ups(&s, "a") == 2);
    assert_eq!(s.crew_copies(), vec![first.clone()]);
    let found = std::fs::metadata(&first).unwrap();
    assert_eq!(
        (found.ino(), found.mtime(), found.mtime_nsec()),
        (made.ino(), made.mtime(), made.mtime_nsec()),
        "the copy was made again"
    );
    let pid = find_agent(&s.snapshot(), "a").pid.unwrap();
    assert!(runs(pid, &first));

    // Another build (another file, as a rebuild makes): its own copy, and
    // once recovery has restarted the agents the first copy is gone.
    kill_daemon(&s);
    let other = s.own_crew("crew-2");
    start_from(&s, &other);
    s.wait_for("a to be up on the other build", || ups(&s, "a") == 3);
    s.wait_for("the first build's copy to go", || {
        s.crew_copies().len() == 1
    });
    let second = s.crew_copy();
    assert_ne!(second, first);
    let pid = find_agent(&s.snapshot(), "a").pid.unwrap();
    assert!(runs(pid, &second), "a does not run the new build's copy");
}

#[test]
fn a_copy_removed_under_a_running_daemon_is_back_within_a_tick_and_a_launch_gets_it() {
    let s = Session::new();
    let team = s.team("pinned", vec![says_up("a", &[])], vec![]);
    s.run(&team);
    s.wait_for("a to be up", || ups(&s, "a") == 1);
    let copy = s.crew_copy();
    let pid = find_agent(&s.snapshot(), "a").pid.unwrap();

    std::fs::remove_file(&copy).unwrap();
    let removed = Instant::now();
    s.wait_for("the copy to be made again", || copy.exists());
    assert!(
        removed.elapsed() < Duration::from_secs(12),
        "the copy took {:?} to come back",
        removed.elapsed()
    );
    assert_eq!(s.crew_copies(), vec![copy.clone()]);
    s.wait_for("the warning", || {
        !events_of(&s, "daemon.repinned").is_empty()
    });
    let repinned = events_of(&s, "daemon.repinned");
    assert_eq!(repinned.len(), 1, "{repinned:?}");
    assert_eq!(repinned[0].level, EventLevel::Warn);
    assert!(
        repinned[0].message.contains("was gone"),
        "{}",
        repinned[0].message
    );

    // Gone again: an agent launched before the next tick gets it all the same.
    std::fs::remove_file(&copy).unwrap();
    let pid = restarted(&s, "a", pid, 2);
    assert!(runs(pid, &copy));
    assert_eq!(events_of(&s, "daemon.repinned").len(), 2);
}

#[test]
fn a_daemon_that_cannot_keep_a_copy_runs_its_team_and_says_so() {
    let s = Session::new();
    // `<state>/bin` cannot be made: a file is in its way.
    std::fs::create_dir_all(s.workspace.join(".crew")).unwrap();
    std::fs::write(s.workspace.join(".crew/bin"), "").unwrap();
    let team = s.team(
        "unpinned",
        vec![agent("a", "", &["--on-task=done finished"])],
        vec![task("t", "a", &[])],
    );
    s.run(&team);
    s.wait_all_done();
    let unpinned = events_of(&s, "daemon.unpinned");
    assert_eq!(unpinned.len(), 1, "{unpinned:?}");
    assert_eq!(unpinned[0].level, EventLevel::Warn);
    let said = &unpinned[0].message;
    assert!(
        said.starts_with("could not keep a copy of the crew binary (")
            && said.ends_with("while this team runs will break its agents' tools"),
        "{said}"
    );
    let status = s.ok(&["status", "-C", &s.workspace.to_string_lossy()]);
    assert!(status.contains(&format!("warning    {said}")), "{status}");
}
