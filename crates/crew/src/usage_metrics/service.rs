//! The usage metrics service: collection on its own schedule, queries,
//! watches, and the lifecycle inputs the daemon calls at its central seams.
//!
//! Collection is independent of any client: each registered stream is read
//! by at most one job at a time, at most two blocking jobs run at once, and a
//! stream is rechecked at most once a second while it grows and once every
//! five seconds while idle. A stopped source drains for a bounded grace and
//! reports whether its end was seen. Nothing here starts a runtime, sends a
//! prompt, reads a credential, or touches an agent's activity or scheduling.
//!
//! The daemon wires dispatch, authorization and connection state once
//! (`notes/protocol-features.md`, P2); this module exports typed handlers,
//! readiness and watch state for it.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use crew_core::rpc::asides::AsideUsage;
use crew_core::rpc::features::{FeatureUnavailable, Id, PageRequest, Unwatch, Wide};
use crew_core::rpc::usage_metrics::{
    CostEstimate, CoverageReason, EpicAttribution, PriceBook, UsageBreakdown, UsageChanged,
    UsageObservations, UsagePrices, UsageQuery, UsageRuns, UsageSeries, UsageSummary,
    UsageWatchResult,
};
use crew_core::rpc::RpcError;
use crew_core::RuntimeKind;
use tokio::sync::{watch, Semaphore};

use super::collect::pump;
use super::ledger::{ExecutionStatus, Ledger, Locator, Registration, Retention, WorkBinding};
use super::query::{self, Context};
use crate::agents::files::{AgentFiles, CliHome};

/// At most this many registered streams are collected at once.
pub const MAX_STREAMS: usize = 64;
/// At most this many blocking reader jobs run at once.
pub const READERS: usize = 2;
/// At most this many usage watches per connection.
pub const MAX_WATCHES: usize = 4;
const ACTIVE_MS: i64 = 1_000;
const IDLE_MS: i64 = 5_000;
/// How long a stopped source is drained.
const GRACE_MS: i64 = 30_000;
/// How often retention runs.
const RETENTION_MS: i64 = 3_600_000;

/// Whether the service can answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Unavailable(FeatureUnavailable),
}

/// Opens a stream's native file, fresh, for one read.
pub trait Opener: Send + Sync {
    fn open(&self, source: &Source) -> Option<File>;
}

/// What the service needs to reopen a stream's native file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub runtime: RuntimeKind,
    pub session: String,
    pub started_ms: i64,
    pub locator: Locator,
}

/// Opens native files the way the daemon reaches each home: a private home
/// beneath the agent's directory, a CLI home through its captured root.
pub struct NativeOpener {
    pub state_dir: PathBuf,
    homes: Mutex<HashMap<PathBuf, Arc<CliHome>>>,
}

impl NativeOpener {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            homes: Mutex::new(HashMap::new()),
        }
    }

    /// Captures a CLI home when its member launches, so later reads go
    /// through that root.
    pub fn capture(&self, path: &std::path::Path) -> bool {
        match CliHome::capture(path) {
            Some(home) => {
                self.homes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(path.to_path_buf(), Arc::new(home));
                true
            }
            None => false,
        }
    }
}

impl Opener for NativeOpener {
    fn open(&self, source: &Source) -> Option<File> {
        let opened = match &source.locator {
            Locator::Private { agent_id } => {
                let root = File::open(&self.state_dir).ok()?;
                use std::os::fd::AsFd;
                let files = AgentFiles::open_existing(
                    root.as_fd(),
                    self.state_dir.join("agents").join(agent_id),
                    agent_id,
                )
                .ok()?;
                match source.runtime {
                    RuntimeKind::Claude => files.claude_transcript(&source.session),
                    RuntimeKind::Codex => files.codex_rollout(&source.session, source.started_ms),
                    _ => None,
                }
            }
            Locator::CliHome { path } => {
                let home = self
                    .homes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(path)
                    .cloned()?;
                match source.runtime {
                    RuntimeKind::Claude => home.claude_transcript(&source.session),
                    RuntimeKind::Codex => home.codex_rollout(&source.session, source.started_ms),
                    _ => None,
                }
            }
        };
        opened.map(|o| o.file)
    }
}

