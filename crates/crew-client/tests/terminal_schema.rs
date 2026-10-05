//! Constructor/shape conformance for the approved native terminal amendment.
//! Behavior belongs to the actual planner/driver consumer suites.

use crew_client::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

fn records<T: DeserializeOwned + Serialize>(case: &Value, field: &str) {
    if let Some(values) = case[field].as_array() {
        for value in values {
            let record: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                serde_json::to_value(record).unwrap(),
                *value,
                "{}:{field}",
                case["id"]
            );
        }
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/native/actions/native/terminal-routing.json"
    ))
    .unwrap()
}

#[test]
fn eight_contract_tagged_cases_construct_real_records_and_preserve_scope_target() {
    let data = fixture();
    assert_eq!(data["kind"], "native_contract");
    assert_eq!(
        data["contract_commit"],
        "986dcb8ccfc8126d0d0568b9241167fcebe2e1ce"
    );
    let ids = [
        "native-terminal-two-attach",
        "native-terminal-route-input",
        "native-terminal-route-resize",
        "native-terminal-close-one",
        "native-terminal-stale-id",
        "native-terminal-unacked-attach",
        "native-terminal-retire-scope",
        "native-terminal-duplicate-close",
    ];
    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), ids.len());
    for (case, id) in cases.iter().zip(ids) {
        assert_eq!(case["id"], id);
        records::<ActionIntent>(case, "intents");
        records::<ActionPlan>(case, "plans");
        records::<ClientEffect>(case, "effects");
        records::<OperationOutcome>(case, "outcomes");
        records::<ClientInput>(case, "inputs");
        records::<OperationNotice>(case, "notices");
        records::<TerminalChunk>(case, "chunks");
    }
    let first = &cases[0]["effects"][0]["data"];
    let second = &cases[0]["effects"][1]["data"];
    assert_eq!(
        first["operation_id"],
        first["lane"]["terminal"]["attachment_id"]
    );
    assert_eq!(
        second["operation_id"],
        second["lane"]["terminal"]["attachment_id"]
    );
    assert_ne!(first["lane"], second["lane"]);
    assert_eq!(first["scope"], second["scope"]);
    assert_eq!(
        cases[1]["effects"][0]["data"]["lane"]["terminal"]["attachment_id"],
        101
    );
    assert_eq!(
        cases[2]["effects"][0]["data"]["lane"]["terminal"]["attachment_id"],
        102
    );
}

#[test]
fn dispatch_and_local_close_cannot_be_mistaken_for_a_second_command_or_wire_null() {
    let dispatch = serde_json::to_value(ActionPlan::Dispatch { operation_id: 203 }).unwrap();
    assert_eq!(
        dispatch,
        json!({"kind":"dispatch","data":{"operation_id":203}})
    );
    let closed = OperationOutcome::ClosedLocally { attachment_id: 101 };
    assert_ne!(
        serde_json::to_value(closed).unwrap(),
        serde_json::to_value(OperationOutcome::Succeeded {
            result: CommandResult::Null
        })
        .unwrap()
    );
    let cases = fixture();
    assert_eq!(cases["cases"][3]["effects"][0]["kind"], "close_attachment");
    assert_eq!(cases["cases"][3]["notices"][0]["kind"], "closed_locally");
    assert_eq!(cases["cases"][3]["notices"][0]["attachment_id"], 101);
    assert_ne!(
        cases["cases"][3]["notices"][0]["operation_id"],
        cases["cases"][3]["notices"][0]["attachment_id"]
    );
    assert_eq!(cases["cases"][3]["inputs"][0]["kind"], "attachment_ended");
    assert_eq!(
        cases["cases"][3]["expected_behavior"]["daemon_acknowledgement"],
        false
    );
}
