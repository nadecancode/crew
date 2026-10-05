//! Real daemon/process regressions for parked Waiting cards and task identity.

mod common;

use common::*;
use crew_core::{AgentStatus, MessageType, TaskStatus};
use serde_json::{json, Value};

fn tool(s: &Session, name: &str, args: Value) -> String {
    let out = s.ctl("owner", &["tool", name, &args.to_string()]);
    assert!(
        out.status.success(),
        "{name}: {}{}",
        stdout(&out),
        stderr(&out)
    );
    stdout(&out)
}

fn working(s: &Session, id: &str) {
    s.wait(id, |snap| {
        find_task(snap, id).status == TaskStatus::Working
            && s.identity_path("owner").exists()
            && find_agent(snap, "owner").status != AgentStatus::Spawning
    });
}

fn park(s: &Session) {
    tool(
        s,
        "agent_status",
        json!({"status":"waiting", "note":"on answerer"}),
    );
}

#[test]
fn waiting_owner_runs_ready_work_keeps_mid_card_answer_and_resumes_once() {
    let s = Session::new();
    s.run(&s.team(
        "parked-answer",
        vec![
            agent("owner", "architect", &[]),
            agent(
                "answerer",
                "schema",
                &["--on-message=sleep 1000", "--on-message=reply three tables"],
            ),
        ],
        vec![
            task("design", "owner", &[]),
            task("independent", "owner", &[]),
        ],
    ));
    working(&s, "design");
    tool(
        &s,
        "agent_send",
        json!({"to":"answerer", "type":"question", "body":"schema?"}),
    );
    park(&s);
    working(&s, "independent");
    s.wait_for("mid-card answer", || {
        s.messages().iter().any(|m| m.kind == MessageType::Answer)
    });
    let answer = s
        .messages()
        .into_iter()
        .find(|m| m.kind == MessageType::Answer)
        .unwrap();
    let snap = s.snapshot();
    assert_eq!(find_task(&snap, "design").status, TaskStatus::Waiting);
    assert_eq!(find_task(&snap, "independent").status, TaskStatus::Working);
    assert!(tool(&s, "task_get", json!({})).starts_with("independent [working]"));
    tool(
        &s,
        "task_update",
        json!({"percent":47, "message":"independent preparation"}),
    );
    tool(&s, "agent_status", json!({"status":"working"}));
    let snap = s.snapshot();
    assert_eq!(find_task(&snap, "independent").progress, Some(47));
    assert_eq!(find_task(&snap, "design").progress, Some(0));
    assert_eq!(find_task(&snap, "design").status, TaskStatus::Waiting);
    let workdir = &find_agent(&snap, "owner").workdir;
    std::fs::write(
        std::path::Path::new(workdir).join("proof.txt"),
        "independent preparation\n",
    )
    .unwrap();
    tool(
        &s,
        "git_commit",
        json!({"message":"Prepare independent design"}),
    );
    let snap = s.snapshot();
    assert!(find_task(&snap, "independent")
        .artifacts
        .iter()
        .any(|a| a.kind == "commit"));
    assert!(find_task(&snap, "design").artifacts.is_empty());
    // Consume the inbox during the other card. The durable wakeup must
    // still refer to this original answer at the later resume boundary.
    tool(&s, "agent_inbox", json!({}));
    assert!(tool(&s, "agent_inbox", json!({"id":answer.id})).contains("three tables"));
    tool(&s, "task_complete", json!({"result":"independent done"}));
    working(&s, "design");
    s.wait_for("resume notice", || {
        s.messages().iter().any(|m| {
            m.metadata["task_resumption"] == true
                && m.metadata["message_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(answer.id)))
        })
    });
    tool(
        &s,
        "task_complete",
        json!({"result":"design used three tables"}),
    );
    s.wait_all_done();
    // Retried completion is harmless, and emits no second completion.
    tool(
        &s,
        "task_complete",
        json!({"task_id":"design", "result":"retry"}),
    );
    for id in ["design", "independent"] {
        let events = s.events();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == "task.dispatched" && e.task_id.as_deref() == Some(id))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.event_type == "task.done" && e.task_id.as_deref() == Some(id))
                .count(),
            1
        );
    }
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.resumed")
            .count(),
        1
    );
    assert!(!s.has_event("scheduler.stall"));
}