/// A registered stream's collection state.
#[derive(Clone, Debug)]
struct Stream {
    source: Source,
    due: i64,
    busy: bool,
    caught_up: bool,
    /// Draining after its owner stopped, until this time.
    stopping: Option<i64>,
}

/// One usage watch of a connection.
#[derive(Clone, Debug)]
struct Watch {
    id: Id,
    run: Id,
    revision: u64,
    sent_at: i64,
}

/// The service, shared by the daemon's handlers and its collector task.
pub struct UsageMetrics {
    ledger: Arc<Mutex<Ledger>>,
    opener: Arc<dyn Opener>,
    streams: Mutex<BTreeMap<String, Stream>>,
    expected: Mutex<HashMap<String, u64>>,
    watches: Mutex<HashMap<String, Vec<Watch>>>,
    next_watch: Mutex<u64>,
    epoch: Id,
    revision: watch::Sender<u64>,
    permits: Arc<Semaphore>,
    retained_at: Mutex<i64>,
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::internal(format!("usage metrics: {e:#}"))
}

impl UsageMetrics {
    pub fn new(ledger: Ledger, opener: Arc<dyn Opener>, epoch: Id) -> Arc<Self> {
        let revision = ledger.revision().unwrap_or(0);
        Arc::new(Self {
            ledger: Arc::new(Mutex::new(ledger)),
            opener,
            streams: Mutex::new(BTreeMap::new()),
            expected: Mutex::new(HashMap::new()),
            watches: Mutex::new(HashMap::new()),
            next_watch: Mutex::new(0),
            epoch,
            revision: watch::channel(revision).0,
            permits: Arc::new(Semaphore::new(READERS)),
            retained_at: Mutex::new(0),
        })
    }

    pub fn readiness(&self) -> Readiness {
        match self.ledger.try_lock() {
            Err(std::sync::TryLockError::Poisoned(_)) => {
                Readiness::Unavailable(FeatureUnavailable::NotReady)
            }
            _ => Readiness::Ready,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T, RpcError> {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        let out = f(&mut ledger).map_err(internal)?;
        let revision = ledger.revision().map_err(internal)?;
        drop(ledger);
        self.revision.send_if_modified(|r| {
            let changed = *r != revision;
            *r = revision;
            changed
        });
        Ok(out)
    }

    /// Wakes when the metrics revision changes.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    fn context(&self, run: &str, now: i64) -> Context {
        let streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        Context {
            now,
            expected_agents: self
                .expected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(run)
                .copied(),
            backlog_files: Some(streams.values().filter(|s| !s.caught_up).count() as u64),
        }
    }

    // Lifecycle inputs, called by the daemon at its central seams.

    /// A run starts being tracked; see [`Ledger::begin_run`].
    pub fn begin_run(
        &self,
        run: &str,
        started_at: Option<i64>,
        tracking_since: i64,
    ) -> Result<(), RpcError> {
        self.with(|l| l.begin_run(run, started_at, tracking_since))
    }

    /// A run ended (session cleared): its streams stop.
    pub fn end_run(&self, run: &str, now: i64) -> Result<(), RpcError> {
        self.with(|l| l.end_run(run, now))?;
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        for s in streams.values_mut() {
            s.stopping.get_or_insert(now + GRACE_MS);
        }
        Ok(())
    }

    /// Model-backed member incarnations registered in a run, whatever their
    /// sources' state.
    pub fn set_expected_agents(&self, run: &str, n: u64) {
        self.expected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(run.to_string(), n);
    }

    /// A member launched with a confirmed native session: start collecting
    /// it. Refused (with the coverage reason) for an unsupported runtime, a
    /// missing session, a session another incarnation owns, a source that
    /// cannot be opened, or beyond [`MAX_STREAMS`].
    pub fn register(
        &self,
        reg: &Registration,
        now: i64,
    ) -> Result<Result<String, CoverageReason>, RpcError> {
        let source = Source {
            runtime: reg.runtime,
            session: reg.native_session_id.clone(),
            started_ms: reg.started_ms,
            locator: reg.locator.clone(),
        };
        if self.streams.lock().unwrap_or_else(|e| e.into_inner()).len() >= MAX_STREAMS {
            return Ok(Err(CoverageReason::NotTracked));
        }
        let Some(file) = self.opener.open(&source) else {
            return Ok(Err(CoverageReason::SourceGap));
        };
        let registered = self.with(|l| l.register(reg, &file, now))?;
        if let Ok(stream) = &registered {
            self.streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    stream.clone(),
                    Stream {
                        source,
                        due: now,
                        busy: false,
                        caught_up: false,
                        stopping: None,
                    },
                );
        }
        Ok(registered)
    }

