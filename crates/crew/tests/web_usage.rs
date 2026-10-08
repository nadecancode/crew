//! Usage metrics replies for the web dashboard (notes/dashboard-features.md
//! §Usage metrics, F12–F14), computed by the real ledger and queries from
//! native session records, so the page's fixtures are what the daemon would
//! answer. `CREW_UPDATE_FIXTURES=1 cargo test -p crew --test web_usage`
//! rewrites `tests/fixtures/web/usage.json`; the page's tests check its
//! figures against hand computations of the inputs listed in it.
//!
//! Counters are the arithmetic examples of notes/usage-metrics.md scaled by
//! 100,000, so their reference costs read in dollars: Haiku with every
//! counter $5.85, the same without the write split $5.55 to $6.30. Luna
//! without write counters is above 272,000 input tokens at that scale, so
//! its long-context rates apply: $0.935 to $1.035.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crew::usage_metrics::collect::drain;
use crew::usage_metrics::ledger::{
    AsideIds, ExecutionStatus, Ledger, Locator, Registration, WorkBinding,
};
use crew::usage_metrics::query::{self, Context};
use crew::usage_metrics::source::Start;
use crew_core::rpc::features::{
    FeatureSession, FeatureVersions, Id, PageRequest, UnavailableFeatures,
};
use crew_core::rpc::usage_metrics::{
    AccountAttribution, ActivityKind, CoverageReason, EpicAttribution, UsageChanged,
    UsageDimension, UsageEpicFilter, UsageKind, UsagePrices, UsageQuery, UsageScope,
    UsageTaskFilter, UsageWatchResult, WorkAttribution,
};
use crew_core::types::RuntimeKind;
use serde::Serialize;
use serde_json::{json, Value};

/// 2026-10-08T00:00:00Z, when the current run starts.
const T0: i64 = 1_791_417_600_000;
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;
/// When the replies are taken.
const NOW: i64 = T0 + 2 * HOUR;
const RUN: &str = "run-2";
const EARLIER: &str = "run-1";

fn ms(t: i64) -> String {
    chrono::DateTime::from_timestamp_millis(t)
        .unwrap()
        .to_rfc3339()
}

fn claude(session: &str, id: &str, model: &str, t: i64, usage: Value) -> Value {
    json!({"type": "assistant", "timestamp": ms(t), "sessionId": session, "requestId": format!("req_{id}"),
        "message": {"id": id, "model": model, "content": [{"type": "text", "text": "PRIVATE"}], "usage": usage}})
}

/// Claude counters; `split` gives the 5-minute and 1-hour writes.
fn cu(n: u64, r: u64, w: u64, split: Option<(u64, u64)>, o: u64) -> Value {
    let mut u = json!({"input_tokens": n, "cache_read_input_tokens": r, "cache_creation_input_tokens": w, "output_tokens": o});
    if let Some((m5, h1)) = split {
        u["cache_creation"] =
            json!({"ephemeral_5m_input_tokens": m5, "ephemeral_1h_input_tokens": h1});
    }
    u
}

fn codex_count(t: i64, total: (u64, u64, u64), last: (u64, u64, u64)) -> Value {
    let v = |(i, r, o): (u64, u64, u64)| json!({"input_tokens": i, "cached_input_tokens": r, "output_tokens": o});
    json!({"type": "event_msg", "timestamp": ms(t),
        "payload": {"type": "token_count", "info": {"total_token_usage": v(total), "last_token_usage": v(last)}}})
}

fn append(path: &Path, values: &[Value]) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for v in values {
        writeln!(f, "{v}").unwrap();
    }
}

fn id(s: &str) -> Id {
    Id::new(s.to_string()).unwrap()
}

fn task(t: &str, epic: Option<&str>) -> WorkBinding {
    binding(WorkAttribution::Task {
        id: t.into(),
        epic: epic.map_or(EpicAttribution::NoEpic, |e| EpicAttribution::Epic {
            id: e.into(),
        }),
    })
}

fn binding(work: WorkAttribution) -> WorkBinding {
    WorkBinding {
        work,
        account: AccountAttribution::Unknown {
            reason: CoverageReason::UnknownAccount,
        },
        board_revision: Some(1),
        event_seq: Some(1),
    }
}

struct World {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    ledger: Ledger,
}