#[test]
fn explicit_parked_operations_keep_executing_projection_and_refuse_second_execution() {
    let s = Session::new();
    s.run(&s.team(
        "parked-target",
        vec![agent("owner", "architect", &[])],
        vec![task("parked", "owner", &[]), task("current", "owner", &[])],
    ));
    working(&s, "parked");
    tool(&s, "task_update", json!({"percent":23}));
    park(&s);
    working(&s, "current");
    tool(
        &s,
        "task_update",
        json!({"percent":61, "message":"current preparation"}),
    );
    tool(&s, "agent_status", json!({"status":"working"}));
    tool(
        &s,
        "task_update",
        json!({"task_id":"parked", "percent":77, "message":"parked note"}),
    );
    let out = s.ctl(
        "owner",
        &[
            "tool",
            "agent_status",
            &json!({"status":"working", "task_id":"parked"}).to_string(),
        ],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("another task is executing"));
    tool(
        &s,
        "agent_status",
        json!({"status":"waiting", "task_id":"parked", "note":"still waiting"}),
    );
    let before = s.snapshot();
    s.send("owner", "Answer for the parked card.");
    s.wait_for("parked news", || {
        s.messages()
            .iter()
            .any(|m| m.body == "Answer for the parked card.")
    });
    tool(&s, "agent_inbox", json!({}));
    assert_eq!(find_agent(&before, "owner").progress, Some(61));
    assert_eq!(
        find_agent(&before, "owner").task.as_deref(),
        Some("task current")
    );
    assert_eq!(find_agent(&before, "owner").activity, "current preparation");
    tool(
        &s,
        "task_complete",
        json!({"task_id":"parked", "result":"completed explicitly"}),
    );
    let after = s.snapshot();
    assert_eq!(find_agent(&after, "owner").status, AgentStatus::Working);
    assert_eq!(find_agent(&after, "owner").progress, Some(61));
    assert_eq!(
        find_agent(&after, "owner").task.as_deref(),
        Some("task current")
    );
    assert_eq!(find_task(&after, "current").status, TaskStatus::Working);
    assert!(tool(&s, "task_get", json!({})).starts_with("current [working]"));
    tool(&s, "task_complete", json!({"result":"current done"}));
    s.wait_all_done();
    assert!(
        !s.has_event("task.resumed"),
        "finished parked card drops its cue"
    );
}

#[test]
fn multiple_parked_cards_resume_latest_started_first_when_capacity_returns() {
    let s = Session::new();
    s.run(&s.team(
        "parked-order",
        vec![agent("owner", "architect", &[])],
        vec![
            task("first", "owner", &[]),
            task("second", "owner", &[]),
            task("third", "owner", &[]),
            task("current", "owner", &[]),
        ],
    ));
    for id in ["first", "second", "third"] {
        working(&s, id);
        park(&s);
    }
    working(&s, "current");
    s.send("owner", "Answers for the parked designs are available.");
    s.wait_for("news delivered", || {
        find_agent(&s.snapshot(), "owner").inbox_total >= 5
    });
    tool(&s, "agent_inbox", json!({}));
    tool(&s, "task_complete", json!({"result":"current done"}));
    for id in ["third", "second", "first"] {
        working(&s, id);
        assert!(tool(&s, "task_get", json!({})).starts_with(&format!("{id} [working]")));
        tool(&s, "task_complete", json!({"result":format!("{id} done")}));
    }
    let snap = s.wait_all_done();
    assert_eq!(find_agent(&snap, "owner").status, AgentStatus::Done);
    let order: Vec<_> = s
        .events()
        .into_iter()
        .filter(|e| e.event_type == "task.resumed")
        .map(|e| e.task_id.unwrap())
        .collect();
    assert_eq!(order, ["third", "second", "first"]);
}

