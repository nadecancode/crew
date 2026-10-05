//! Fixture-only probe of the real protocol serializer. No daemon or network IO.

use crew_core::rpc::Request;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn record(case_id: &str, index: usize, request: Request) -> Value {
    let bytes = serde_json::to_string(&request).expect("serialize Request");
    json!({
        "case_id": case_id,
        "call_index": index,
        "request": serde_json::to_value(&request).unwrap(),
        "json": bytes,
    })
}

fn with_envelope(call: &Value) -> Request {
    let mut request = call.as_object().expect("call object").clone();
    request.insert("id".into(), json!(7));
    serde_json::from_value(Value::Object(request)).expect("fixture call decodes as Request")
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("fixture directory"));
    let write = std::env::args().any(|arg| arg == "--write");
    let print = std::env::args().any(|arg| arg == "--print");
    let table: Value =
        serde_json::from_str(&fs::read_to_string(root.join("capabilities.json")).unwrap()).unwrap();
    for (name, actual) in [
        ("max_message_bytes", crew_core::types::MAX_MESSAGE_BYTES),
        (
            "max_question_options",
            crew_core::types::MAX_QUESTION_OPTIONS,
        ),
        ("max_option_chars", crew_core::types::MAX_OPTION_CHARS),
    ] {
        assert_eq!(table["limits"][name], json!(actual), "source limit {name}");
    }
    let mut records = Vec::new();
    let mut decoded = 0;
    let mut rejected = 0;
    for file in [
        "answers",
        "commands",
        "controls",
        "messaging",
        "wire",
        "workspaces",
    ] {
        let text = fs::read_to_string(root.join(format!("{file}.jsonl"))).unwrap();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let case: Value = serde_json::from_str(line).expect("JSONL case");
            let id = case["id"].as_str().unwrap();
            let input = &case["input"];
            let expected = &case["expected"];
            if case["layer"] == "wire" {
                let result = serde_json::from_value::<Request>(input.clone());
                if expected["outcome"] == "decode_error" {
                    assert!(result.is_err(), "{id}: expected invalid request");
                    rejected += 1;
                } else {
                    let request = result.unwrap_or_else(|error| panic!("{id}: {error}"));
                    assert_eq!(
                        serde_json::to_value(&request.call).unwrap(),
                        expected["call"],
                        "{id}: defaults and canonical Call shape"
                    );
                    records.push(record(id, 0, request));
                    decoded += 1;
                }
                continue;
            }
            if input.get("method").is_some() {
                records.push(record(id, 0, with_envelope(input)));
            } else if let Some(call) = expected.get("call") {
                records.push(record(id, 0, with_envelope(call)));
            } else if let Some(calls) = expected["calls"].as_array() {
                for (index, call) in calls.iter().enumerate() {
                    records.push(record(id, index, with_envelope(call)));
                }
            }
        }
    }
    let golden = root.join("request-bytes.jsonl");
    if write {
        let lines: Vec<String> = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect();
        fs::write(&golden, format!("{}\n", lines.join("\n"))).unwrap();
    } else {
        let expected: Vec<Value> = fs::read_to_string(&golden)
            .expect("request-bytes.jsonl; generate deliberately with --write-wire")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records, expected, "canonical Request bytes changed");
    }
    if print {
        for wanted in [
            "compose-direct",
            "wire-resume-defaults",
            "wire-models-empty-object",
        ] {
            let entry = records.iter().find(|r| r["case_id"] == wanted).unwrap();
            println!("{wanted}: {}", entry["json"].as_str().unwrap());
        }
    }
    println!(
        "{} canonical Request byte records {}; {decoded} valid and {rejected} invalid wire cases checked.",
        records.len(),
        if write { "written" } else { "matched" }
    );
}