impl World {
    fn source(
        &mut self,
        run: &str,
        agent: &str,
        runtime: RuntimeKind,
        session: &str,
        aside: Option<(&str, &str)>,
    ) -> (String, PathBuf) {
        let path = self.dir.join(format!("{session}.jsonl"));
        File::create(&path).unwrap();
        let reg = Registration {
            run_id: run.into(),
            kind: if aside.is_some() {
                UsageKind::Aside
            } else {
                UsageKind::Live
            },
            runtime,
            native_session_id: session.into(),
            agent_id: agent.into(),
            incarnation: format!("{agent}-1"),
            launch_id: format!("{agent}-launch"),
            aside: aside.map(|(execution_id, aside_id)| AsideIds {
                execution_id: execution_id.into(),
                aside_id: aside_id.into(),
            }),
            locator: Locator::Private {
                agent_id: agent.into(),
            },
            started_ms: T0,
            start: Start::Fresh,
            cli_version: None,
        };
        let stream = self
            .ledger
            .register(&reg, &File::open(&path).unwrap(), T0)
            .unwrap()
            .unwrap();
        (stream, path)
    }

    fn bind(&mut self, (stream, path): &(String, PathBuf), b: &WorkBinding) {
        self.ledger
            .bind_work(stream, &File::open(path).unwrap(), b, T0)
            .unwrap();
    }

    fn write(&mut self, (stream, path): &(String, PathBuf), values: &[Value]) {
        append(path, values);
        drain(&mut self.ledger, stream, &File::open(path).unwrap(), NOW).unwrap();
    }
}

fn scope(run: &str, kind: UsageKind) -> UsageScope {
    UsageScope {
        workspace_run_id: id(run),
        kind,
        agent_incarnation: None,
        task: UsageTaskFilter::All,
        epic: UsageEpicFilter::All,
        model: None,
        account_id: None,
    }
}

fn query(scope: UsageScope) -> UsageQuery {
    UsageQuery {
        scope,
        from_ms: None,
        to_ms: None,
        group_by: None,
        bucket_seconds: None,
        price_book_id: None,
        page: PageRequest::default(),
    }
}

