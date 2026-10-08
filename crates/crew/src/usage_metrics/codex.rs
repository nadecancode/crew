//! Codex rollouts (`sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`).
//!
//! Codex reports cumulative counters in `token_count` events. An unchanged
//! vector is a duplicate. Otherwise the increment over the previous known
//! vector is the new usage; a field that first becomes known seeds its own
//! baseline with a gap instead of charging its lifetime total. The increment
//! counts as one request only when it equals the direct `last_token_usage`
//! on every field, unknown fields included, with input and output known;
//! anything else is a native batch of unknown cardinality. A decrease starts
//! a gap, and closes charging for the rest of the file generation: a direct
//! last-request vector equal to the new cumulative does not prove a new
//! occurrence, and neither do later increments, since old records appended
//! again (all of them or a subset) look the same. A new file generation
//! reseeds and charges what is appended after it. Output already includes
//! reasoning, which is never added again.
//!
//! An increment is identified by the vector it starts from and the one it
//! reaches, not by its epoch, so the same increment read again (appended
//! again, after a reset, after a rewrite or a restart) is the same
//! observation and is never charged twice. An increment is never revised:
//! the same edge read again with another model or unit is a conflicting
//! replay. Two genuinely distinct increments with the same start and end
//! would be counted once: the error is an undercount, never a double charge.

use crew_core::rpc::usage_metrics::{CoverageReason, ObservationUnit};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::counters::{self, field};
use super::source::{
    self, Decoder, Item, Line, Observation, Phase, Signal, Skip, Stamp, Start, Str,
};

/// One cumulative or direct vector, as reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vector {
    pub input: Option<u64>,
    pub cached: Option<u64>,
    pub write: Option<u64>,
    pub output: Option<u64>,
}

impl Vector {
    fn read(u: &Usage) -> (Self, bool) {
        let mut valid = true;
        let mut get = |v: &Option<Value>| match field(v.as_ref()) {
            Ok(n) => n,
            Err(_) => {
                valid = false;
                None
            }
        };
        let vector = Self {
            input: get(&u.input_tokens),
            cached: get(&u.cached_input_tokens),
            write: get(&u.cache_write_input_tokens),
            output: get(&u.output_tokens),
        };
        (vector, valid)
    }

    fn fields(self) -> [Option<u64>; 4] {
        [self.input, self.cached, self.write, self.output]
    }

    fn decreased_from(self, previous: Self) -> bool {
        self.fields()
            .iter()
            .zip(previous.fields())
            .any(|(now, before)| matches!((now, before), (Some(a), Some(b)) if *a < b))
    }

    /// The increment over `previous`, and whether a field first became known.
    fn since(self, previous: Self) -> (Self, bool) {
        let mut seeded = false;
        let mut part = |now: Option<u64>, before: Option<u64>| match (now, before) {
            (Some(a), Some(b)) => Some(a - b),
            (Some(_), None) => {
                seeded = true;
                None
            }
            (None, _) => None,
        };
        let delta = Self {
            input: part(self.input, previous.input),
            cached: part(self.cached, previous.cached),
            write: part(self.write, previous.write),
            output: part(self.output, previous.output),
        };
        (delta, seeded)
    }

    fn is_empty(self) -> bool {
        self.fields().iter().all(Option::is_none)
    }

    /// Exactly one request: equal to the direct vector on every field,
    /// unknown fields included, with input and output known.
    fn is_single(self, last: Self) -> bool {
        self == last && self.input.is_some() && self.output.is_some()
    }

    fn key(self) -> String {
        let f = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
        format!(
            "{}/{}/{}/{}",
            f(self.input),
            f(self.cached),
            f(self.write),
            f(self.output)
        )
    }
}

