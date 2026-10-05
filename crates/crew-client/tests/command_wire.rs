use crew_client::{ClientError, Command, CommandKind, CommandResult};
use crew_core::rpc::Request;
use crew_protocol::MAX_FRAME_BYTES;
use serde_json::{json, Value};

#[path = "support/commands.rs"]
mod commands;
use commands::construct;

#[test]
fn encoder_is_byte_equal_to_the_real_request_for_every_catalogue_variant() {
    for kind in CommandKind::ALL {
        for boundary in [false, true] {
            let command = construct(*kind, boundary);
            let bytes = command.encode(u64::MAX).unwrap();
            let host: Request = serde_json::from_str(&bytes).unwrap();
            assert_eq!(host.call.method(), kind.method());
            assert_eq!(serde_json::to_string(&host).unwrap(), bytes, "{kind:?}");
            if *kind == CommandKind::Inbox {
                assert!(bytes.contains("\"unread_only\":false,\"ack\":false"));
            }
        }
    }
}

#[test]
fn real_encoder_reproduces_all_existing_64_canonical_byte_records() {
    for line in include_str!("../../../fixtures/native/actions/request-bytes.jsonl").lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        let request = &record["request"];
        let mut schema = json!({"kind":request["method"]});
        if let Some(params) = request.get("params") {
            schema["params"] = params.clone();
        }
        let command: Command = serde_json::from_value(schema).unwrap();
        assert_eq!(
            command.encode(request["id"].as_u64().unwrap()).unwrap(),
            record["json"],
            "{}",
            record["case_id"]
        );
    }
}

#[test]
fn result_decoder_consumes_real_fixture_payloads_for_every_available_method() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/results.json")).unwrap();
    let mut covered = 0;
    for kind in CommandKind::ALL {
        // Handler result fixtures include null for these methods, but the
        // real Session writer suppresses every successful terminal reply.
        if matches!(kind, CommandKind::AttachInput | CommandKind::AttachResize) {
            assert!(construct(*kind, false).decode_result(Value::Null).is_err());
            continue;
        }
        if let Some(payloads) = fixtures.get(kind.method()).and_then(Value::as_array) {
            let mut command = construct(*kind, false);
            if let Command::Attach { agent, .. } = &mut command {
                *agent = payloads[0]["attached"].as_str().unwrap().into();
            }
            for payload in payloads {
                command.decode_result(payload.clone()).unwrap();
            }
            covered += 1;
        }
    }
    assert!(
        covered >= 24,
        "fixture catalogue unexpectedly shrank: {covered}"
    );
}

#[test]
fn malformed_method_payloads_fail_without_false_acknowledgements() {
    for (command, field) in [
        (Command::Ping, "pong"),
        (Command::Subscribe, "subscribed"),
        (Command::Shutdown { stop_agents: true }, "stopping"),
        (Command::AgentFocus { id: "a".into() }, "ok"),
        (
            Command::AgentInput {
                id: "a".into(),
                text: None,
                submit: false,
            },
            "ok",
        ),
    ] {
        for invalid in [
            json!(null),
            json!({}),
            json!({field:false}),
            json!({field:1}),
            json!({field:"true"}),
        ] {
            assert!(matches!(
                command.decode_result(invalid),
                Err(ClientError::DecodeFailure { .. })
            ));
        }
    }
    let attach = Command::Attach {
        agent: "a".into(),
        rows: 24,
        cols: 80,
    };
    assert!(attach.decode_result(json!({"attached":"b"})).is_err());
    assert!(Command::AgentsList
        .decode_result(json!({"agents":[]}))
        .is_err());
    assert!(Command::TasksList
        .decode_result(json!({"tasks":[]}))
        .is_err());
    assert!(Command::AgentCompact {
        id: Some("a".into()),
        keep: None
    }
    .decode_result(Value::Null)
    .is_err());
    assert_eq!(
        Command::AgentKill { id: "a".into() }
            .decode_result(Value::Null)
            .unwrap(),
        CommandResult::Agent(None)
    );
    assert!(Command::Resume {
        agents: vec!["a".into()],
        restart: false
    }
    .decode_result(json!({"resumed":[null]}))
    .is_err());
}

#[test]
fn oversized_encoding_fails_before_a_host_can_write_a_frame() {
    let command = Command::AttachInput {
        data: "a".repeat(MAX_FRAME_BYTES),
    };
    assert!(matches!(
        command.encode(1),
        Err(ClientError::TooLarge { .. })
    ));
}