#[test]
fn a_parked_card_does_not_release_its_dependents_and_started_blocked_keeps_slot() {
    let s = Session::new();
    s.run(&s.team(
        "parked-gates",
        vec![
            agent("owner", "architect", &[]),
            agent("other", "build", &[]),
        ],
        vec![
            task("prerequisite", "owner", &[]),
            task("independent", "owner", &[]),
            task("dependent", "other", &["prerequisite"]),
        ],
    ));
    working(&s, "prerequisite");
    tool(
        &s,
        "agent_status",
        json!({"status":"blocked", "note":"cannot proceed"}),
    );
    let snap = s.snapshot();
    assert_eq!(find_task(&snap, "independent").status, TaskStatus::Ready);
    assert_eq!(find_task(&snap, "dependent").status, TaskStatus::Queued);
    tool(
        &s,
        "agent_status",
        json!({"status":"waiting", "note":"on answerer"}),
    );
    working(&s, "independent");
    assert_eq!(
        find_task(&s.snapshot(), "dependent").status,
        TaskStatus::Queued
    );
    tool(
        &s,
        "task_complete",
        json!({"task_id":"prerequisite", "result":"required input supplied"}),
    );
    working(&s, "dependent");
    tool(&s, "task_complete", json!({"result":"independent done"}));
    let out = s.ctl(
        "other",
        &["tool", "task_complete", "{\"result\":\"dependent done\"}"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    s.wait_all_done();
}

#[test]
fn reparking_consumes_each_news_cue_and_cue_free_cards_leave_ready_work_available() {
    let s = Session::new();
    s.run(&s.team(
        "parked-repark",
        vec![agent("owner", "architect", &[])],
        vec![
            task("first", "owner", &[]),
            task("second", "owner", &[]),
            task("current", "owner", &[]),
            task("next", "owner", &[]),
        ],
    ));
    for id in ["first", "second"] {
        working(&s, id);
        park(&s);
    }
    working(&s, "current");
    // Dispatching a new card is not a cue for either parked card.
    assert!(!s.has_event("task.resumed"));
    s.send("owner", "There is new information to inspect.");
    s.wait_for("new mail", || {
        find_agent(&s.snapshot(), "owner").inbox_total >= 4
    });
    tool(&s, "agent_inbox", json!({}));
    tool(&s, "task_complete", json!({"result":"current done"}));
    for id in ["second", "first"] {
        working(&s, id);
        tool(
            &s,
            "agent_status",
            json!({"status":"waiting", "task_id":id, "note":"answer still missing"}),
        );
    }
    working(&s, "next");
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.resumed")
            .count(),
        2
    );
    tool(&s, "task_complete", json!({"result":"next done"}));
    let waiting = s.wait("owner waiting after yielding independent work", |snap| {
        find_agent(snap, "owner").status == AgentStatus::Waiting
    });
    assert_eq!(find_task(&waiting, "first").status, TaskStatus::Waiting);
    assert_eq!(find_task(&waiting, "second").status, TaskStatus::Waiting);
    // Explicit status and inbox reads trigger ordinary runtime work, but do
    // not turn the old news into another cue or another resume notice.
    park(&s);
    tool(&s, "agent_inbox", json!({"all":true}));
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.resumed")
            .count(),
        2
    );
    s.send("owner", "Another answer has arrived.");
    working(&s, "second");
    tool(&s, "task_complete", json!({"result":"second done"}));
    working(&s, "first");
    tool(&s, "task_complete", json!({"result":"first done"}));
    s.wait_all_done();
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.resumed")
            .count(),
        3
    );
}

