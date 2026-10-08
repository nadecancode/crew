//! Claude Code transcripts (`projects/<project>/<session>.jsonl`).
//!
//! Usage is keyed by the assistant `message.id`. Claude Code writes a
//! streamed message more than once; each copy is a revision whose counters
//! replace the earlier ones, never add to them. `<synthetic>` messages carry
//! no billed usage. Sidechains stay excluded, with their coverage gap, until
//! a verified child-session contract exists. A message without a stable id
//! is unusable rather than another request.

use crew_core::rpc::usage_metrics::{CoverageReason, ObservationUnit};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::counters::{self, field};
use super::source::{self, Decoder, Flag, Item, Line, Observation, Signal, Skip, Stamp, Str};

/// Decoder state for one Claude Code transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claude {
    /// The native session this stream belongs to.
    session: Option<String>,
    /// Native turns finished so far in this generation.
    turn: u64,
    last_ms: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    subtype: Str,
    #[serde(default)]
    timestamp: Stamp,
    #[serde(default)]
    session_id: Str,
    #[serde(default)]
    request_id: Str,
    #[serde(default)]
    is_sidechain: Flag,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    attachment: Option<Attachment>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    id: Str,
    #[serde(default)]
    model: Str,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<Value>,
    cache_read_input_tokens: Option<Value>,
    cache_creation_input_tokens: Option<Value>,
    output_tokens: Option<Value>,
    #[serde(default)]
    cache_creation: Option<Buckets>,
}

#[derive(Deserialize)]
struct Buckets {
    ephemeral_5m_input_tokens: Option<Value>,
    ephemeral_1h_input_tokens: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    request_id: Str,
    #[serde(default)]
    client_change: Option<ClientChange>,
    #[serde(default)]
    newly_dropped: Option<Dropped>,
}

#[derive(Deserialize)]
struct ClientChange {
    #[serde(default)]
    kinds: Option<Kinds>,
}

/// `kinds` is a list or a separated string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Kinds {
    Text(String),
    List(Vec<Str>),
}

#[derive(Deserialize)]
struct Dropped {
    #[serde(default)]
    reason: Str,
}

/// The parts every record has, read when a full decode fails: whether the
/// failed record was one this adapter counts.
#[derive(Deserialize)]
struct Head {
    #[serde(rename = "type", default)]
    kind: Str,
    #[serde(default)]
    subtype: Str,
}

const NEEDLES: &[&[u8]] = &[
    b"\"usage\"",
    b"turn_duration",
    b"compact_boundary",
    b"thinking_drop",
];

impl Claude {
    pub fn new(session: &str) -> Self {
        Self {
            session: source::ident(session),
            turn: 0,
            last_ms: None,
        }
    }

    fn history_changed(a: &Attachment) -> Option<String> {
        if a.kind.as_deref() != Some("thinking_drop") {
            return None;
        }
        let changed = match a.client_change.as_ref()?.kinds.as_ref()? {
            Kinds::Text(s) => s.split([',', ' ']).any(|k| k == "messagesHistoryChanged"),
            Kinds::List(l) => l
                .iter()
                .any(|k| k.as_deref() == Some("messagesHistoryChanged")),
        };
        let mismatch = a.newly_dropped.as_ref()?.reason.as_deref() == Some("prefix_mismatch");
        (changed && mismatch)
            .then(|| a.request_id.as_deref().and_then(source::ident))
            .flatten()
    }
}