/// Decoder state for one Codex rollout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Codex {
    session: Option<String>,
    /// The rollout's `session_meta` named another session.
    foreign: bool,
    epoch: u32,
    previous: Option<Vector>,
    /// The next cumulative vector seeds instead of being charged.
    seed_next: bool,
    model: Option<String>,
    turn: Option<String>,
    /// The model or turn changed since the previous cumulative vector.
    model_changed: bool,
    turn_changed: bool,
    last_ms: Option<i64>,
    /// The counters went down in this file generation. No later increment
    /// of the generation can be told apart from an old record appended
    /// again, so none is charged.
    #[serde(default)]
    closed: bool,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    timestamp: Stamp,
    #[serde(default)]
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    id: Str,
    #[serde(default)]
    model: Str,
    #[serde(default)]
    turn_id: Str,
    #[serde(default)]
    info: Option<Info>,
}

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    total_token_usage: Option<Usage>,
    #[serde(default)]
    last_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<Value>,
    cached_input_tokens: Option<Value>,
    cache_write_input_tokens: Option<Value>,
    output_tokens: Option<Value>,
}

#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    payload: Option<HeadPayload>,
}

#[derive(Deserialize)]
struct HeadPayload {
    #[serde(rename = "type", default)]
    kind: Str,
}

const NEEDLES: &[&[u8]] = &[
    b"token_count",
    b"turn_context",
    b"session_meta",
    b"compacted",
];

impl Codex {
    pub fn new(session: &str, start: Start) -> Self {
        Self {
            session: source::ident(session),
            foreign: false,
            epoch: 0,
            previous: None,
            seed_next: start == Start::Existing,
            model: None,
            turn: None,
            model_changed: false,
            turn_changed: false,
            last_ms: None,
            closed: false,
        }
    }

    fn context(&mut self, payload: &Payload) {
        let model = payload.model.as_deref().and_then(source::model);
        if model.is_some() && model != self.model {
            self.model_changed |= self.previous.is_some();
            self.model = model;
        }
        let turn = payload.turn_id.as_deref().and_then(source::ident);
        if turn.is_some() && turn != self.turn {
            self.turn_changed |= self.previous.is_some();
            self.turn = turn;
        }
    }

    fn settle(&mut self, now: Vector) {
        self.previous = Some(now);
        self.seed_next = false;
        self.model_changed = false;
        self.turn_changed = false;
    }

    fn token_count(
        &mut self,
        line: &Line,
        info: &Info,
        native_ms: Option<i64>,
        out: &mut Vec<Item>,
    ) {
        let Some(total) = info.total_token_usage.as_ref() else {
            out.push(Item::Skipped {
                skip: Skip::MissingCumulative,
                offset: line.offset,
            });
            return;
        };
        let (now, now_valid) = Vector::read(total);
        let last = info.last_token_usage.as_ref().map(Vector::read);
        // Another session's vectors neither charge nor seed our baseline.
        if self.foreign {
            if line.phase == Phase::Charge {
                out.push(Item::Skipped {
                    skip: Skip::ForeignSession,
                    offset: line.offset,
                });
            }
            return;
        }
        if line.phase == Phase::Seed {
            if self.previous.is_some_and(|p| now.decreased_from(p)) {
                self.epoch += 1;
            }
            self.settle(now);
            return;
        }
        let gap = |out: &mut Vec<Item>| {
            out.push(Item::Gap {
                reason: CoverageReason::SourceGap,
                offset: line.offset,
            })
        };
        let previous = match self.previous {
            Some(p) if p == now => {
                out.push(Item::Skipped {
                    skip: Skip::Duplicate,
                    offset: line.offset,
                });
                return;
            }
            None if self.seed_next => {
                gap(out);
                self.settle(now);
                return;
            }
            Some(p) if now.decreased_from(p) => {
                self.epoch += 1;
                self.closed = true;
                gap(out);
                self.settle(now);
                return;
            }
            // After a decrease nothing proves a new occurrence: an old
            // increment replayed with others skipped would look new.
            _ if self.closed => {
                out.push(Item::Gap {
                    reason: CoverageReason::AmbiguousBoundary,
                    offset: line.offset,
                });
                self.settle(now);
                return;
            }
            previous => previous,
        };
        let (delta, seeded) = match previous {
            Some(p) => now.since(p),
            // A fresh session: its first cumulative vector is its usage.
            None => (now, false),
        };
        let mut reasons = Vec::new();
        if seeded {
            reasons.push(CoverageReason::SourceGap);
        }
        if !now_valid {
            reasons.push(CoverageReason::InvalidCounter);
        }
        let single = matches!(last, Some((l, true)) if now_valid && delta.is_single(l));
        let unit = if delta.is_empty() {
            ObservationUnit::Unusable
        } else if single {
            ObservationUnit::Request
        } else {
            reasons.push(CoverageReason::NativeBatch);
            ObservationUnit::CounterDelta
        };
        self.emit(line, previous, now, delta, unit, reasons, native_ms, out);
        self.settle(now);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        line: &Line,
        previous: Option<Vector>,
        now: Vector,
        usage: Vector,
        unit: ObservationUnit,
        mut reasons: Vec<CoverageReason>,
        native_ms: Option<i64>,
        out: &mut Vec<Item>,
    ) {
        let single = unit == ObservationUnit::Request;
        // A batch spanning a model change has no single model to price it by.
        let model = if single || !self.model_changed {
            self.model.clone()
        } else {
            None
        };
        if model.is_none() {
            reasons.push(CoverageReason::UnknownModel);
        }
        let turn = if single || !self.turn_changed {
            self.turn.clone()
        } else {
            None
        };
        let gap_ms = source::gap(&mut self.last_ms, native_ms);
        out.push(Item::Observation(Box::new(Observation {
            key: format!(
                "c:{}>{}",
                previous.map_or_else(|| "-".to_string(), Vector::key),
                now.key()
            ),
            unit,
            normalized: counters::codex(
                Ok(usage.input),
                Ok(usage.cached),
                Ok(usage.write),
                Ok(usage.output),
            ),
            model,
            request_id: None,
            turn,
            native_ms,
            gap_ms,
            generation: line.generation,
            offset: line.offset,
            line: line.number,
            phase: line.phase,
            reasons,
            revisable: false,
        })));
    }
}

