//! Optional end-to-end test against the real Claude Code CLI.
//!
//! Ignored by default because it needs `claude` on PATH, a logged-in account
//! and a few cents of tokens:
//!
//!     cargo test -p crew --test real_cli -- --ignored --nocapture
//!
//! Two real Claude Code sessions: `alpha` must get an answer from `beta` by
//! messaging it directly, then commit the result and complete its task.

mod common;

use std::time::{Duration, Instant};

use common::*;
use crew_core::config::AgentSpec;
use crew_core::{MessageType, RuntimeKind, TaskStatus};

fn claude(id: &str, role: &str) -> AgentSpec {
    AgentSpec {
        id: id.to_string(),
        role: role.to_string(),
        runtime: Some(RuntimeKind::Claude),
        model: Some("haiku".to_string()),
        ..AgentSpec::default()
    }
}

#[test]
#[ignore = "needs the Claude Code CLI and spends tokens"]
fn two_real_claude_code_agents_talk_directly() {
    let s = Session::new();
    let mut team = s.team(
        "real",
        vec![claude("alpha", "asker"), claude("beta", "answerer")],
        vec![task("ask", "alpha", &[])],
    );
    team.defaults.auto_trust = true;
    team.tasks[0].description = "Ask beta directly (agent_send with type \"question\") what \
17 * 3 is. When beta's answer arrives, write just the number to answer.txt, git_commit, then \
task_complete."
        .to_string();
    s.run(&team);

    let deadline = Instant::now() + Duration::from_secs(240);
    let snapshot = loop {
        let snapshot = s.snapshot();
        if find_task(&snapshot, "ask").status == TaskStatus::Done {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "the agents did not finish:\n{}",
            s.ok(&["logs", "-n", "80", "-C", &s.workspace.to_string_lossy()])
        );
        std::thread::sleep(Duration::from_secs(2));
    };

    let messages = s.messages();
    let question = messages
        .iter()
        .find(|m| m.from == "alpha" && m.to == "beta")
        .expect("alpha's question to beta");
    assert!(question.is_p2p());
    let answer = messages
        .iter()
        .find(|m| m.from == "beta" && m.to == "alpha")
        .expect("beta's answer to alpha");
    assert!(answer.body.contains("51"), "{}", answer.body);
    assert!(messages.iter().all(|m| m.kind == MessageType::Task || m.is_p2p()));

    let alpha = find_agent(&snapshot, "alpha");
    let answer_file = std::path::Path::new(&alpha.workdir).join("answer.txt");
    assert!(std::fs::read_to_string(answer_file).unwrap().contains("51"));
    assert!(find_task(&snapshot, "ask")
        .artifacts
        .iter()
        .any(|a| a.kind == "commit"));
    // The sessions reported their lifecycle through hooks, and messages were
    // pushed over the socket rather than typed.
    let events = s.events();
    assert!(events.iter().any(|e| e.event_type == "delivery.socket"));
    assert!(events
        .iter()
        .any(|e| e.event_type == "message.delivered" && e.message.contains("via socket")));
}