    /// A turn boundary: usage read from here on is `binding`'s.
    pub fn bind_work(&self, stream: &str, binding: &WorkBinding, now: i64) -> Result<(), RpcError> {
        let source = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(stream)
            .map(|s| s.source.clone())
            .ok_or_else(|| RpcError::not_found("no usage stream"))?;
        let file = self
            .opener
            .open(&source)
            .ok_or_else(|| RpcError::unavailable("usage source cannot be opened"))?;
        self.with(|l| l.bind_work(stream, &file, binding, now))
    }

    /// The member stopped: drain its stream for a bounded grace.
    pub fn stop(&self, stream: &str, now: i64) {
        if let Some(s) = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(stream)
        {
            s.stopping.get_or_insert(now + GRACE_MS);
            s.due = now;
        }
    }

    /// A task's first valid completion; see [`Ledger::first_completion`].
    pub fn first_completion(
        &self,
        run: &str,
        task: &str,
        at: i64,
        event_seq: Option<u64>,
        epic: &EpicAttribution,
    ) -> Result<bool, RpcError> {
        self.with(|l| l.first_completion(run, task, at, event_seq, epic))
    }

    /// An aside execution's status; see [`Ledger::execution_status`].
    pub fn execution_status(
        &self,
        run: &str,
        aside: &str,
        execution: &str,
        status: ExecutionStatus,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), RpcError> {
        self.with(|l| l.execution_status(run, aside, execution, status, started_at, finished_at))
    }

    pub fn aside_usage(&self, run: &str, execution: &str) -> Result<Option<AsideUsage>, RpcError> {
        let book = query::book(None)?;
        self.with(|l| l.aside_usage(run, execution, &book))
    }

    pub fn aside_total_cost(
        &self,
        run: &str,
        aside: &str,
    ) -> Result<Option<CostEstimate>, RpcError> {
        let book = query::book(None)?;
        self.with(|l| l.aside_total_cost(run, aside, &book))
    }

    // Collection.

    /// Streams due for a read now, marked busy.
    fn due(&self, now: i64) -> Vec<(String, Source)> {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        streams
            .iter_mut()
            .filter(|(_, s)| !s.busy && s.due <= now)
            .map(|(id, s)| {
                s.busy = true;
                (id.clone(), s.source.clone())
            })
            .collect()
    }

    /// Reads one batch of a stream and schedules its next read. Runs on a
    /// blocking worker.
    fn read(&self, stream: &str, source: &Source, now: i64) {
        let outcome = self
            .opener
            .open(source)
            .map(|file| pump(&*self.ledger, stream, &file, now, None));
        let (caught_up, grew) = match &outcome {
            Some(Ok(p)) => (p.caught_up, p.commits.iter().any(|c| c.visible())),
            _ => (false, false),
        };
        let revision = self
            .ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision()
            .unwrap_or(0);
        self.revision.send_if_modified(|r| {
            let changed = *r != revision;
            *r = revision;
            changed
        });
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = streams.get_mut(stream) else {
            return;
        };
        s.busy = false;
        s.caught_up = caught_up;
        s.due = if !caught_up && outcome.as_ref().is_some_and(|o| o.is_ok()) {
            now
        } else if grew {
            now + ACTIVE_MS
        } else {
            now + IDLE_MS
        };
        if let Some(until) = s.stopping {
            if caught_up || now >= until {
                streams.remove(stream);
                let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
                let _ = ledger.release(stream);
                if !caught_up {
                    let _ = ledger.close_admission(stream, CoverageReason::SourceBacklog, now);
                }
            }
        }
    }