impl Decoder for Codex {
    fn decode(&mut self, line: &Line, out: &mut Vec<Item>) {
        if !source::mentions(&line.text, NEEDLES) {
            return;
        }
        let parsed = source::within_depth(&line.text).then(|| serde_json::from_slice(&line.text));
        let record: Record = match parsed {
            Some(Ok(r)) => r,
            _ => {
                let counted = serde_json::from_slice::<Head>(&line.text).map_or(true, |h| {
                    let inner = h.payload.as_ref().and_then(|p| p.kind.as_deref());
                    matches!(
                        (h.kind.as_deref(), inner),
                        (Some("session_meta" | "turn_context"), _)
                            | (Some("event_msg"), Some("token_count"))
                    )
                });
                if counted {
                    out.push(Item::Skipped {
                        skip: Skip::Malformed,
                        offset: line.offset,
                    });
                }
                return;
            }
        };
        let native_ms = record.timestamp.0;
        let Some(payload) = record.payload else {
            if record.kind.as_deref() == Some("compacted") && !self.foreign {
                out.push(Item::Signal {
                    signal: Signal::Compaction { native_ms },
                    offset: line.offset,
                });
            }
            return;
        };
        match (record.kind.as_deref(), payload.kind.as_deref()) {
            (Some("session_meta"), _) => {
                // Only a valid id decides ownership; a header without one
                // never clears a rejection.
                if let Some(id) = payload.id.as_deref().and_then(source::ident) {
                    self.foreign = self.session.as_ref().is_some_and(|own| *own != id);
                }
            }
            // Another session's context changes nothing of ours.
            (Some("turn_context"), _) if !self.foreign => self.context(&payload),
            (Some("event_msg"), Some("token_count")) => {
                if let Some(info) = &payload.info {
                    self.token_count(line, info, native_ms, out);
                }
            }
            (Some("compacted"), _) | (Some("event_msg"), Some("context_compacted"))
                if !self.foreign =>
            {
                out.push(Item::Signal {
                    signal: Signal::Compaction { native_ms },
                    offset: line.offset,
                });
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.epoch += 1;
        self.previous = None;
        self.seed_next = true;
        self.model = None;
        self.turn = None;
        self.model_changed = false;
        self.turn_changed = false;
        self.last_ms = None;
        self.closed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_metrics::claude::tests::{decode_all, latest};
    use crew_core::RuntimeKind;
    use serde_json::json;

    fn v(i: u64, r: u64, o: u64) -> Value {
        json!({"input_tokens": i, "cached_input_tokens": r, "output_tokens": o, "reasoning_output_tokens": o, "total_tokens": i + o})
    }

    fn tc(ms: i64, total: Value, last: Option<Value>) -> Value {
        let mut info = json!({"total_token_usage": total});
        if let Some(last) = last {
            info["last_token_usage"] = last;
        }
        json!({"type": "event_msg", "timestamp": chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339(),
            "payload": {"type": "token_count", "info": info, "rate_limits": {"primary": {"used_percent": 3.0}}}})
    }

    fn meta(id: &str) -> Value {
        json!({"type": "session_meta", "payload": {"id": id, "cwd": "/private", "base_instructions": {"text": "PRIVATE PROMPT"}}})
    }

    fn turn(model: &str, id: &str) -> Value {
        json!({"type": "turn_context", "payload": {"model": model, "turn_id": id}})
    }

    fn observations(items: &[Item]) -> Vec<Observation> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Observation(o) => Some((**o).clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn duplicates_deduplicate_and_matching_increments_are_requests() {
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                meta("th-1"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
                tc(1, v(100, 80, 10), Some(v(100, 80, 10))),
                tc(2, v(150, 120, 17), Some(v(50, 40, 7))),
            ],
        );
        let obs = observations(&items);
        assert_eq!(obs.len(), 2);
        assert!(obs.iter().all(|o| o.unit == ObservationUnit::Request));
        let c = &obs[1].normalized.counters;
        assert_eq!(
            (c.input_total, c.cache_read, c.output),
            (Some(50), Some(40), Some(7))
        );
        assert_eq!(c.cache_write, None, "absent writes stay unknown");
        assert_eq!(c.input_uncached, None);
        assert_eq!(obs[1].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(obs[1].turn.as_deref(), Some("t1"));
        assert_eq!(obs[1].key, "c:100/80/-/10>150/120/-/17");
        assert!(items.contains(&Item::Skipped {
            skip: Skip::Duplicate,
            offset: obs[0].offset
                + 1
                + tc(0, v(100, 80, 10), Some(v(100, 80, 10)))
                    .to_string()
                    .len() as u64,
        }));
    }

    #[test]
    fn intersection_only_matches_and_missing_direct_vectors_are_batches() {
        let partial = json!({"input_tokens": 50});
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
                // Agrees on the one field both know: not proof.
                tc(1, v(150, 120, 17), Some(partial)),
                tc(2, v(200, 150, 20), None),
                // Two requests reported at once.
                tc(3, v(300, 200, 30), Some(v(60, 30, 5))),
            ],
        );
        let units: Vec<_> = observations(&items).iter().map(|o| o.unit).collect();
        assert_eq!(
            units,
            [
                ObservationUnit::Request,
                ObservationUnit::CounterDelta,
                ObservationUnit::CounterDelta,
                ObservationUnit::CounterDelta,
            ]
        );
        let batch = &observations(&items)[3];
        assert!(batch.reasons.contains(&CoverageReason::NativeBatch));
        assert_eq!(
            batch.normalized.counters.input_total,
            Some(100),
            "tokens kept as evidence"
        );
    }

    #[test]
    fn a_field_that_first_becomes_known_seeds_without_lifetime_catch_up() {
        let without = json!({"input_tokens": 100, "output_tokens": 10});
        let with = json!({"input_tokens": 150, "cached_input_tokens": 9_000, "output_tokens": 17});
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                turn("gpt-6-luna", "t1"),
                tc(0, without.clone(), Some(without)),
                tc(
                    1,
                    with,
                    Some(
                        json!({"input_tokens": 50, "cached_input_tokens": 40, "output_tokens": 7}),
                    ),
                ),
                tc(
                    2,
                    json!({"input_tokens": 200, "cached_input_tokens": 9_030, "output_tokens": 20}),
                    Some(
                        json!({"input_tokens": 50, "cached_input_tokens": 30, "output_tokens": 3}),
                    ),
                ),
            ],
        );
        let obs = observations(&items);
        let seeded = &obs[1];
        assert_eq!(
            seeded.normalized.counters.cache_read, None,
            "no 9,000-token catch-up"
        );
        assert_eq!(seeded.unit, ObservationUnit::CounterDelta);
        assert!(seeded.reasons.contains(&CoverageReason::SourceGap));
        assert_eq!(obs[2].unit, ObservationUnit::Request);
        assert_eq!(obs[2].normalized.counters.cache_read, Some(30));
    }

