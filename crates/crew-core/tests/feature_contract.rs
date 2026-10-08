use std::collections::BTreeSet;

use crew_core::rpc::{feature_decode_error, features, Push, Request, SessionInfo, Snapshot};
use crew_core::Task;
use serde_json::{json, Value};

#[test]
fn legacy_bytes_match_the_frozen_v6_decoder_and_encoder() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/features/legacy-v6.json")).unwrap();
    assert_eq!(
        fixture["source_commit"],
        "b4f9a4424c53ddf54a6db1d0d82164015221faaa"
    );
    for entry in fixture["requests"].as_array().unwrap() {
        let request: Request = serde_json::from_value(entry["input"].clone()).unwrap();
        assert_eq!(serde_json::to_string(&request).unwrap(), entry["canonical"]);
    }
    let task: Task = serde_json::from_str(fixture["task"].as_str().unwrap()).unwrap();
    assert_eq!(task.epic, None);
    assert_eq!(serde_json::to_string(&task).unwrap(), fixture["task"]);
    let session: SessionInfo = serde_json::from_str(fixture["session"].as_str().unwrap()).unwrap();
    assert_eq!(session.workspace_run_id, None);
    assert_eq!(serde_json::to_string(&session).unwrap(), fixture["session"]);
    let snapshot: Snapshot = serde_json::from_str(fixture["snapshot"].as_str().unwrap()).unwrap();
    assert_eq!(snapshot.epics, None);
    assert_eq!(
        serde_json::to_string(&snapshot).unwrap(),
        fixture["snapshot"]
    );
    for encoded in fixture["pushes"].as_array().unwrap() {
        let push: Push = serde_json::from_str(encoded.as_str().unwrap()).unwrap();
        assert_eq!(serde_json::to_string(&push).unwrap(), *encoded);
    }
    for entry in fixture["scoped_methods_rejected"].as_array().unwrap() {
        assert_eq!(entry["unknown_variant"], true);
    }
}

#[test]
fn every_new_method_has_a_typed_round_trip_and_inventory_entry() {
    let fixture: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/features/requests-v1.json")).unwrap();
    let expected: BTreeSet<_> = features::FEATURE_METHODS.iter().map(|m| m.name).collect();
    let mut seen = BTreeSet::new();
    for wire in &fixture {
        let request: Request = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(request.call.method(), wire["method"]);
        assert_eq!(serde_json::to_value(&request).unwrap(), *wire);
        assert!(features::feature_method(request.call.method()).is_some());
        assert!(seen.insert(wire["method"].as_str().unwrap()));
        let mut extra = wire.clone();
        extra["params"]["unknown_scoped_intent"] = json!(true);
        assert!(serde_json::from_value::<Request>(extra).is_err(), "{wire}");
    }
    assert_eq!(seen, expected);
    assert_eq!(expected.len(), 33);
}

#[test]
fn known_malformed_feature_does_not_masquerade_as_old_peer() {
    let malformed =
        json!({"id":9,"method":"features_negotiate","params":{"versions":{"epics":"one"}}});
    assert!(serde_json::from_value::<Request>(malformed.clone()).is_err());
    assert_eq!(
        feature_decode_error(&malformed).unwrap().code,
        "invalid_feature_request"
    );
    assert!(feature_decode_error(&json!({"method":"unknown_future_method"})).is_none());
    assert!(feature_decode_error(&json!({"method":"hello"})).is_none());
}

#[test]
fn aside_decode_limits_return_private_typed_refusals() {
    let mut request = json!({"id":9,"method":"aside_ask","params":{
        "workspace_run_id":"run-1","request_id":"request-1",
        "question":"é".repeat(4097),"targets":null
    }});
    assert!(serde_json::from_value::<Request>(request.clone()).is_err());
    let error = feature_decode_error(&request).unwrap();
    assert_eq!(error.code, "too_large");
    assert_eq!(
        serde_json::to_value(&error).unwrap()["data"],
        json!({"reason":"question_limit","max_bytes":8192})
    );
    assert!(!format!("{error:?}").contains('é'));
    request["params"]["question"] = json!("synthetic secret body");
    request["params"]["targets"] = json!(vec![json!({"agent_id":"a","incarnation":"i"}); 65]);
    let error = feature_decode_error(&request).unwrap();
    assert_eq!(
        serde_json::to_value(&error).unwrap()["data"],
        json!({"reason":"target_limit","max_targets":64})
    );
    request["params"]["targets"] = Value::Null;
    let decoded: Request = serde_json::from_value(request).unwrap();
    assert!(!format!("{decoded:?}").contains("synthetic secret body"));
}
