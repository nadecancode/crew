//! Permanent consumers of the reviewed language-neutral N1 cases.
//!
//! The adapter supplies only N1's task list and caller time. It replaces the
//! task input on snapshots and replaces/appends task inputs on task frames;
//! agent inputs identify callers but do not select work. Production
//! `current_work` supplies every work observation. Agent presentation,
//! selection, history and full reducer conformance belong to later engine tests.

use std::collections::{BTreeMap, BTreeSet};

use crew_client::current_work;
use crew_protocol::{Agent, Task};
use serde_json::{json, Value};

const WORK_CHOICE: &str = include_str!("../../../fixtures/native/state/work-choice.case.json");
const PARKED_RESUMPTION: &str =
    include_str!("../../../fixtures/native/state/parked-resumption.case.json");
const COMPLETED_TIES: &str =
    include_str!("../../../fixtures/native/state/completed-ties.case.json");

#[derive(Default)]
struct TaskInputs {
    tasks: Vec<Task>,
    callers: BTreeSet<String>,
    clock_offset_ms: i64,
}

impl TaskInputs {
    fn apply(&mut self, step: &Value, at_ms: i64) -> Result<(), String> {
        match (step.get("snapshot"), step.get("frame")) {
            (Some(snapshot), None) => {
                self.tasks = serde_json::from_value(snapshot["tasks"].clone())
                    .map_err(|e| format!("snapshot task inputs: {e}"))?;
                let agents: Vec<Agent> = serde_json::from_value(snapshot["agents"].clone())
                    .map_err(|e| format!("snapshot caller inputs: {e}"))?;
                self.callers = agents.into_iter().map(|agent| agent.id).collect();
                let server_time = snapshot["server_time"].as_i64().ok_or("server_time")?;
                self.clock_offset_ms = server_time.checked_sub(at_ms).ok_or("clock offset")?;
            }
            (None, Some(frame)) => {
                let push = &frame["push"];
                match push["kind"].as_str() {
                    Some("task") => {
                        let task: Task = serde_json::from_value(push["data"].clone())
                            .map_err(|e| format!("task input: {e}"))?;
                        if let Some(slot) = self.tasks.iter_mut().find(|t| t.id == task.id) {
                            *slot = task;
                        } else {
                            self.tasks.push(task);
                        }
                    }
                    Some("agent") => {
                        let agent: Agent = serde_json::from_value(push["data"].clone())
                            .map_err(|e| format!("caller input: {e}"))?;
                        self.callers.insert(agent.id);
                    }
                    other => return Err(format!("input outside N1 adapter: {other:?}")),
                }
            }
            _ => return Err("exactly one snapshot or frame is required".into()),
        }
        Ok(())
    }

    fn observe(&self, at_ms: i64) -> Result<Value, String> {
        let server_now_ms = at_ms
            .checked_add(self.clock_offset_ms)
            .ok_or("server time")?;
        // Query callers from wire input, not from the expected work outputs.
        let mut callers = self.callers.clone();
        callers.extend(
            self.tasks
                .iter()
                .filter_map(|task| task.assigned_agent.clone()),
        );
        let work: BTreeMap<_, _> = callers
            .into_iter()
            .map(|agent| {
                let observation = current_work(&self.tasks, &agent, server_now_ms).map(|work| {
                    json!({
                        "task_id": work.task_id,
                        "done": work.done,
                        "elapsed_ms": work.elapsed_ms,
                    })
                });
                (agent, observation)
            })
            .collect();
        let tasks: BTreeMap<_, _> = self.tasks.iter().map(|task| (&task.id, task)).collect();
        Ok(json!({
            "task_ids": self.tasks.iter().map(|task| &task.id).collect::<Vec<_>>(),
            "tasks": tasks,
            "work": work,
        }))
    }
}

