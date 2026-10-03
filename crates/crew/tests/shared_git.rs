//! A shared checkout cannot attribute a commit to one agent, so the git tools
//! that move its HEAD are refused.

mod common;

use common::*;
use crew_core::config::Isolation;
use crew_core::AgentStatus;
use serde_json::json;

#[test]
fn commit_merge_and_cherry_pick_are_refused_in_a_shared_checkout() {
    let s = Session::new();
    let mut team = s.team(
        "shared",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    team.workspace.isolation = Isolation::Shared;
    s.run(&team);
    let done = s.wait("agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });
    assert_eq!(done.session.isolation, "shared");
    std::fs::write(s.workspace.join("work.txt"), "x\n").unwrap();

    for (tool, args) in [
        ("git_commit", json!({ "message": "work" })),
        ("git_cherry_pick", json!({ "commit": "abcdef1" })),
        ("git_merge", json!({ "from_agent": "b" })),
    ] {
        let out = s.ctl("a", &["tool", tool, &args.to_string()]);
        assert!(!out.status.success(), "{tool} should be refused");
        let err = stderr(&out);
        assert!(
            err.contains("checkout is shared") && err.contains("orchestrator"),
            "{tool}: {err}"
        );
    }
    // Nothing was committed: the file is still untracked.
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&s.workspace)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).contains("work.txt"));
}
