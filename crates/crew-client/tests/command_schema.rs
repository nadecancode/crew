//! Schema compatibility uses the real host Request, without defining the
//! production encoder or decoder owned by the actions slice.

use crew_client::{Command, CommandKind, CommandResult};
use crew_core::rpc::{Call, Request};
use crew_protocol::rpc::*;
use crew_protocol::{AgentStatus, RuntimeKind};
use serde_json::{json, Value};

// Exhaustive: adding a catalogue variant without a constructor is a compile
// error. Defaults and integer/optional boundaries are supplied independently.
fn construct(kind: CommandKind, boundary: bool) -> Command {
    let id = "claude-1".to_string();
    let optional = boundary.then(|| "main".to_string());
    let count = boundary.then_some(u32::MAX);
    match kind {
        CommandKind::Ping => Command::Ping,
        CommandKind::Snapshot => Command::Snapshot {
            messages: count,
            events: count,
        },
        CommandKind::Subscribe => Command::Subscribe,
        CommandKind::Workspaces => Command::Workspaces,
        CommandKind::AgentsList => Command::AgentsList,
        CommandKind::TasksList => Command::TasksList,
        CommandKind::TaskGet => Command::TaskGet { id: optional },
        CommandKind::History => Command::History {
            agent: optional,
            limit: count,
        },
        CommandKind::MessageGet => Command::MessageGet {
            id: "m-0104".into(),
        },
        CommandKind::Events => Command::Events {
            agent: optional,
            limit: count,
        },
        CommandKind::Inbox => Command::Inbox {
            agent: optional,
            limit: count,
        },
        CommandKind::AgentOutput => Command::AgentOutput {
            id,
            lines: count,
            screen: boundary,
            ansi: boundary,
        },
        CommandKind::AgentTranscript => Command::AgentTranscript { id, limit: count },
        CommandKind::GitDiff => Command::GitDiff {
            against: optional.clone(),
            agent: optional,
        },
        CommandKind::ModelsList => Command::ModelsList,
        CommandKind::Send => Command::Send(SendParams {
            to: id,
            body: "  résumé\n账目\u{0}  ".into(),
            id: boundary.then(|| "w-0102030405060708".into()),
            correlation_id: optional.clone(),
            reply_to: optional,
            metadata: if boundary {
                json!({"integer":u64::MAX, "empty":{}, "nested":[null,0]})
            } else {
                Value::Null
            },
            ..SendParams::default()
        }),
        CommandKind::Answer => Command::Answer {
            id: "m-9001".into(),
            body: "  Use the adapter  ".into(),
        },
        CommandKind::AgentPause => Command::AgentPause { id },
        CommandKind::AgentResume => Command::AgentResume { id },
        CommandKind::AgentCompact => Command::AgentCompact {
            id: optional.clone(),
            keep: optional,
        },
        CommandKind::AgentRestart => Command::AgentRestart { id },
        CommandKind::AgentRestartFresh => Command::AgentRestartFresh { id },
        CommandKind::AgentKill => Command::AgentKill { id },
        CommandKind::AgentSetModel => Command::AgentSetModel {
            id,
            model: if boundary {
                "opus".into()
            } else {
                String::new()
            },
            runtime: boundary.then_some(RuntimeKind::Claude),
            note: optional,
        },
        CommandKind::AgentFocus => Command::AgentFocus { id },
        CommandKind::AgentInput => Command::AgentInput {
            id,
            text: boundary.then(|| "\r\u{1b}[200~text\u{1b}[201~".into()),
            submit: boundary,
        },
        CommandKind::Resume => Command::Resume {
            agents: if boundary { vec![id] } else { vec![] },
            restart: boundary,
        },
        CommandKind::Shutdown => Command::Shutdown {
            stop_agents: boundary,
        },
        CommandKind::Attach => Command::Attach {
            agent: id,
            rows: if boundary { u16::MAX } else { 0 },
            cols: if boundary { u16::MAX } else { 0 },
        },
        CommandKind::AttachInput => Command::AttachInput {
            data: if boundary {
                "ABuA/w==".into()
            } else {
                String::new()
            },
        },
        CommandKind::AttachResize => Command::AttachResize {
            rows: if boundary { u16::MAX } else { 0 },
            cols: if boundary { u16::MAX } else { 0 },
        },
    }
}

#[test]
fn all_31_command_records_have_compatible_protocol_6_parameter_shapes() {
    assert_eq!(CommandKind::ALL.len(), 31);
    for kind in CommandKind::ALL {
        for boundary in [false, true] {
            let command = construct(*kind, boundary);
            assert_eq!(command.kind(), *kind);
            let schema = serde_json::to_value(&command).unwrap();
            assert_eq!(schema["kind"], kind.method());
            let restored: Command = serde_json::from_value(schema.clone()).unwrap();
            assert_eq!(restored, command);
            // A client schema is deliberately not a request envelope.
            assert!(serde_json::from_value::<Request>(schema.clone()).is_err());
            let mut request = json!({"id":u64::MAX,"method":kind.method()});
            if let Some(params) = schema.get("params") {
                request["params"] = params.clone();
            }
            if *kind == CommandKind::Inbox {
                request["params"]["unread_only"] = json!(false);
                request["params"]["ack"] = json!(false);
            }
            let host: Request = serde_json::from_value(request).unwrap();
            assert_eq!(host.call.method(), kind.method());
            if let Call::Inbox {
                unread_only, ack, ..
            } = host.call
            {
                assert!(!unread_only && !ack);
            }
            let bytes = serde_json::to_string(&host).unwrap();
            if *kind == CommandKind::Inbox {
                let agent = if boundary { "\"main\"" } else { "null" };
                let limit = if boundary {
                    u32::MAX.to_string()
                } else {
                    "null".into()
                };
                assert_eq!(bytes, format!("{{\"id\":{},\"method\":\"inbox\",\"params\":{{\"agent\":{agent},\"unread_only\":false,\"ack\":false,\"limit\":{limit}}}}}", u64::MAX));
            }
            let restored: Request = serde_json::from_str(&bytes).unwrap();
            assert_eq!(serde_json::to_string(&restored).unwrap(), bytes);
            let host_params = serde_json::to_value(&restored.call).unwrap();
            if *kind != CommandKind::Inbox {
                assert_eq!(host_params.get("params"), schema.get("params"));
            }
        }
    }
}

