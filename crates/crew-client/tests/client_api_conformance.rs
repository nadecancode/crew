//! Permanent public ClientApi consumer. Fixtures are workspace-relative; a
//! future crate publication must package them or provide a separate test corpus.

use std::collections::{BTreeMap, BTreeSet};

use crew_client::policy::NATIVE_LIMITS;
use crew_client::*;
use crew_protocol::rpc::{Push, ServerFrame, Snapshot};
use serde_json::{json, Value};

fn clock(at: i64) -> ClockSample {
    ClockSample {
        wall_ms: at,
        monotonic_ms: at.max(0) as u64,
    }
}

fn scope() -> ClientScope {
    ClientScope {
        endpoint_id: "fixture".into(),
        workspace_id: "w".into(),
        generation: None,
        daemon_pid: None,
        daemon_started_at_ms: None,
        attempt: 1,
    }
}

fn replay_case(text: &str) {
    let case: Value = serde_json::from_str(text).unwrap();
    let mut concrete = ClientCore::new(ClientConfig {
        limits: NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::BufferedOverlap,
    })
    .unwrap();
    // Exercise the public trait object used by host wrappers, including real
    // reduction, projection, action registry and clock handling.
    let client: &mut dyn ClientApi = &mut concrete;
    client
        .apply(
            ClientInput::Bind {
                scope: scope(),
                mode: ConnectionMode::LocalOperator,
                grant: Grant {
                    grant_id: "fixture".into(),
                    tiers: vec![GrantTier::Read, GrantTier::Control],
                    workspace_ids: vec!["w".into()],
                    expires_at_ms: None,
                },
                snapshot_mode: SnapshotMode::BufferedOverlap,
            },
            clock(0),
        )
        .unwrap();
    let mut steps = 0;
    let mut known_agents = BTreeSet::new();
    let mut known_tasks = BTreeSet::new();
    for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        let at = step["at_ms"].as_i64().unwrap();
        if let Some(snapshot) = step.get("snapshot") {
            let snapshot: Snapshot = serde_json::from_value(snapshot.clone()).unwrap();
            known_agents.extend(snapshot.agents.iter().map(|agent| agent.id.clone()));
            known_tasks.extend(snapshot.tasks.iter().map(|task| task.id.clone()));
            let operation_id = (1 << 63) + index as u64;
            client
                .apply(
                    ClientInput::SnapshotStarted {
                        scope: scope(),
                        operation_id,
                    },
                    clock(at),
                )
                .unwrap();
            client
                .apply(
                    ClientInput::Snapshot {
                        scope: scope(),
                        operation_id,
                        snapshot: Box::new(snapshot),
                    },
                    clock(at),
                )
                .unwrap();
        } else {
            let frame: ServerFrame = serde_json::from_value(step["frame"].clone()).unwrap();
            let push = frame.push.unwrap();
            match &push {
                Push::Agent(agent) => {
                    known_agents.insert(agent.id.clone());
                }
                Push::AgentRemoved { id } => {
                    known_agents.insert(id.clone());
                }
                Push::Task(task) => {
                    known_tasks.insert(task.id.clone());
                }
                _ => {}
            }
            client
                .apply(
                    ClientInput::Push {
                        scope: scope(),
                        push,
                    },
                    clock(at),
                )
                .unwrap();
        }
        let ViewPayload::Team(team) = client.view(ViewQuery::Team, clock(at)).unwrap().payload
        else {
            panic!("team payload");
        };
        let ViewPayload::Tasks(tasks) = client.view(ViewQuery::Tasks, clock(at)).unwrap().payload
        else {
            panic!("tasks payload");
        };
        let ViewPayload::Messages(messages) = client
            .view(
                ViewQuery::Messages {
                    before: None,
                    limit: 200,
                    filter: MessageFilter {
                        endpoints: MessageEndpoints::All,
                        kind: None,
                        text: String::new(),
                    },
                },
                clock(at),
            )
            .unwrap()
            .payload
        else {
            panic!("messages payload");
        };
        let ViewPayload::Events(events) = client
            .view(
                ViewQuery::Events {
                    before: None,
                    limit: 200,
                    filter: EventFilter {
                        agent_id: None,
                        level: None,
                        text: String::new(),
                        vcs_only: false,
                    },
                },
                clock(at),
            )
            .unwrap()
            .payload
        else {
            panic!("events payload");
        };
        let ViewPayload::Questions(questions) = client
            .view(ViewQuery::Questions, clock(at))
            .unwrap()
            .payload
        else {
            panic!("questions payload");
        };
        let mut agent_ids: Vec<_> = team.agents.iter().map(|p| &p.agent.id).collect();
        agent_ids.sort();
        let agents: BTreeMap<_, _> = team
            .agents
            .iter()
            .map(|p| (&p.agent.id, &p.agent))
            .collect();
        let task_records: BTreeMap<_, _> =
            tasks.tasks.iter().map(|p| (&p.task.id, &p.task)).collect();
        let work: BTreeMap<_, _> = team
            .agents
            .iter()
            .map(|p| (&p.agent.id, &p.current_work))
            .collect();
        let actual = json!({ "agent_ids":agent_ids,"agents":agents,
            "task_ids":tasks.tasks.iter().map(|p| &p.task.id).collect::<Vec<_>>(),"tasks":task_records,
            "messages":messages.items,"message_ids":messages.items.iter().map(|m| &m.id).collect::<Vec<_>>(),
            "events":events.items,"event_seqs":events.items.iter().map(|e| e.seq).collect::<Vec<_>>(),
            "question_ids":questions.iter().map(|q| &q.id).collect::<Vec<_>>(),"work":work });
        let mut expected = step["expect"].clone();
        // Clock offset is internal; work observations verify its actual public
        // effect. Navigation validity is inspected through detail queries.
        expected.as_object_mut().unwrap().remove("clock_offset_ms");
        let selection = expected
            .as_object_mut()
            .unwrap()
            .remove("selection_validity");
        subset(&actual, &expected, &format!("{}/{index}", case["id"]));
        let mut observations = BTreeMap::new();
        // Query identities observed in wire inputs, never generated from
        // expected outputs. Retain disappeared identities to inspect them.
        for id in &known_agents {
            let ViewPayload::AgentDetail(detail) = client
                .view(ViewQuery::AgentDetail { id: id.clone() }, clock(at))
                .unwrap()
                .payload
            else {
                panic!("agent detail");
            };
            observations.insert(("agent", id.as_str()), detail.agent.is_some());
        }
        for id in &known_tasks {
            let ViewPayload::TaskDetail(detail) = client
                .view(ViewQuery::TaskDetail { id: id.clone() }, clock(at))
                .unwrap()
                .payload
            else {
                panic!("task detail");
            };
            observations.insert(("task", id.as_str()), detail.task.is_some());
        }
        if let Some(selection) = selection {
            for item in selection.as_array().unwrap() {
                let kind = item["kind"].as_str().unwrap();
                let id = item["id"].as_str().unwrap();
                assert_eq!(
                    observations[&(kind, id)],
                    item["exists"].as_bool().unwrap(),
                    "{} step{index} {kind}:{id}",
                    case["id"]
                );
            }
        }
        steps += 1;
    }
    assert!(steps > 0);
}

fn subset(actual: &Value, expected: &Value, path: &str) {
    match expected {
        Value::Object(fields) => {
            for (key, expected) in fields {
                subset(
                    actual
                        .get(key)
                        .unwrap_or_else(|| panic!("{path}.{key}:missing")),
                    expected,
                    &format!("{path}.{key}"),
                );
            }
        }
        Value::Array(items) => {
            let values = actual.as_array().unwrap();
            assert_eq!(values.len(), items.len(), "{path}");
            for (index, (actual, expected)) in values.iter().zip(items).enumerate() {
                subset(actual, expected, &format!("{path}[{index}]"));
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}

#[test]
fn all_reviewed_state_vectors_replay_through_the_real_public_api() {
    for text in [
        include_str!("../../../fixtures/native/state/work-choice.case.json"),
        include_str!("../../../fixtures/native/state/parked-resumption.case.json"),
        include_str!("../../../fixtures/native/state/completed-ties.case.json"),
        include_str!("../../../fixtures/native/state/optional-values.case.json"),
        include_str!("../../../fixtures/native/state/snapshot-replacement.case.json"),
    ] {
        replay_case(text);
    }
}