#[test]
fn an_at_rest_answer_resumes_in_its_delivery_without_a_second_notice() {
    let s = Session::new();
    s.run(&s.team(
        "parked-direct-answer",
        vec![agent("owner", "architect", &[])],
        vec![task("design", "owner", &[])],
    ));
    working(&s, "design");
    tool(&s, "agent_inbox", json!({}));
    park(&s);
    let before = s
        .events()
        .iter()
        .filter(|e| e.event_type == "message.delivered" && e.agent_id.as_deref() == Some("owner"))
        .count();
    s.send("owner", "The requested answer is available.");
    working(&s, "design");
    s.wait_for("answer delivery logged", || {
        s.events()
            .iter()
            .filter(|e| {
                e.event_type == "message.delivered" && e.agent_id.as_deref() == Some("owner")
            })
            .count()
            > before
    });
    assert!(!s.has_event("task.resumed"));
    assert!(!s
        .messages()
        .iter()
        .any(|m| m.metadata["task_resumption"] == true));
    let after = s
        .events()
        .iter()
        .filter(|e| e.event_type == "message.delivered" && e.agent_id.as_deref() == Some("owner"))
        .count();
    assert_eq!(after, before + 1);
    tool(&s, "task_complete", json!({"result":"design done"}));
    s.wait_all_done();
}

#[test]
fn rework_uses_a_parked_owners_free_slot_without_redispatching_the_card() {
    let s = Session::new();
    let mut rework = task("rework", "owner", &[]);
    rework.reviewer = Some("critic".into());
    s.run(&s.team(
        "parked-rework",
        vec![
            agent("owner", "architect", &[]),
            agent("critic", "review", &[]),
        ],
        vec![rework, task("parked", "owner", &[])],
    ));
    working(&s, "rework");
    tool(&s, "task_complete", json!({"result":"first version"}));
    working(&s, "parked");
    park(&s);
    let verdict = |approve| {
        let out = s.ctl(
            "critic",
            &[
                "tool",
                "review_submit",
                &json!({"task_id":"rework", "approve":approve, "comments":"reviewed"}).to_string(),
            ],
        );
        assert!(out.status.success(), "{}", stderr(&out));
    };
    verdict(false);
    working(&s, "rework");
    assert!(tool(&s, "task_get", json!({})).starts_with("rework [working]"));
    assert_eq!(
        find_task(&s.snapshot(), "parked").status,
        TaskStatus::Waiting
    );
    assert_eq!(
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.dispatched" && e.task_id.as_deref() == Some("rework"))
            .count(),
        1
    );
    tool(&s, "task_complete", json!({"result":"revised version"}));
    verdict(true);
    tool(
        &s,
        "task_complete",
        json!({"task_id":"parked", "result":"parked work done"}),
    );
    s.wait_all_done();
}

#[test]
fn a_fresh_restart_clears_the_turn_that_parked_and_redispatches_open_cards() {
    if common::test_support::isolate_operator_test(
        "a_fresh_restart_clears_the_turn_that_parked_and_redispatches_open_cards",
    ) {
        return;
    }
    let s = Session::new();
    s.run(&s.team(
        "parked-fresh-restart",
        vec![agent("owner", "architect", &[])],
        vec![task("first", "owner", &[]), task("next", "owner", &[])],
    ));
    working(&s, "first");
    let identity = crew_core::paths::Identity::load(&s.identity_path("owner")).unwrap();
    let (mut client, _) = crew_core::client::Client::agent(&identity, "parking-test").unwrap();
    client
        .call_value(crew_core::rpc::Call::Hook {
            event: "turn-start".into(),
            payload: Value::Null,
            launch: None,
        })
        .unwrap();
    park(&s);
    assert_eq!(find_task(&s.snapshot(), "next").status, TaskStatus::Ready);
    let (mut operator, _) =
        crew_core::client::Client::operator(std::path::Path::new(&identity.socket), "restart-test")
            .unwrap();
    operator
        .call_value(crew_core::rpc::Call::AgentRestartFresh { id: "owner".into() })
        .unwrap();
    working(&s, "first");
    s.wait_for("first redispatched to the fresh session", || {
        s.events()
            .iter()
            .filter(|e| e.event_type == "task.dispatched" && e.task_id.as_deref() == Some("first"))
            .count()
            == 2
    });
    tool(&s, "task_complete", json!({"result":"first after restart"}));
    working(&s, "next");
    tool(&s, "task_complete", json!({"result":"next after restart"}));
    s.wait_all_done();
}
