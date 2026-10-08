//! Counter parity with the committed lifecycle reports (P09, U01).
//!
//! `crates/crew/tests/fixtures/usage/lifecycle` holds allowlisted extracts of
//! the nine native sessions the reports in `notes/prompt-cache` were measured
//! from (`scripts/cache/usage-fixtures.mjs`). Replaying them through the
//! reader and adapters must reproduce every reported request's counters and
//! every member's totals. Documented differences, all from the report's
//! watchdog parser (`scripts/cache/usage.py`) rather than the native data:
//! it reads absent Claude counters as zero and ignores Codex's reported
//! `cache_write_input_tokens`, so it shows Codex writes as unknown where the
//! rollout reports an explicit zero; and it counts every Codex cumulative
//! change as a request, where an increment here is a request only when it
//! equals the direct `last_token_usage`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;

use crew_core::rpc::features::Wide;
use crew_core::rpc::usage_metrics::ObservationUnit;
use crew_core::RuntimeKind;
use serde_json::Value;

use super::metrics::{totals, Fact};
use super::price::Book;
use super::source::{read, Adapter, Checkpoint, Item, Observation, Start};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every observation of a fixture, latest revision per key, in first-seen order.
fn replay(path: &PathBuf, runtime: RuntimeKind, session: &str) -> Vec<Observation> {
    let file = File::open(path).unwrap();
    let mut cp = Checkpoint::new(&file, Start::Fresh).unwrap();
    let mut adapter = Adapter::new(runtime, session, Start::Fresh).unwrap();
    let mut order = Vec::new();
    let mut latest = BTreeMap::new();
    loop {
        let batch = read(&file, &cp).unwrap();
        for item in adapter.decode(&batch) {
            match item {
                Item::Observation(o) => {
                    if !latest.contains_key(&o.key) {
                        order.push(o.key.clone());
                    }
                    latest.insert(o.key.clone(), *o);
                }
                Item::Skipped { skip, .. } => {
                    assert!(skip.reason().is_none(), "{path:?}: lost usage: {skip:?}")
                }
                Item::Gap { .. } => panic!("{path:?}: unexpected gap"),
                Item::Signal { .. } => {}
            }
        }
        let done = batch.caught_up();
        cp = batch.checkpoint;
        if done {
            break;
        }
    }
    order
        .into_iter()
        .map(|k| latest.remove(&k).unwrap())
        .collect()
}

fn n(v: &Value) -> Option<u64> {
    v.as_u64()
}

#[test]
fn lifecycle_fixtures_reproduce_the_reported_counters() {
    let book = Book::standard_2026_10_08();
    let mut checked = 0;
    for run in ["01", "02", "03"] {
        let report: Value = serde_json::from_slice(
            &std::fs::read(repo().join(format!("notes/prompt-cache/lifecycle-before-{run}.json")))
                .unwrap(),
        )
        .unwrap();
        for agent in report["agents"].as_array().unwrap() {
            let name = agent["agent"].as_str().unwrap();
            let session = agent["session_id"].as_str().unwrap();
            let runtime = match agent["runtime"].as_str().unwrap() {
                "claude" => RuntimeKind::Claude,
                "codex" => RuntimeKind::Codex,
                other => panic!("{other}"),
            };
            let fixture = repo().join(format!(
                "crates/crew/tests/fixtures/usage/lifecycle/{run}-{name}.jsonl"
            ));
            let rows = replay(&fixture, runtime, session);
            let reported = agent["requests"].as_array().unwrap();
            let what = format!("{run} {name}");
            assert_eq!(rows.len(), reported.len(), "{what}: observations");
            assert_eq!(
                reported.len() as u64,
                agent["request_count"].as_u64().unwrap()
            );
            for (i, (row, req)) in rows.iter().zip(reported).enumerate() {
                let c = &row.normalized.counters;
                let at = format!("{what} #{i}");
                assert!(
                    row.normalized.input_valid && row.normalized.output_valid,
                    "{at}"
                );
                assert_eq!(c.input_total, n(&req["input_tokens"]), "{at}: I");
                assert_eq!(c.cache_read, n(&req["cached_tokens"]), "{at}: R");
                assert_eq!(c.output, n(&req["output_tokens"]), "{at}: O");
                assert_eq!(row.model.as_deref(), req["model"].as_str(), "{at}: model");
                match runtime {
                    RuntimeKind::Claude => {
                        assert_eq!(
                            row.key,
                            format!("m:{}", req["message_id"].as_str().unwrap())
                        );
                        assert_eq!(row.unit, ObservationUnit::Request, "{at}");
                        assert_eq!(c.input_uncached, n(&req["uncached_tokens"]), "{at}: N");
                        assert_eq!(c.cache_write, n(&req["write_tokens"]), "{at}: W");
                        assert_eq!(c.cache_write_1h, n(&req["write_1h_tokens"]), "{at}: W1");
                        assert_eq!(row.request_id.as_deref(), req["request_id"].as_str());
                    }
                    _ => {
                        // The watchdog left native writes out; the rollout
                        // reports them, here all explicit zeros.
                        assert!(req["write_tokens"].is_null());
                        assert_eq!(c.cache_write, Some(0), "{at}: W");
                        assert_eq!(c.input_uncached, n(&req["uncached_tokens"]), "{at}: N");
                        assert_eq!(row.unit, ObservationUnit::Request, "{at}: proven single");
                    }
                }
                let native =
                    chrono::DateTime::parse_from_rfc3339(req["timestamp"].as_str().unwrap())
                        .unwrap()
                        .timestamp_millis();
                assert_eq!(row.native_ms, Some(native), "{at}: time of the last copy");
            }
            let facts: Vec<Fact> = rows
                .iter()
                .map(|o| Fact {
                    unit: o.unit,
                    normalized: o.normalized.clone(),
                    model: o.model.clone(),
                    agent: name.into(),
                    source: session.into(),
                    turn: o.turn.clone(),
                    unusable: None,
                })
                .collect();
            let t = totals(&facts, &book, Some(1));
            let wide = |key: &str| Some(Wide(agent[key].as_u64().unwrap() as u128));
            assert_eq!(t.input_total.known_sum, wide("input_tokens"), "{what}: I");
            assert_eq!(t.cache_read.known_sum, wide("cached_tokens"), "{what}: R");
            assert_eq!(t.output.known_sum, wide("output_tokens"), "{what}: O");
            assert_eq!(
                t.requests_observed.known_sum,
                wide("request_count"),
                "{what}"
            );
            assert!(t.requests_observed.coverage.request_cardinality_complete);
            let hit = t.hit_rate.value.unwrap().get();
            assert!(
                (hit - agent["hit_rate"].as_f64().unwrap()).abs() < 1e-12,
                "{what}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 9);
}