/// The scenario of F12–F14: one earlier run and the current one.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut w = World {
        dir: dir.path().to_path_buf(),
        _dir: dir,
        ledger: Ledger::in_memory().unwrap(),
    };
    let haiku = "claude-haiku-4-5-20251001";

    // The earlier run: tracking began after it started, so its first
    // completions before then are not all known.
    w.ledger
        .begin_run(EARLIER, Some(T0 - 26 * HOUR), T0 - 25 * HOUR)
        .unwrap();
    let old = w.source(EARLIER, "alpha", RuntimeKind::Claude, "s-old", None);
    w.bind(&old, &task("t0", None));
    w.write(
        &old,
        &[claude(
            "s-old",
            "msg_old",
            haiku,
            T0 - 25 * HOUR + 10 * MIN,
            cu(
                1_000_000,
                8_000_000,
                1_000_000,
                Some((600_000, 400_000)),
                500_000,
            ),
        )],
    );
    w.ledger
        .first_completion(
            EARLIER,
            "t0",
            T0 - 24 * HOUR - 30 * MIN,
            None,
            &EpicAttribution::NoEpic,
        )
        .unwrap();
    w.ledger.end_run(EARLIER, T0 - 24 * HOUR).unwrap();

    w.ledger.begin_run(RUN, Some(T0), T0).unwrap();

    // alpha (Claude Haiku): every counter, then a big write; task t1 of e1.
    let alpha = w.source(RUN, "alpha", RuntimeKind::Claude, "s-alpha", None);
    w.bind(&alpha, &task("t1", Some("e1")));
    w.write(
        &alpha,
        &[claude(
            "s-alpha",
            "msg_a1",
            haiku,
            T0 + 10 * MIN,
            cu(
                1_000_000,
                8_000_000,
                1_000_000,
                Some((600_000, 400_000)),
                500_000,
            ),
        )],
    );
    w.write(
        &alpha,
        &[claude(
            "s-alpha",
            "msg_a2",
            haiku,
            T0 + 40 * MIN,
            cu(2_000, 5_000, 20_000, Some((20_000, 0)), 1_000),
        )],
    );

    // beta (Claude Haiku): an aggregate write without its TTL split;
    // orchestration, known to have no task.
    let beta = w.source(RUN, "beta", RuntimeKind::Claude, "s-beta", None);
    w.bind(
        &beta,
        &binding(WorkAttribution::NoTask {
            activity: ActivityKind::Orchestration,
        }),
    );
    w.write(
        &beta,
        &[claude(
            "s-beta",
            "msg_b1",
            haiku,
            T0 + 15 * MIN,
            cu(1_000_000, 8_000_000, 1_000_000, None, 500_000),
        )],
    );

    // gamma (Codex Luna): no write counters; one proven request, then a
    // batch of unknown cardinality; task t2 without an epic.
    let gamma = w.source(RUN, "gamma", RuntimeKind::Codex, "th-gamma", None);
    w.bind(&gamma, &task("t2", None));
    w.write(
        &gamma,
        &[
            json!({"type": "session_meta", "payload": {"id": "th-gamma"}}),
            json!({"type": "turn_context", "payload": {"model": "gpt-6-luna", "turn_id": "g1"}}),
            codex_count(
                T0 + 20 * MIN,
                (10_000_000, 8_000_000, 500_000),
                (10_000_000, 8_000_000, 500_000),
            ),
        ],
    );
    w.write(
        &gamma,
        &[codex_count(
            T0 + 50 * MIN,
            (10_200_000, 8_120_000, 510_000),
            (100_000, 60_000, 5_000),
        )],
    );

    // delta (Claude): a model without a price, never bound to work.
    let delta = w.source(RUN, "delta", RuntimeKind::Claude, "s-delta", None);
    w.write(
        &delta,
        &[claude(
            "s-delta",
            "msg_d1",
            "claude-mystery-1",
            T0 + 30 * MIN,
            cu(100_000, 0, 0, Some((0, 0)), 50_000),
        )],
    );

    // epsilon: a member whose session has no usage yet.
    w.source(RUN, "epsilon", RuntimeKind::Claude, "s-epsilon", None);

    // One aside answer with usage, one that failed before any.
    let aside = w.source(
        RUN,
        "alpha",
        RuntimeKind::Claude,
        "s-aside",
        Some(("x1", "q1")),
    );
    w.write(
        &aside,
        &[claude(
            "s-aside",
            "msg_x1",
            haiku,
            T0 + 45 * MIN,
            cu(1_000, 8_000, 1_000, Some((1_000, 0)), 500),
        )],
    );
    for (execution, status) in [
        ("x1", ExecutionStatus::Answered),
        ("x2", ExecutionStatus::Failed),
    ] {
        w.ledger
            .execution_status(
                RUN,
                "q1",
                execution,
                status,
                Some(T0 + 44 * MIN),
                Some(T0 + 46 * MIN),
            )
            .unwrap();
    }

    // First completions: t1 (completed again later, counted once), t2.
    for (t, at, epic) in [
        (
            "t1",
            T0 + 60 * MIN,
            EpicAttribution::Epic { id: "e1".into() },
        ),
        ("t2", T0 + 70 * MIN, EpicAttribution::NoEpic),
        (
            "t1",
            T0 + 90 * MIN,
            EpicAttribution::Epic { id: "e1".into() },
        ),
    ] {
        w.ledger.first_completion(RUN, t, at, None, &epic).unwrap();
    }
    w
}

fn ctx() -> Context {
    Context {
        now: NOW,
        expected_agents: Some(5),
        backlog_files: Some(2),
    }
}

fn pretty<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

