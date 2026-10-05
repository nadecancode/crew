//! Schema compatibility uses the real host Request, without defining the
//! production encoder or decoder owned by the actions slice.

use crew_client::{Command, CommandKind, CommandResult};
use crew_core::rpc::{Call, Request};
use crew_protocol::rpc::*;
use crew_protocol::AgentStatus;
use serde_json::{json, Value};

#[path = "support/commands.rs"]
mod commands;
use commands::construct;

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