    #[test]
    fn a_decrease_closes_charging_for_the_generation() {
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                turn("gpt-6-luna", "t1"),
                tc(0, v(1_000, 800, 50), Some(v(1_000, 800, 50))),
                // A reset whose cumulative equals the direct vector is not
                // proof of a new request: an old record appended again looks
                // the same.
                tc(1, v(120, 100, 6), Some(v(120, 100, 6))),
                // Rising counters after it prove nothing either.
                tc(2, v(150, 120, 9), Some(v(30, 20, 3))),
                tc(3, v(90, 10, 4), Some(v(30, 10, 2))),
            ],
        );
        let obs = observations(&items);
        assert_eq!(obs.len(), 1, "only the first increment");
        let gap = |reason| {
            items
                .iter()
                .filter(|i| matches!(i, Item::Gap { reason: r, .. } if *r == reason))
                .count()
        };
        // Each decrease is a source gap; a rise after one is ambiguous.
        assert_eq!(gap(CoverageReason::SourceGap), 2);
        assert_eq!(gap(CoverageReason::AmbiguousBoundary), 1);
    }

    #[test]
    fn a_partial_replay_after_a_decrease_is_not_charged() {
        let a = tc(0, v(100, 80, 10), Some(v(100, 80, 10)));
        let b = tc(1, v(150, 120, 15), Some(v(50, 40, 5)));
        let c = tc(2, v(200, 160, 20), Some(v(50, 40, 5)));
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[turn("gpt-6-luna", "t1"), a.clone(), b, c.clone(), a, c],
        );
        let total: u64 = latest(&items)
            .values()
            .filter_map(|o| o.normalized.counters.input_total)
            .sum();
        assert_eq!(
            total, 200,
            "A, B and C once; the replayed A..C edge is not new"
        );
    }

    #[test]
    fn an_old_token_count_appended_again_is_not_charged_again() {
        let a = tc(0, v(100, 100, 10), Some(v(100, 100, 10)));
        let b = tc(1, v(150, 140, 15), Some(v(50, 40, 5)));
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[turn("gpt-6-luna", "t1"), a.clone(), b.clone(), a, b],
        );
        let rows = latest(&items);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let total: u64 = rows
            .values()
            .filter_map(|o| o.normalized.counters.input_total)
            .sum();
        assert_eq!(total, 150);
    }

    #[test]
    fn explicit_write_zero_reasoning_and_model_changes() {
        let with_write = |i: u64, r: u64, w: u64, o: u64| json!({"input_tokens": i, "cached_input_tokens": r, "cache_write_input_tokens": w, "output_tokens": o, "reasoning_output_tokens": 999});
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                turn("gpt-6-luna", "t1"),
                tc(
                    0,
                    with_write(100, 80, 0, 10),
                    Some(with_write(100, 80, 0, 10)),
                ),
                turn("gpt-6.1-sol", "t2"),
                tc(
                    1,
                    with_write(300, 200, 0, 30),
                    Some(with_write(90, 60, 0, 10)),
                ),
            ],
        );
        let obs = observations(&items);
        assert_eq!(
            obs[0].normalized.counters.cache_write,
            Some(0),
            "explicit zero kept"
        );
        assert_eq!(obs[0].normalized.counters.input_uncached, Some(20));
        assert_eq!(
            obs[0].normalized.counters.output,
            Some(10),
            "reasoning is inside output"
        );
        assert_eq!(obs[1].unit, ObservationUnit::CounterDelta);
        assert_eq!(
            obs[1].model, None,
            "a batch across a model change is unpriced"
        );
        assert_eq!(obs[1].turn, None);
        assert!(obs[1].reasons.contains(&CoverageReason::UnknownModel));
    }

    #[test]
    fn existing_sessions_reseed_and_foreign_rollouts_are_not_charged() {
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                meta("other"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(10, 5, 1), Some(v(10, 5, 1))),
            ],
        );
        assert!(observations(&items).is_empty());
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Skipped {
                skip: Skip::ForeignSession,
                ..
            }
        )));

        // Without a seed prefix, an existing session's first vector seeds.
        let mut codex = Codex::new("th-1", Start::Existing);
        let mut out = Vec::new();
        for (n, value) in [
            turn("gpt-6-luna", "t1"),
            tc(0, v(5_000, 4_000, 100), Some(v(10, 5, 1))),
            tc(1, v(5_010, 4_005, 101), Some(v(10, 5, 1))),
        ]
        .iter()
        .enumerate()
        {
            let line = Line {
                generation: 0,
                offset: n as u64 * 100,
                number: n as u64 + 1,
                phase: Phase::Charge,
                text: value.to_string().into_bytes(),
            };
            codex.decode(&line, &mut out);
        }
        let obs = observations(&out);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].normalized.counters.input_total, Some(10));
        assert_eq!(obs[0].unit, ObservationUnit::Request);
        let kept = format!("{out:?}{}", serde_json::to_string(&codex).unwrap());
        assert!(
            !kept.contains("PRIVATE") && !kept.contains("private"),
            "{kept}"
        );
        let _ = latest(&out);
    }

    #[test]
    fn a_replaced_source_reseeds_without_charging_its_prefix() {
        use crate::usage_metrics::claude::tests::assistant;
        use crate::usage_metrics::source::{read, Adapter, Checkpoint, Start};
        use std::fs::File;

        let lines =
            |values: &[Value]| -> String { values.iter().map(|v| format!("{v}\n")).collect() };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let first = [
            turn("gpt-6-luna", "t1"),
            tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
        ];
        std::fs::write(&path, lines(&first)).unwrap();
        let f = File::open(&path).unwrap();
        let mut adapter = Adapter::new(RuntimeKind::Codex, "th-1", Start::Fresh).unwrap();
        let batch = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap()).unwrap();
        assert_eq!(observations(&adapter.decode(&batch)).len(), 1);

        // The same history plus a request, written as a new file under the name.
        let mut second = first.to_vec();
        second.push(tc(1, v(150, 120, 17), Some(v(50, 40, 7))));
        let tmp = dir.path().join("new.jsonl");
        std::fs::write(&tmp, lines(&second)).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let replaced = read(&File::open(&path).unwrap(), &batch.checkpoint).unwrap();
        let items = adapter.decode(&replaced);
        assert!(replaced.loss.is_some());
        assert!(
            observations(&items).is_empty(),
            "the ambiguous prefix is not charged"
        );
        assert!(items.iter().any(|i| matches!(i, Item::Gap { .. })));

        // Verified new increments after it are.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(
            &mut f,
            lines(&[tc(2, v(210, 170, 20), Some(v(60, 50, 3)))]).as_bytes(),
        )
        .unwrap();
        let next = read(&File::open(&path).unwrap(), &replaced.checkpoint).unwrap();
        let obs = observations(&adapter.decode(&next));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].normalized.counters.input_total, Some(60));
        assert_eq!(obs[0].unit, ObservationUnit::Request);

        // A rewritten Claude transcript replays its message ids only as seed
        // evidence, for the ledger to reconcile against what it holds.
        let path = dir.path().join("claude.jsonl");
        let a = assistant(
            "a",
            1,
            json!({"input_tokens": 1, "cache_read_input_tokens": 2, "cache_creation_input_tokens": 3, "output_tokens": 4}),
        );
        std::fs::write(&path, lines(std::slice::from_ref(&a))).unwrap();
        let f = File::open(&path).unwrap();
        let mut claude = Adapter::new(RuntimeKind::Claude, "s-1", Start::Fresh).unwrap();
        let batch = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap()).unwrap();
        claude.decode(&batch);
        let tmp = dir.path().join("claude-new.jsonl");
        std::fs::write(&tmp, lines(&[a])).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let replayed = read(&File::open(&path).unwrap(), &batch.checkpoint).unwrap();
        let obs = observations(&claude.decode(&replayed));
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].key, "m:a");
        assert_eq!(obs[0].phase, Phase::Seed);
    }

    #[test]
    fn another_sessions_headers_and_context_change_nothing() {
        let items = decode_all(
            RuntimeKind::Codex,
            "th-1",
            &[
                meta("th-1"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
                meta("other"),
                turn("gpt-6.1-sol", "t-other"),
                // A header without a valid id keeps the rejection.
                json!({"type": "session_meta", "payload": {"id": "has space"}}),
                json!({"type": "session_meta", "payload": {}}),
                tc(1, v(10, 5, 1), Some(v(10, 5, 1))),
                meta("th-1"),
                tc(2, v(150, 120, 17), Some(v(50, 40, 7))),
            ],
        );
        let obs = observations(&items);
        assert_eq!(obs.len(), 2);
        assert_eq!(
            obs[1].model.as_deref(),
            Some("gpt-6-luna"),
            "not the foreign model"
        );
        assert_eq!(obs[1].turn.as_deref(), Some("t1"));
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Skipped {
                skip: Skip::ForeignSession,
                ..
            }
        )));
    }

    #[test]
    fn a_foreign_seed_prefix_does_not_move_the_baseline_and_foreign_compactions_are_silent() {
        use crate::usage_metrics::source::{read, Adapter, Checkpoint};
        use std::fs::File;
        use std::io::Write;

        let lines =
            |values: &[Value]| -> String { values.iter().map(|v| format!("{v}\n")).collect() };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        // Existing prefix: own total 100, then another session's total 0.
        std::fs::write(
            &path,
            lines(&[
                meta("th-1"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
                meta("other"),
                tc(1, v(0, 0, 0), Some(v(0, 0, 0))),
                json!({"type": "compacted", "payload": null}),
                json!({"type": "event_msg", "payload": {"type": "context_compacted"}}),
            ]),
        )
        .unwrap();
        let f = File::open(&path).unwrap();
        let mut adapter = Adapter::new(RuntimeKind::Codex, "th-1", Start::Existing).unwrap();
        let cp = Checkpoint::new(&f, Start::Existing).unwrap();
        let seeded = adapter.decode(&read(&f, &cp).unwrap());
        assert!(
            seeded.iter().all(|i| !matches!(i, Item::Signal { .. })),
            "{seeded:?}"
        );
        let next_cp = read(&f, &cp).unwrap().checkpoint;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(
                lines(&[
                    json!({"type": "compacted", "payload": null}),
                    meta("th-1"),
                    tc(2, v(150, 120, 17), Some(v(50, 40, 7))),
                ])
                .as_bytes(),
            )
            .unwrap();
        let items = adapter.decode(&read(&File::open(&path).unwrap(), &next_cp).unwrap());
        let obs = observations(&items);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].normalized.counters.input_total, Some(50), "not 150");
        assert_eq!(obs[0].unit, ObservationUnit::Request);
        assert!(
            items.iter().all(|i| !matches!(i, Item::Signal { .. })),
            "a compaction while foreign is not ours"
        );
    }

    #[test]
    fn a_child_rollout_is_identified_by_its_own_thread_id_not_the_inherited_session_id() {
        // Codex 0.159 child rollouts (crates/crew/tests/fixtures/codex-child):
        // session_meta.id names the child, session_id is the parent's.
        let child_meta = |id: &str, session: &str| json!({"type": "session_meta", "payload": {"id": id, "session_id": session}});
        let own = decode_all(
            RuntimeKind::Codex,
            "child-1",
            &[
                child_meta("child-1", "parent-1"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
            ],
        );
        assert_eq!(observations(&own).len(), 1);
        let as_parent = decode_all(
            RuntimeKind::Codex,
            "parent-1",
            &[
                child_meta("child-1", "parent-1"),
                turn("gpt-6-luna", "t1"),
                tc(0, v(100, 80, 10), Some(v(100, 80, 10))),
            ],
        );
        assert!(
            observations(&as_parent).is_empty(),
            "the parent is not charged the child's usage"
        );
    }
}