fn v<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn usage_json() {
    let mut w = world();
    let live = || query(scope(RUN, UsageKind::Live));
    let grouped = |d: UsageDimension| UsageQuery {
        group_by: Some(d),
        ..live()
    };
    let epic = UsageQuery {
        scope: UsageScope {
            epic: UsageEpicFilter::Id("e1".into()),
            ..scope(RUN, UsageKind::Live)
        },
        ..live()
    };
    let no_completions = UsageQuery {
        to_ms: Some(crew_core::rpc::features::Timestamp::new(T0 + 30 * MIN).unwrap()),
        ..live()
    };
    let series = UsageQuery {
        bucket_seconds: Some(300),
        from_ms: Some(crew_core::rpc::features::Timestamp::new(T0).unwrap()),
        to_ms: Some(crew_core::rpc::features::Timestamp::new(T0 + HOUR).unwrap()),
        ..live()
    };
    let c = ctx();
    let summary = query::summary(&w.ledger, &live(), &c).unwrap();
    let mut out = json!({
        "inputs": "see crates/crew/tests/web_usage.rs: counters are the notes/usage-metrics.md examples times 100,000",
        "negotiated": v(FeatureSession {
            accepted: FeatureVersions::try_from(
                [("usage_metrics".to_string(), 1)].into_iter().collect::<std::collections::BTreeMap<_, _>>(),
            )
            .unwrap(),
            connection_epoch: id("ep-1"),
            workspace_run_id: Some(id(RUN)),
            unavailable: UnavailableFeatures::default(),
        }),
        "summary": v(&summary),
        "watch": v(UsageWatchResult { epoch: id("ep-1"), watch_id: id("w-1"), initial: summary.clone() }),
        "summary_epic": v(query::summary(&w.ledger, &epic, &c).unwrap()),
        "summary_no_completions": v(query::summary(&w.ledger, &no_completions, &c).unwrap()),
        "summary_asides": v(query::summary(&w.ledger, &query(scope(RUN, UsageKind::Aside)), &c).unwrap()),
        "summary_earlier": v(query::summary(&w.ledger, &query(scope(EARLIER, UsageKind::Live)), &Context { now: NOW, expected_agents: None, backlog_files: None }).unwrap()),
        "series": v(query::series(&w.ledger, &series, &c).unwrap()),
        "breakdown_agent": v(query::breakdown(&w.ledger, &grouped(UsageDimension::Agent), &c).unwrap()),
        "breakdown_epic": v(query::breakdown(&w.ledger, &grouped(UsageDimension::Epic), &c).unwrap()),
        "breakdown_model": v(query::breakdown(&w.ledger, &grouped(UsageDimension::Model), &c).unwrap()),
        "observations": v(query::observations(&w.ledger, &live()).unwrap()),
        "runs": v(query::runs(&w.ledger, &PageRequest::default(), &c).unwrap()),
        "prices": v(query::prices(&UsagePrices { price_book_id: None }).unwrap()),
    });

    // A later copy of msg_a1 reports less output: the figure goes down.
    let before = w.ledger.revision().unwrap();
    let alpha = ("claude:s-alpha".to_string(), w.dir.join("s-alpha.jsonl"));
    w.write(
        &alpha,
        &[claude(
            "s-alpha",
            "msg_a1",
            "claude-haiku-4-5-20251001",
            T0 + 10 * MIN,
            cu(
                1_000_000,
                8_000_000,
                1_000_000,
                Some((600_000, 400_000)),
                400_000,
            ),
        )],
    );
    let after = w.ledger.revision().unwrap();
    assert!(after > before);
    out["changed"] = json!({"push": {"kind": "usage_changed", "data": v(UsageChanged {
        workspace_run_id: id(RUN),
        epoch: id("ep-1"),
        watch_id: id("w-1"),
        from_revision: crew_core::rpc::features::Wide(before as u128),
        to_revision: crew_core::rpc::features::Wide(after as u128),
        resync_required: false,
    })}});
    out["summary_corrected"] = v(query::summary(&w.ledger, &live(), &c).unwrap());

    // zeta: a member whose session file is missing. What it spent is not
    // known, so no estimate has a finite upper bound any more.
    w.ledger
        .record_source_gap(
            RUN,
            "claude:s-zeta",
            Some(UsageKind::Live),
            CoverageReason::SourceGap,
            T0 + 5 * MIN,
        )
        .unwrap();
    let c = Context {
        expected_agents: Some(6),
        ..ctx()
    };
    out["summary_missing_source"] = v(query::summary(&w.ledger, &live(), &c).unwrap());
    check("usage.json", pretty(&out));
}

fn check(name: &str, contents: String) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/web");
    let path = dir.join(name);
    if std::env::var("CREW_UPDATE_FIXTURES").as_deref() == Ok("1") {
        std::fs::write(&path, &contents).unwrap();
        return;
    }
    let found = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        found == contents,
        "{} differs from what the usage queries answer now; if the change is meant, run \
`CREW_UPDATE_FIXTURES=1 cargo test -p crew --test web_usage` and tell the frontend",
        path.display()
    );
}