    /// One pass of collection: every due stream, in this thread. Tests and
    /// a synchronous shutdown drain use it; the daemon runs [`Self::spawn`].
    pub fn tick(&self, now: i64) {
        for (stream, source) in self.due(now) {
            self.read(&stream, &source, now);
        }
        self.retain(now);
    }

    fn retain(&self, now: i64) {
        let mut last = self.retained_at.lock().unwrap_or_else(|e| e.into_inner());
        if now - *last >= RETENTION_MS {
            *last = now;
            drop(last);
            let _ = self.with(|l| l.expire(now, Retention::default()));
        }
    }

    /// Runs collection until the returned handle is aborted: due streams on
    /// at most [`READERS`] blocking workers. A worker's permit is held until
    /// its blocking read returns, whatever happens to the task that started
    /// it.
    pub fn spawn(self: &Arc<Self>, clock: fn() -> i64) -> tokio::task::JoinHandle<()> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let now = clock();
                for (stream, source) in service.due(now) {
                    let Ok(permit) = Arc::clone(&service.permits).acquire_owned().await else {
                        return;
                    };
                    let worker = Arc::clone(&service);
                    tokio::task::spawn_blocking(move || {
                        worker.read(&stream, &source, clock());
                        drop(permit);
                    });
                }
                let retainer = Arc::clone(&service);
                let _ = tokio::task::spawn_blocking(move || retainer.retain(clock())).await;
            }
        })
    }

    // Queries, run on blocking workers by the daemon.

    pub fn summary(&self, q: &UsageQuery, now: i64) -> Result<UsageSummary, RpcError> {
        let ctx = self.context(q.scope.workspace_run_id.as_str(), now);
        self.query(|l| query::summary(l, q, &ctx))
    }

    pub fn series(&self, q: &UsageQuery, now: i64) -> Result<UsageSeries, RpcError> {
        let ctx = self.context(q.scope.workspace_run_id.as_str(), now);
        self.query(|l| query::series(l, q, &ctx))
    }

    pub fn breakdown(&self, q: &UsageQuery, now: i64) -> Result<UsageBreakdown, RpcError> {
        let ctx = self.context(q.scope.workspace_run_id.as_str(), now);
        self.query(|l| query::breakdown(l, q, &ctx))
    }

    pub fn observations(&self, q: &UsageQuery) -> Result<UsageObservations, RpcError> {
        self.query(|l| query::observations(l, q))
    }

    pub fn runs(&self, page: &PageRequest, now: i64) -> Result<UsageRuns, RpcError> {
        let ctx = self.context("", now);
        self.query(|l| query::runs(l, page, &ctx))
    }

    pub fn prices(&self, req: &UsagePrices) -> Result<PriceBook, RpcError> {
        query::prices(req)
    }

    fn query<T>(&self, f: impl FnOnce(&Ledger) -> Result<T, RpcError>) -> Result<T, RpcError> {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        f(&ledger)
    }

    // Watches, keyed by the daemon's authenticated connection epoch.

    /// Registers a watch before capturing its initial summary, so no change
    /// between them is lost. At most [`MAX_WATCHES`] per connection.
    pub fn watch(
        &self,
        conn: &str,
        q: &UsageQuery,
        now: i64,
    ) -> Result<UsageWatchResult, RpcError> {
        let id = {
            let mut watches = self.watches.lock().unwrap_or_else(|e| e.into_inner());
            let list = watches.entry(conn.to_string()).or_default();
            if list.len() >= MAX_WATCHES {
                return Err(RpcError::new(
                    "busy",
                    "too many usage watches on this connection",
                ));
            }
            let mut next = self.next_watch.lock().unwrap_or_else(|e| e.into_inner());
            *next += 1;
            let id = Id::new(format!("usage-watch-{}", *next)).expect("short");
            list.push(Watch {
                id: id.clone(),
                run: q.scope.workspace_run_id.clone(),
                revision: *self.revision.borrow(),
                sent_at: now,
            });
            id
        };
        match self.summary(q, now) {
            Ok(initial) => {
                let captured = initial.revision.0 as u64;
                if let Some(w) = self
                    .watches
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(conn)
                    .and_then(|l| l.iter_mut().find(|w| w.id == id))
                {
                    w.revision = captured;
                }
                Ok(UsageWatchResult {
                    epoch: self.epoch.clone(),
                    watch_id: id,
                    initial,
                })
            }
            Err(e) => {
                self.forget(conn, &id);
                Err(e)
            }
        }
    }

    fn forget(&self, conn: &str, id: &Id) -> bool {
        let mut watches = self.watches.lock().unwrap_or_else(|e| e.into_inner());
        let Some(list) = watches.get_mut(conn) else {
            return false;
        };
        let before = list.len();
        list.retain(|w| &w.id != id);
        let removed = list.len() < before;
        if list.is_empty() {
            watches.remove(conn);
        }
        removed
    }

    pub fn unwatch(&self, conn: &str, req: &Unwatch) -> Result<(), RpcError> {
        if self.forget(conn, &req.watch_id) {
            Ok(())
        } else {
            Err(RpcError::not_found(
                "no such usage watch on this connection",
            ))
        }
    }

    /// The connection went away: its watches end.
    pub fn disconnect(&self, conn: &str) {
        self.watches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(conn);
    }

    /// Change notices for a connection's watches, at most one per watch per
    /// second, each covering every revision since its last notice.
    pub fn pending(&self, conn: &str, now: i64) -> Vec<UsageChanged> {
        let current = *self.revision.borrow();
        let mut watches = self.watches.lock().unwrap_or_else(|e| e.into_inner());
        let Some(list) = watches.get_mut(conn) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for w in list.iter_mut() {
            if w.revision != current && now - w.sent_at >= ACTIVE_MS {
                out.push(UsageChanged {
                    workspace_run_id: w.run.clone(),
                    epoch: self.epoch.clone(),
                    watch_id: w.id.clone(),
                    from_revision: Wide(w.revision as u128),
                    to_revision: Wide(current as u128),
                    resync_required: false,
                });
                w.revision = current;
                w.sent_at = now;
            }
        }
        out
    }
}