fn subset(actual: &Value, expected: &Value, path: &str) -> Result<(), String> {
    match expected {
        Value::Object(fields) => {
            let object = actual
                .as_object()
                .ok_or_else(|| format!("{path}: not an object"))?;
            for (key, expected) in fields {
                let actual = object
                    .get(key)
                    .ok_or_else(|| format!("{path}.{key}: missing"))?;
                subset(actual, expected, &format!("{path}.{key}"))?;
            }
        }
        Value::Array(values) => {
            let array = actual
                .as_array()
                .ok_or_else(|| format!("{path}: not an array"))?;
            if array.len() != values.len() {
                return Err(format!(
                    "{path}: array length {} != {}",
                    array.len(),
                    values.len()
                ));
            }
            for (index, (actual, expected)) in array.iter().zip(values).enumerate() {
                subset(actual, expected, &format!("{path}[{index}]"))?;
            }
        }
        _ if actual != expected => {
            return Err(format!("{path}: actual {actual}, expected {expected}"));
        }
        _ => {}
    }
    Ok(())
}

fn replay(case: &Value) -> Result<usize, String> {
    if case["schema_version"] != 1 || case["protocol"] != 6 {
        return Err("unexpected corpus version".into());
    }
    let id = case["id"].as_str().ok_or("case id")?;
    let steps = case["steps"].as_array().ok_or("case steps")?;
    if steps.is_empty() || steps[0].get("snapshot").is_none() {
        return Err("case must start with a snapshot".into());
    }
    let mut inputs = TaskInputs::default();
    for (index, step) in steps.iter().enumerate() {
        let path = format!("{id}/{index}");
        let at_ms = step["at_ms"].as_i64().ok_or("at_ms")?;
        inputs
            .apply(step, at_ms)
            .map_err(|e| format!("{path}: {e}"))?;
        let actual = inputs.observe(at_ms)?;
        let expected = &step["expect"];
        if expected["work"]
            .as_object()
            .is_none_or(|work| work.is_empty())
        {
            return Err(format!("{path}: no work observation"));
        }
        // Common expect is the native/TUI oracle. Legacy web overrides are
        // deliberately never applied here. Only N1-supported fields are read.
        for key in ["work", "task_ids", "tasks"] {
            if let Some(expected) = expected.get(key) {
                subset(&actual[key], expected, &format!("{path}.{key}"))?;
            }
        }
    }
    Ok(steps.len())
}

fn case(text: &str) -> Value {
    serde_json::from_str(text).expect("reviewed fixture JSON")
}

#[test]
fn work_choice_replays_reviewed_inputs_through_the_production_query() {
    assert_eq!(replay(&case(WORK_CHOICE)).unwrap(), 9);
}

#[test]
fn parked_resumption_replay_keeps_identity_metadata_and_selects_original_work() {
    assert_eq!(replay(&case(PARKED_RESUMPTION)).unwrap(), 6);
}

#[test]
fn completed_ties_use_common_native_expectations_with_legacy_web_kept_separate() {
    let case = case(COMPLETED_TIES);
    assert_eq!(replay(&case).unwrap(), 2);
    for step in case["steps"].as_array().unwrap() {
        let legacy = &step["legacy_client_expect"]["web"]["work"];
        assert!(legacy.is_object(), "explicit legacy web expectation");
        assert_ne!(step["expect"]["work"], *legacy);
    }
}

#[test]
fn changed_wire_inputs_fail_the_authored_oracle_without_changing_expectations() {
    let mut work = case(WORK_CHOICE);
    assert_eq!(
        work["steps"][0]["snapshot"]["tasks"][0]["status"],
        "blocked"
    );
    work["steps"][0]["snapshot"]["tasks"][0]["status"] = json!("waiting");
    let failure = replay(&work).unwrap_err();
    assert!(
        failure.contains("work-choice/0.work.claude-1."),
        "{failure}"
    );

    let mut parked = case(PARKED_RESUMPTION);
    assert_eq!(
        parked["steps"][4]["frame"]["push"]["data"]["id"],
        "parked-old"
    );
    parked["steps"][4]["frame"]["push"]["data"]["started_at"] = json!(1_759_999_996_500_i64);
    let failure = replay(&parked).unwrap_err();
    assert!(
        failure.contains("parked-resumption/4.work.claude-1.elapsed_ms"),
        "{failure}"
    );

    let mut completed = case(COMPLETED_TIES);
    assert_eq!(
        completed["steps"][0]["snapshot"]["tasks"][1]["id"],
        "done-second"
    );
    completed["steps"][0]["snapshot"]["tasks"][1]["completed_at"] = json!(500);
    let failure = replay(&completed).unwrap_err();
    assert!(
        failure.contains("completed-ties/0.work.claude-1.task_id"),
        "{failure}"
    );
}