impl Decoder for Claude {
    fn decode(&mut self, line: &Line, out: &mut Vec<Item>) {
        if !source::mentions(&line.text, NEEDLES) {
            return;
        }
        let parsed = source::within_depth(&line.text).then(|| serde_json::from_slice(&line.text));
        let record: Record = match parsed {
            Some(Ok(r)) => r,
            _ => {
                let counted = serde_json::from_slice::<Head>(&line.text).map_or(true, |h| {
                    matches!(
                        (h.kind.as_deref(), h.subtype.as_deref()),
                        (Some("assistant"), _)
                            | (Some("system"), Some("turn_duration" | "compact_boundary"))
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
        // Ownership first: a sidechain's or another session's records change
        // no state, control records included.
        let usage = record.kind.as_deref() == Some("assistant")
            && record.message.as_ref().is_some_and(|m| m.usage.is_some());
        let foreign = if record.is_sidechain.0 == Some(true) {
            Some(Skip::Sidechain)
        } else if record
            .session_id
            .as_deref()
            .is_some_and(|s| self.session.as_deref() != Some(s))
        {
            Some(Skip::ForeignSession)
        } else {
            None
        };
        if let Some(skip) = foreign {
            if usage {
                out.push(Item::Skipped {
                    skip,
                    offset: line.offset,
                });
            }
            return;
        }
        match (record.kind.as_deref(), record.subtype.as_deref()) {
            (Some("system"), Some("turn_duration")) => {
                out.push(Item::Signal {
                    signal: Signal::TurnEnd {
                        turn: self.turn.to_string(),
                        native_ms,
                    },
                    offset: line.offset,
                });
                self.turn += 1;
                return;
            }
            (Some("system"), Some("compact_boundary")) => {
                out.push(Item::Signal {
                    signal: Signal::Compaction { native_ms },
                    offset: line.offset,
                });
                return;
            }
            _ => {}
        }
        if let Some(request_id) = record.attachment.as_ref().and_then(Self::history_changed) {
            out.push(Item::Signal {
                signal: Signal::HistoryChanged { request_id },
                offset: line.offset,
            });
            return;
        }
        if record.kind.as_deref() != Some("assistant") {
            return;
        }
        let Some(message) = record.message else {
            return;
        };
        let Some(usage) = message.usage else {
            return;
        };
        if message.model.as_deref() == Some("<synthetic>") {
            out.push(Item::Skipped {
                skip: Skip::Synthetic,
                offset: line.offset,
            });
            return;
        }
        let buckets = usage.cache_creation.as_ref();
        let normalized = counters::claude(
            field(usage.input_tokens.as_ref()),
            field(usage.cache_read_input_tokens.as_ref()),
            field(usage.cache_creation_input_tokens.as_ref()),
            field(buckets.and_then(|b| b.ephemeral_5m_input_tokens.as_ref())),
            field(buckets.and_then(|b| b.ephemeral_1h_input_tokens.as_ref())),
            field(usage.output_tokens.as_ref()),
        );
        let (key, unit, reasons) = match message.id.as_deref().and_then(source::ident) {
            Some(id) => (format!("m:{id}"), ObservationUnit::Request, Vec::new()),
            None => (
                format!("l:{}:{}", line.generation, line.offset),
                ObservationUnit::Unusable,
                vec![CoverageReason::MissingRequestIdentity],
            ),
        };
        let mut reasons = reasons;
        let model = message.model.as_deref().and_then(source::model);
        if model.is_none() {
            reasons.push(CoverageReason::UnknownModel);
        }
        let gap_ms = source::gap(&mut self.last_ms, native_ms);
        out.push(Item::Observation(Box::new(Observation {
            key,
            unit,
            normalized,
            model,
            request_id: record.request_id.as_deref().and_then(source::ident),
            turn: Some(self.turn.to_string()),
            native_ms,
            gap_ms,
            generation: line.generation,
            offset: line.offset,
            line: line.number,
            phase: line.phase,
            reasons,
        })));
    }

    fn reset(&mut self) {
        self.turn = 0;
        self.last_ms = None;
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::usage_metrics::source::tests::file;
    use crate::usage_metrics::source::{read, Adapter, Checkpoint, Start};
    use crew_core::RuntimeKind;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs::File;

    pub fn assistant(id: &str, ms: i64, usage: Value) -> Value {
        json!({
            "type": "assistant",
            "timestamp": chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339(),
            "sessionId": "s-1",
            "requestId": format!("req_{id}"),
            "message": {
                "id": id,
                "model": "claude-haiku-4-5-20251001",
                "content": [{"type": "text", "text": "PRIVATE RESPONSE"}],
                "usage": usage,
            },
        })
    }

    fn usage(n: u64, r: u64, w: u64, o: u64) -> Value {
        json!({"input_tokens": n, "cache_read_input_tokens": r, "cache_creation_input_tokens": w, "output_tokens": o})
    }

    pub fn decode_all(runtime: RuntimeKind, session: &str, values: &[Value]) -> Vec<Item> {
        let text: String = values.iter().map(|v| format!("{v}\n")).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "s.jsonl", text.as_bytes());
        let f = File::open(&path).unwrap();
        let mut cp = Checkpoint::new(&f, Start::Fresh).unwrap();
        let mut adapter = Adapter::new(runtime, session, Start::Fresh).unwrap();
        let mut items = Vec::new();
        loop {
            let batch = read(&f, &cp).unwrap();
            items.extend(adapter.decode(&batch));
            let done = batch.caught_up();
            cp = batch.checkpoint;
            if done {
                return items;
            }
        }
    }

    /// The latest revision per key, as the ledger keeps it.
    pub fn latest(items: &[Item]) -> BTreeMap<String, Observation> {
        let mut map = BTreeMap::new();
        for item in items {
            if let Item::Observation(o) = item {
                map.insert(o.key.clone(), (**o).clone());
            }
        }
        map
    }

    #[test]
    fn streamed_copies_replace_counters_and_synthetic_and_sidechain_rows_do_not_count() {
        let mut side = assistant("side", 3, usage(1, 1, 1, 1));
        side["isSidechain"] = json!(true);
        let mut synthetic = assistant("syn", 4, usage(1, 1, 1, 1));
        synthetic["message"]["model"] = json!("<synthetic>");
        let mut late = assistant("a", 2, usage(10, 70, 20, 9));
        late["message"]["usage"]["cache_creation"] =
            json!({"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 20});
        let items = decode_all(
            RuntimeKind::Claude,
            "s-1",
            &[
                assistant("a", 1, usage(10, 70, 20, 5)),
                late,
                side,
                synthetic,
            ],
        );
        let rows = latest(&items);
        assert_eq!(rows.len(), 1);
        let a = &rows["m:a"];
        assert_eq!(a.unit, ObservationUnit::Request);
        assert_eq!(a.normalized.counters.input_total, Some(100));
        assert_eq!(a.normalized.counters.output, Some(9), "the last copy wins");
        assert_eq!(a.normalized.counters.cache_write_1h, Some(20));
        assert_eq!(a.request_id.as_deref(), Some("req_a"));
        let skips: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                Item::Skipped { skip, .. } => Some(*skip),
                _ => None,
            })
            .collect();
        assert_eq!(skips, [Skip::Sidechain, Skip::Synthetic]);
        assert_eq!(
            Skip::Sidechain.reason(),
            Some(CoverageReason::UnsupportedSidechain)
        );
    }

    #[test]
    fn a_later_copy_missing_a_field_or_lower_replaces_without_merging() {
        let mut missing = assistant("a", 2, usage(10, 70, 20, 5));
        missing["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_read_input_tokens");
        let items = decode_all(
            RuntimeKind::Claude,
            "s-1",
            &[assistant("a", 1, usage(10, 70, 20, 50)), missing],
        );
        let a = &latest(&items)["m:a"];
        assert_eq!(a.normalized.counters.cache_read, None, "no stale merge");
        assert_eq!(a.normalized.counters.input_total, None);
        assert_eq!(
            a.normalized.counters.output,
            Some(5),
            "a lower copy is a revision"
        );
    }

    #[test]
    fn missing_identity_is_unusable_and_other_sessions_are_not_charged() {
        let mut anonymous = assistant("x", 1, usage(1, 2, 3, 4));
        anonymous["message"].as_object_mut().unwrap().remove("id");
        let mut copied = assistant("old", 2, usage(1, 2, 3, 4));
        copied["sessionId"] = json!("parent");
        let items = decode_all(RuntimeKind::Claude, "s-1", &[anonymous, copied]);
        let rows = latest(&items);
        let only = rows.values().next().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(only.unit, ObservationUnit::Unusable);
        assert_eq!(only.reasons, [CoverageReason::MissingRequestIdentity]);
        assert!(items.iter().any(|i| matches!(
            i,
            Item::Skipped {
                skip: Skip::ForeignSession,
                ..
            }
        )));
    }

    #[test]
    fn turns_follow_native_boundaries_and_history_diagnostics_are_strict() {
        let drop = |request: &str, kinds: Value, reason: &str| {
            json!({"type": "attachment", "timestamp": "2026-10-08T00:00:00Z",
                "attachment": {"type": "thinking_drop", "requestId": request,
                    "clientChange": {"kinds": kinds}, "newlyDropped": {"reason": reason}}})
        };
        let end = json!({"type": "system", "subtype": "turn_duration", "timestamp": "2026-10-08T00:00:00Z"});
        let items = decode_all(
            RuntimeKind::Claude,
            "s-1",
            &[
                assistant("a", 5_000, usage(1, 1, 1, 1)),
                end.clone(),
                drop("req_b", json!("messagesHistoryChanged"), "prefix_mismatch"),
                assistant("b", 1_000, usage(1, 1, 1, 1)),
                drop("req_c", json!(["none"]), "prefix_mismatch"),
                drop("req_c", json!(["messagesHistoryChanged"]), "other"),
                assistant("c", 4_000, usage(1, 1, 1, 1)),
            ],
        );
        let rows = latest(&items);
        assert_eq!(rows["m:a"].turn.as_deref(), Some("0"));
        assert_eq!(rows["m:b"].turn.as_deref(), Some("1"));
        assert_eq!(rows["m:b"].gap_ms, None, "time went backwards");
        assert_eq!(rows["m:c"].gap_ms, Some(3_000));
        let changed: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                Item::Signal {
                    signal: Signal::HistoryChanged { request_id },
                    ..
                } => Some(request_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(changed, ["req_b"]);
    }

    #[test]
    fn malformed_counted_records_are_reported_and_bodies_are_never_kept() {
        let text = format!(
            "{}\n{}\n{}\n",
            r#"{"type":"assistant","message":"usage"}"#,
            r#"{"type":"user","usage":1} trailing"#,
            assistant("a", 1, usage(1, 2, 3, 4)),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "s.jsonl", text.as_bytes());
        let f = File::open(&path).unwrap();
        let mut adapter = Adapter::new(RuntimeKind::Claude, "s-1", Start::Fresh).unwrap();
        let batch = read(&f, &Checkpoint::new(&f, Start::Fresh).unwrap()).unwrap();
        let items = adapter.decode(&batch);
        let malformed = items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    Item::Skipped {
                        skip: Skip::Malformed,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(malformed, 2, "an unparseable line may have been usage");
        let kept = format!(
            "{items:?}{}",
            String::from_utf8(adapter.state().unwrap()).unwrap()
        );
        assert!(!kept.contains("PRIVATE"), "{kept}");
    }

    #[test]
    fn other_sessions_and_sidechains_change_no_state() {
        let end = |session: &str, sidechain: bool| json!({"type": "system", "subtype": "turn_duration", "sessionId": session, "isSidechain": sidechain});
        let drop = json!({"type": "attachment", "sessionId": "parent",
            "attachment": {"type": "thinking_drop", "requestId": "req_x",
                "clientChange": {"kinds": "messagesHistoryChanged"}, "newlyDropped": {"reason": "prefix_mismatch"}}});
        let items = decode_all(
            RuntimeKind::Claude,
            "s-1",
            &[
                end("parent", false),
                end("s-1", true),
                drop,
                assistant("a", 1, usage(1, 1, 1, 1)),
            ],
        );
        assert!(
            items.iter().all(|i| !matches!(i, Item::Signal { .. })),
            "{items:?}"
        );
        assert_eq!(latest(&items)["m:a"].turn.as_deref(), Some("0"));
    }
}