/// A timestamp for the service clock.
pub fn now_ms() -> i64 {
    crew_core::util::now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_metrics::claude::tests::assistant;
    use crate::usage_metrics::source::Start;
    use crew_core::rpc::usage_metrics::{
        UsageEpicFilter, UsageKind, UsageScope, UsageTaskFilter, WorkAttribution,
    };
    use serde_json::{json, Value};
    use std::io::Write;
    use std::path::Path;

    const NOW: i64 = 1_791_417_600_000;

    /// Opens sessions from a map of session id to path.
    #[derive(Default)]
    struct Files(Mutex<HashMap<String, PathBuf>>);

    impl Opener for Files {
        fn open(&self, source: &Source) -> Option<File> {
            let path = self.0.lock().unwrap().get(&source.session).cloned()?;
            File::open(path).ok()
        }
    }

    fn msg(id: &str, o: u64) -> Value {
        assistant(
            id,
            NOW,
            json!({"input_tokens": 10, "cache_read_input_tokens": 80, "cache_creation_input_tokens": 10, "output_tokens": o}),
        )
    }

    fn append(path: &Path, values: &[Value]) {
        let text: String = values.iter().map(|v| format!("{v}\n")).collect();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }

    fn reg(session: &str) -> Registration {
        Registration {
            run_id: "r1".into(),
            kind: UsageKind::Live,
            runtime: RuntimeKind::Claude,
            native_session_id: session.into(),
            agent_id: "worker".into(),
            incarnation: format!("i-{session}"),
            launch_id: "l1".into(),
            aside: None,
            locator: Locator::Private {
                agent_id: "worker".into(),
            },
            started_ms: NOW,
            start: Start::Fresh,
            cli_version: None,
        }
    }

    fn query() -> UsageQuery {
        UsageQuery {
            scope: UsageScope {
                workspace_run_id: Id::new("r1").unwrap(),
                kind: UsageKind::Live,
                agent_incarnation: None,
                task: UsageTaskFilter::All,
                epic: UsageEpicFilter::All,
                model: None,
                account_id: None,
            },
            from_ms: None,
            to_ms: None,
            group_by: None,
            bucket_seconds: None,
            price_book_id: None,
            page: PageRequest::default(),
        }
    }

    fn setup() -> (tempfile::TempDir, Arc<Files>, Arc<UsageMetrics>) {
        let dir = tempfile::tempdir().unwrap();
        let files = Arc::new(Files::default());
        let service = UsageMetrics::new(
            Ledger::in_memory().unwrap(),
            files.clone(),
            Id::new("epoch-1").unwrap(),
        );
        service.begin_run("r1", Some(NOW), NOW).unwrap();
        (dir, files, service)
    }

    fn add(dir: &tempfile::TempDir, files: &Files, session: &str) -> PathBuf {
        let path = dir.path().join(format!("{session}.jsonl"));
        std::fs::write(&path, "").unwrap();
        files.0.lock().unwrap().insert(session.into(), path.clone());
        path
    }

    fn output(service: &UsageMetrics, now: i64) -> Option<u128> {
        service
            .summary(&query(), now)
            .unwrap()
            .totals
            .output
            .known_sum
            .map(|w| w.0)
    }

    #[test]
    fn collection_runs_on_its_own_schedule_and_signals_changes() {
        let (dir, files, service) = setup();
        let path = add(&dir, &files, "s-1");
        let stream = service.register(&reg("s-1"), NOW).unwrap().unwrap();
        let mut changes = service.changes();
        changes.borrow_and_update();
        append(&path, &[msg("a", 5)]);
        service.tick(NOW);
        assert_eq!(output(&service, NOW), Some(5));
        assert!(changes.has_changed().unwrap());

        // It grew: due again in a second, not before.
        append(&path, &[msg("b", 6)]);
        service.tick(NOW + 500);
        assert_eq!(output(&service, NOW), Some(5));
        service.tick(NOW + 1_000);
        assert_eq!(output(&service, NOW), Some(11));
        // Idle now: five seconds.
        service.tick(NOW + 2_000);
        append(&path, &[msg("c", 1)]);
        service.tick(NOW + 6_000);
        assert_eq!(output(&service, NOW), Some(11));
        service.tick(NOW + 7_000);
        assert_eq!(output(&service, NOW), Some(12));

        // A turn boundary through the service binds later usage.
        service
            .bind_work(
                &stream,
                &WorkBinding {
                    work: WorkAttribution::NoTask {
                        activity: crew_core::rpc::usage_metrics::ActivityKind::Keepalive,
                    },
                    account: crew_core::rpc::usage_metrics::AccountAttribution::Unknown {
                        reason: CoverageReason::UnknownAccount,
                    },
                    board_revision: None,
                    event_seq: None,
                },
                NOW,
            )
            .unwrap();
        append(&path, &[msg("d", 2)]);
        service.tick(NOW + 20_000);
        let mut no_task = query();
        no_task.scope.task = UsageTaskFilter::NoTask;
        let s = service.summary(&no_task, NOW).unwrap();
        assert_eq!(s.totals.output.known_sum, Some(Wide(2)));
    }

    #[test]
    fn registration_limits_and_stopped_sources() {
        let (dir, files, service) = setup();
        assert_eq!(
            service.register(&reg("missing"), NOW).unwrap(),
            Err(CoverageReason::SourceGap),
            "a source that cannot be opened is not collected"
        );
        for n in 0..MAX_STREAMS {
            add(&dir, &files, &format!("s{n}"));
            service
                .register(&reg(&format!("s{n}")), NOW)
                .unwrap()
                .unwrap();
        }
        add(&dir, &files, "one-more");
        assert_eq!(
            service.register(&reg("one-more"), NOW).unwrap(),
            Err(CoverageReason::NotTracked)
        );

        // A stopped source drains, then leaves; one that cannot be read
        // before its grace ends closes with a backlog gap.
        let path = dir.path().join("s0.jsonl");
        let mut late = msg("late", 3);
        late["sessionId"] = json!("s0");
        append(&path, &[late]);
        service.stop("claude:s0", NOW);
        service.tick(NOW);
        assert_eq!(output(&service, NOW), Some(3));
        assert!(!service.streams.lock().unwrap().contains_key("claude:s0"));
        files.0.lock().unwrap().remove("s1");
        service.stop("claude:s1", NOW);
        service.tick(NOW + GRACE_MS);
        assert!(!service.streams.lock().unwrap().contains_key("claude:s1"));
        let ledger = service.ledger.lock().unwrap();
        assert_eq!(
            ledger.coverage("claude:s1").unwrap().get("source_backlog"),
            Some(&1)
        );
    }

    #[test]
    fn watches_register_first_coalesce_and_end_with_their_connection() {
        let (dir, files, service) = setup();
        let path = add(&dir, &files, "s-1");
        service.register(&reg("s-1"), NOW).unwrap().unwrap();
        let w = service.watch("conn-a", &query(), NOW).unwrap();
        assert_eq!(w.epoch.as_str(), "epoch-1");
        assert_eq!(w.initial.totals.output.known_sum, None);
        for _ in 1..MAX_WATCHES {
            service.watch("conn-a", &query(), NOW).unwrap();
        }
        assert_eq!(
            service.watch("conn-a", &query(), NOW).unwrap_err().code,
            "busy"
        );
        assert!(
            service.pending("conn-a", NOW + 5_000).is_empty(),
            "nothing changed"
        );

        append(&path, &[msg("a", 5)]);
        service.tick(NOW);
        append(&path, &[msg("b", 6)]);
        service.tick(NOW + 1_000);
        let changed = service.pending("conn-a", NOW + 1_500);
        assert_eq!(changed.len(), MAX_WATCHES, "one coalesced notice per watch");
        assert!(changed[0].to_revision.0 > changed[0].from_revision.0);
        append(&path, &[msg("c", 1)]);
        service.tick(NOW + 2_000);
        assert!(
            service.pending("conn-a", NOW + 2_000).is_empty(),
            "at most once a second"
        );
        assert_eq!(service.pending("conn-a", NOW + 2_600).len(), MAX_WATCHES);

        assert_eq!(
            service
                .unwatch(
                    "conn-a",
                    &Unwatch {
                        watch_id: Id::new("nope").unwrap()
                    }
                )
                .unwrap_err()
                .code,
            "not_found"
        );
        service
            .unwatch(
                "conn-a",
                &Unwatch {
                    watch_id: w.watch_id.clone(),
                },
            )
            .unwrap();
        assert!(service
            .unwatch(
                "conn-b",
                &Unwatch {
                    watch_id: w.watch_id
                }
            )
            .is_err());
        service.disconnect("conn-a");
        assert!(service.pending("conn-a", NOW + 9_000).is_empty());
        assert_eq!(service.readiness(), Readiness::Ready);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_spawned_collector_reads_without_a_client() {
        let (dir, files, service) = setup();
        let path = add(&dir, &files, "s-1");
        service.register(&reg("s-1"), now_ms()).unwrap().unwrap();
        let mut changes = service.changes();
        changes.borrow_and_update();
        let task = service.spawn(now_ms);
        append(&path, &[msg("a", 5)]);
        tokio::time::timeout(std::time::Duration::from_secs(10), changes.changed())
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert_eq!(output(&service, now_ms()), Some(5));
    }
}