#[test]
fn existing_canonical_request_byte_corpus_stays_unchanged_after_rpc_extraction() {
    let text = include_str!("../../../fixtures/native/actions/request-bytes.jsonl");
    let mut count = 0;
    for line in text.lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        let bytes = record["json"].as_str().unwrap();
        let request: Request = serde_json::from_str(bytes).unwrap();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            bytes,
            "{}",
            record["case_id"]
        );
        count += 1;
    }
    assert_eq!(count, 64);
}

#[test]
fn app_schema_excludes_admin_hello_and_credential_calls_and_rejects_invalid_counts() {
    for method in [
        "hello",
        "load_team",
        "credential",
        "web_open",
        "web_close",
        "agent_add",
        "task_create",
        "hook",
        "git_commit",
    ] {
        assert!(serde_json::from_value::<Command>(json!({"kind":method,"params":{}})).is_err());
    }
    for limit in [json!(-1), json!(u64::MAX), json!("300"), json!(1.5)] {
        assert!(serde_json::from_value::<Command>(
            json!({"kind":"history","params":{"agent":null,"limit":limit}})
        )
        .is_err());
    }
    assert!(serde_json::from_value::<Command>(json!({"kind":"agent_set_model","params":{"id":"a","model":"","runtime":"native","note":null}})).is_err());
}

#[test]
fn every_success_result_constructs_with_bare_array_and_optional_agent_semantics() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    let snapshot: Snapshot = serde_json::from_value(fixtures["snapshot"][0].clone()).unwrap();
    let message = snapshot.messages.first().cloned().unwrap();
    let agent = snapshot.agents.first().cloned().unwrap();
    let values = [
        CommandResult::Pong,
        CommandResult::Subscribed,
        CommandResult::Stopping,
        CommandResult::Ok,
        CommandResult::Attached {
            agent: "claude-1".into(),
        },
        CommandResult::Null,
        CommandResult::Snapshot(Box::new(snapshot.clone())),
        CommandResult::Workspaces(Workspaces::default()),
        CommandResult::Agents(snapshot.agents.clone()),
        CommandResult::Tasks(snapshot.tasks.clone()),
        CommandResult::TaskCard(Box::new(
            serde_json::from_value(fixtures["task_get"][0].clone()).unwrap(),
        )),
        CommandResult::Messages(vec![message.clone()]),
        CommandResult::Message(Box::new(message)),
        CommandResult::Events(snapshot.events),
        CommandResult::Inbox(InboxResult::default()),
        CommandResult::Output(OutputResult::default()),
        CommandResult::Transcript(TranscriptResult::default()),
        CommandResult::Diff(Box::default()),
        CommandResult::Models(ModelsResult::default()),
        CommandResult::Sent(Box::new(
            serde_json::from_value(fixtures["send"][0].clone()).unwrap(),
        )),
        CommandResult::Agent(Some(Box::new(agent))),
        CommandResult::Agent(None),
        CommandResult::Resumed {
            agents: vec!["claude-1".into()],
        },
    ];
    for value in values {
        let bytes = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<CommandResult>(&bytes).unwrap(),
            value
        );
    }
    assert!(!AgentStatus::Done.is_terminal());
}

#[test]
fn portable_frame_decoder_retains_known_validation_and_unknown_push_behavior() {
    for data in [
        json!({"future":true}),
        json!([null, 1]),
        json!("future"),
        Value::Null,
    ] {
        let frame: ServerFrame =
            serde_json::from_value(json!({"push":{"kind":"future","data":data}})).unwrap();
        assert_eq!(frame.push, Some(Push::Unknown));
    }
    for data in [
        json!({"kind":"agent_removed","data":{}}),
        json!({"kind":"message","data":{"id":1}}),
        json!({"data":{}}),
    ] {
        assert!(serde_json::from_value::<ServerFrame>(json!({"push":data})).is_err());
    }
    let pushes = include_str!("../../crew/tests/fixtures/web/pushes.jsonl");
    for line in pushes.lines() {
        let original: Value = serde_json::from_str(line).unwrap();
        let frame: ServerFrame = serde_json::from_value(original.clone()).unwrap();
        let restored: ServerFrame =
            serde_json::from_value(serde_json::to_value(&frame).unwrap()).unwrap();
        assert_eq!(restored.push, frame.push);
    }
}
