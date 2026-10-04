//! The dashboard server's workspaces (notes/web-multi.md §multi-join,
//! §multi-server "Reconciling").
//!
//! A workspace is shown once the operator joined it. Its **membership** has
//! a generation that owns every pending open, every workspace connection and
//! the server's own connection to its daemon (the *watch*); taking it out
//! revokes the generation first and answers only when nothing of it holds the
//! daemon any more (M9).
//!
//! What the server believes of a membership (`running`, `stopped`,
//! `incompatible`, `unreachable`, and which watch) is changed by one task,
//! one event after another, always by comparing it with what the registry
//! publishes at that moment (M10). A watch belongs to one incarnation of a
//! daemon; the end of one that was superseded is not news. A rescan request
//! sets a flag that the task clears before it reads the registry, so none is
//! lost to a scan already under way. Nothing is read on a timer: the only
//! timer here is the retry of a published daemon that does not answer.
//!
//! The listing carries a revision that grows with every change, read
//! together with the listing under the table's lock.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{
    Call, DaemonRef, OtherSession, Push, Request, RpcError, ServerFrame, WorkspaceEntry,
    WorkspaceSummary, Workspaces,
};
use crew_core::types::{WebEventKind, WorkspaceState};
use crew_core::util::now_ms;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::task::JoinHandle;

use super::daemons::{self, Daemons, DialError, Dialled, DIAL_WITHIN};

/// Workspaces joined at once.
pub(crate) const MAX_JOINED: usize = 32;
/// A published daemon that did not answer is dialled again after this,
/// doubling up to `RETRY_MAX`.
pub(crate) const RETRY_FIRST: Duration = Duration::from_millis(250);
pub(crate) const RETRY_MAX: Duration = Duration::from_secs(5);
/// A removal cancels whatever of the generation has not let go after this.
pub(crate) const LET_GO_WITHIN: Duration = Duration::from_secs(1);
/// And then waits at most this long for the cancelled to be gone.
const DRAINED_WITHIN: Duration = Duration::from_secs(5);
/// A page's `workspaces` call asks for a rescan at most this often.
const LISTING_RESCAN_EVERY: Duration = Duration::from_secs(1);
/// The longest line read from a daemon.
const MAX_LINE: usize = crew_core::MAX_FRAME_BYTES + 1024;

/// How many units of one generation (pending opens, connections, the
/// watch) hold, or may still get, a connection to its daemon.
pub(crate) struct Gauge(watch::Sender<usize>);

/// One unit's hold on its generation, for exactly as long as it owns a
/// connection to the daemon or may still get one.
pub(crate) struct Guard(Arc<Gauge>);

impl Gauge {
    fn new() -> Arc<Self> {
        Arc::new(Self(watch::channel(0).0))
    }

    pub(crate) fn hold(self: &Arc<Self>) -> Guard {
        self.0.send_modify(|n| *n += 1);
        Guard(self.clone())
    }

    pub(crate) fn count(&self) -> usize {
        *self.0.borrow()
    }

    async fn drained(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0 .0.send_modify(|n| *n -= 1);
    }
}

/// One run of a daemon: what its registry entry says of its process.
type Incarnation = (u32, Option<u64>, i64);

fn incarnation(info: &DaemonInfo) -> Incarnation {
    (info.pid, info.pid_start, info.started_at)
}

/// The server's connection to a running daemon of a membership.
struct WatchRef {
    epoch: u64,
    of: Incarnation,
    task: JoinHandle<()>,
    /// Lines to send it (`web_event`).
    out: mpsc::UnboundedSender<Vec<u8>>,
    /// The greatest summary `rev` applied from it.
    summary_rev: u64,
}

struct Member {
    id: String,
    generation: u64,
    workspace: String,
    project: Option<String>,
    state: WorkspaceState,
    detail: String,
    summary: Option<WorkspaceSummary>,
    joined_at: i64,
    /// The registry entry the watch is on (while `running`).
    published: Option<DaemonInfo>,
    watch: Option<WatchRef>,
    /// The next wait before a published daemon that did not answer is
    /// dialled again, and the timer armed for it.
    retry_delay: Duration,
    retry: Option<JoinHandle<()>>,
    revoked: watch::Sender<bool>,
    gauge: Arc<Gauge>,
    /// The web connections admitted under this generation.
    conns: Vec<u64>,
}

impl Member {
    fn set(&mut self, state: WorkspaceState, detail: impl Into<String>) -> bool {
        let detail = detail.into();
        if self.state == state && self.detail == detail {
            return false;
        }
        self.state = state;
        self.detail = detail;
        true
    }

    fn entry(&self) -> WorkspaceEntry {
        WorkspaceEntry {
            id: self.id.clone(),
            generation: self.generation,
            daemon: match (&self.state, &self.published) {
                (WorkspaceState::Running, Some(info)) => Some(DaemonRef {
                    pid: info.pid,
                    started_at: info.started_at,
                }),
                _ => None,
            },
            project: self.project.clone(),
            workspace: self.workspace.clone(),
            state: self.state,
            detail: self.detail.clone(),
            summary: self.summary.clone(),
            joined_at: self.joined_at,
        }
    }

    fn stop_retry(&mut self) {
        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
        self.retry_delay = RETRY_FIRST;
    }
}

#[derive(Default)]
struct Table {
    /// In the order they were joined.
    members: Vec<Member>,
    /// Taken out by the operator: no rescan joins them again.
    removed: BTreeSet<String>,
    generations: u64,
    epochs: u64,
    rev: u64,
    others: Vec<OtherSession>,
    /// The server is stopping: nothing more is admitted or joined.
    closed: bool,
}

impl Table {
    fn member(&mut self, id: &str) -> Option<&mut Member> {
        self.members.iter_mut().find(|m| m.id == id)
    }
}

/// What the reconciler is asked to do.
enum Event {
    Reconcile(String),
    WatchEnded { id: String, epoch: u64 },
    Join(Join, oneshot::Sender<Result<Vec<String>, RpcError>>),
}

/// The ways of joining (§multi-join).
pub(crate) enum Join {
    /// One workspace, by its state directory.
    One(PathBuf),
    /// Every running session.
    All,
    /// The memberships of a server being replaced, running or not.
    Restore(Vec<PathBuf>),
}

/// An open that passed admission: it counts for its generation from here.
pub(crate) struct Admission {
    pub id: String,
    pub info: DaemonInfo,
    pub revoked: watch::Receiver<bool>,
    pub guard: Guard,
}

/// Test hooks: a hold after a scan read the registry, counters.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Hooks {
    pub scan_hold: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    pub scans: std::sync::atomic::AtomicU64,
    pub reconciles: Mutex<Vec<String>>,
    pub drained_within: Mutex<Option<Duration>>,
}

pub(crate) struct Hub {
    table: Mutex<Table>,
    /// The listing's revision, for the index connections to watch.
    listing: watch::Sender<u64>,
    /// How many memberships are `running`, for the server's linger.
    running: watch::Sender<usize>,
    events: mpsc::UnboundedSender<Event>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    rescan_wanted: AtomicBool,
    wake: Notify,
    daemons: Arc<dyn Daemons>,
    /// `[web] join = "all"`: every rescan joins what runs.
    join_all: bool,
    /// The dashboard's origin, told to each daemon by its watch.
    url: Mutex<String>,
    listing_rescan_at: Mutex<Option<tokio::time::Instant>>,
    #[cfg(test)]
    pub(crate) hooks: Hooks,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    pub(crate) fn new(daemons: Arc<dyn Daemons>, join_all: bool) -> Arc<Self> {
        let (events, events_rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            table: Mutex::new(Table::default()),
            listing: watch::channel(0).0,
            running: watch::channel(0).0,
            events,
            events_rx: Mutex::new(Some(events_rx)),
            rescan_wanted: AtomicBool::new(false),
            wake: Notify::new(),
            daemons,
            join_all,
            url: Mutex::new(String::new()),
            listing_rescan_at: Mutex::new(None),
            #[cfg(test)]
            hooks: Hooks::default(),
        })
    }

    /// Start the task that reconciles; it ends with the hub's last owner.
    pub(crate) fn start(self: &Arc<Self>, url: String) -> JoinHandle<()> {
        *lock(&self.url) = url;
        let events = lock(&self.events_rx).take().expect("started once");
        tokio::spawn(reconciler(self.clone(), events))
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        lock(&self.table)
    }

    /// The listing changed: a new revision, told to whoever watches.
    fn changed(&self, t: &mut Table) {
        t.rev += 1;
        self.listing.send_replace(t.rev);
        let running = t
            .members
            .iter()
            .filter(|m| m.state == WorkspaceState::Running)
            .count();
        self.running.send_if_modified(|n| {
            let changed = *n != running;
            *n = running;
            changed
        });
    }

    /// The listing as it is now, with its revision.
    pub(crate) fn listing(&self) -> Workspaces {
        let t = self.table();
        Workspaces {
            rev: t.rev,
            joined: t.members.iter().map(Member::entry).collect(),
            others: t.others.clone(),
        }
    }

    /// The revision, as it changes.
    pub(crate) fn listing_changes(&self) -> watch::Receiver<u64> {
        self.listing.subscribe()
    }

    /// How many memberships are `running`, as it changes.
    pub(crate) fn running(&self) -> watch::Receiver<usize> {
        self.running.subscribe()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.table().members.is_empty()
    }

    /// Ask for a rescan: every joined workspace is reconciled against a
    /// reading of the registry that begins after this call.
    pub(crate) fn rescan(&self) {
        self.rescan_wanted.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// A page asked for the listing: a rescan, at most one a second.
    pub(crate) fn asked_for_listing(&self) {
        let now = tokio::time::Instant::now();
        let mut at = lock(&self.listing_rescan_at);
        if at.is_some_and(|at| now < at + LISTING_RESCAN_EVERY) {
            return;
        }
        *at = Some(now);
        drop(at);
        self.rescan();
    }

    /// Reconcile one workspace (a dial failed, a connection's daemon side
    /// ended).
    pub(crate) fn reconcile(&self, id: &str) {
        let _ = self.events.send(Event::Reconcile(id.to_string()));
    }

    /// Join, through the reconciler: the ids joined (and reconciled).
    pub(crate) async fn join(&self, how: Join) -> Result<Vec<String>, RpcError> {
        let (done, answer) = oneshot::channel();
        self.events
            .send(Event::Join(how, done))
            .map_err(|_| RpcError::unavailable("the dashboard server is stopping"))?;
        answer
            .await
            .map_err(|_| RpcError::unavailable("the dashboard server is stopping"))?
    }

    /// Admission of a workspace connection, under the table's lock: the id
    /// must be joined and `running`; from here the open counts for that
    /// membership's generation.
    pub(crate) fn admit(&self, id: &str, conn: u64) -> Result<Admission, RpcError> {
        let mut t = self.table();
        if t.closed {
            return Err(RpcError::unavailable("the dashboard is stopping"));
        }
        let Some(m) = t.member(id) else {
            return Err(RpcError::not_found(format!(
                "workspace {id} is not in this dashboard: run `crew web` in it"
            )));
        };
        let name = m.project.clone().unwrap_or_else(|| m.workspace.clone());
        match m.state {
            WorkspaceState::Running => {}
            WorkspaceState::Stopped => {
                return Err(RpcError::unavailable(format!(
                    "{name} is not running: start it (`crew run` in {})",
                    m.workspace
                )))
            }
            WorkspaceState::Unreachable => {
                return Err(RpcError::unavailable(format!(
                    "{name} cannot be reached: {}",
                    m.detail
                )))
            }
            WorkspaceState::Incompatible => {
                return Err(RpcError::new("protocol", m.detail.clone()));
            }
        }
        let Some(info) = m.published.clone() else {
            return Err(RpcError::unavailable(format!("{name} is not running")));
        };
        m.conns.push(conn);
        Ok(Admission {
            id: id.to_string(),
            info,
            revoked: m.revoked.subscribe(),
            guard: m.gauge.hold(),
        })
    }

    /// A workspace connection ended.
    pub(crate) fn left(&self, id: &str, conn: u64) {
        if let Some(m) = self.table().member(id) {
            m.conns.retain(|c| *c != conn);
        }
    }

    /// Dial the daemon of an admitted open for a browser.
    pub(crate) async fn dial(&self, admission: &Admission) -> Result<Dialled, DialError> {
        self.daemons.dial(&admission.info, true).await
    }

    /// Something every joined workspace's log should hold.
    pub(crate) fn web_event(&self, kind: WebEventKind, count: u32) {
        let line = request_line(1, Call::WebEvent { kind, count });
        for m in &self.table().members {
            if let Some(w) = &m.watch {
                let _ = w.out.send(line.clone());
            }
        }
    }

    /// A summary from the watch `epoch` of `id`: applied if that is still
    /// its watch and the summary is newer than the last from it.
    fn apply_summary(&self, id: &str, epoch: u64, summary: WorkspaceSummary) {
        let mut t = self.table();
        let Some(m) = t.member(id) else { return };
        let Some(w) = m.watch.as_mut().filter(|w| w.epoch == epoch) else {
            return;
        };
        if summary.rev <= w.summary_rev {
            return;
        }
        w.summary_rev = summary.rev;
        if summary.project.is_some() {
            m.project = summary.project.clone();
        }
        m.summary = Some(summary);
        self.changed(&mut t);
    }

    /// Take a workspace out (M9): revoke its generation, end whatever of it
    /// holds the daemon (cancelling, and awaiting, what does not let go
    /// within a second), and only then answer. `which`: its id or its state
    /// directory.
    pub(crate) async fn remove(&self, which: &str, web: &super::Web) -> Result<(), RpcError> {
        let id = id_for(which);
        // 1. Revoked, under the table's lock.
        let mut m = {
            let mut t = self.table();
            let Some(at) = t.members.iter().position(|m| m.id == id) else {
                return Err(RpcError::not_found(format!(
                    "{which} is not in this dashboard"
                )));
            };
            let m = t.members.remove(at);
            t.removed.insert(id.clone());
            // Still running: one of the others again.
            if m.state == WorkspaceState::Running {
                if let Some(info) = &m.published {
                    t.others.push(OtherSession {
                        id: id.clone(),
                        project: m.project.clone().or(info.project.clone()),
                        workspace: m.workspace.clone(),
                    });
                }
            }
            m.revoked.send_replace(true);
            self.changed(&mut t);
            m
        };
        m.stop_retry();
        // 2-3. Pending opens and connections see the revocation; the watch
        // is closed.
        let watch = m.watch.take().map(|w| {
            w.task.abort();
            w.task
        });
        // 4. Drained, or cancelled and awaited, then drained.
        if tokio::time::timeout(LET_GO_WITHIN, m.gauge.drained())
            .await
            .is_err()
        {
            web.cancel(&m.conns).await;
        }
        if let Some(watch) = watch {
            let _ = watch.await;
        }
        #[cfg(test)]
        let within = lock(&self.hooks.drained_within).unwrap_or(DRAINED_WITHIN);
        #[cfg(not(test))]
        let within = DRAINED_WITHIN;
        if tokio::time::timeout(within, m.gauge.drained())
            .await
            .is_err()
        {
            tracing::error!(
                "{id} was taken out of the dashboard, but {} of its connections did not end",
                m.gauge.count()
            );
            return Err(RpcError::internal(format!(
                "{id} was taken out, but some of its connections did not end; it stays out"
            )));
        }
        Ok(())
    }

    /// The server stops: nothing more is admitted or joined, and every
    /// generation is revoked. The watches go last (`close_watches`).
    pub(crate) fn close(&self) {
        let mut t = self.table();
        t.closed = true;
        for m in &mut t.members {
            m.revoked.send_replace(true);
            m.stop_retry();
        }
    }

    /// The watches end, each awaited.
    pub(crate) async fn close_watches(&self) {
        let tasks: Vec<JoinHandle<()>> = {
            let mut t = self.table();
            t.members
                .iter_mut()
                .filter_map(|m| m.watch.take().map(|w| w.task))
                .collect()
        };
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Reconcile one membership with what the registry publishes for it.
    async fn reconcile_one(self: &Arc<Self>, id: &str, published: Option<DaemonInfo>) {
        #[cfg(test)]
        lock(&self.hooks.reconciles).push(id.to_string());
        let dial = {
            let mut t = self.table();
            let Some(m) = t.member(id) else { return };
            let mut changed = false;
            let dial = match published {
                None => {
                    if let Some(w) = m.watch.take() {
                        w.task.abort();
                    }
                    m.stop_retry();
                    changed |= m.set(WorkspaceState::Stopped, "");
                    if m.published.take().is_some() {
                        changed = true;
                    }
                    None
                }
                Some(info) => {
                    if m.watch.as_ref().is_some_and(|w| w.of == incarnation(&info)) {
                        None
                    } else {
                        // None, or one on an earlier daemon: superseded,
                        // whatever it still has to say.
                        if let Some(w) = m.watch.take() {
                            w.task.abort();
                        }
                        match self.daemons.check(&info) {
                            Ok(()) => Some((info, m.generation)),
                            Err(why) => {
                                m.stop_retry();
                                changed |= m.set(WorkspaceState::Unreachable, why);
                                None
                            }
                        }
                    }
                }
            };
            if changed {
                self.changed(&mut t);
            }
            dial
        };
        let Some((info, generation)) = dial else {
            return;
        };
        let dialled = self.watch_dial(&info).await;
        let mut t = self.table();
        let epoch = t.epochs + 1;
        let Some(m) = t.member(id).filter(|m| m.generation == generation) else {
            return;
        };
        if m.watch.as_ref().is_some_and(|w| w.of == incarnation(&info)) {
            return;
        }
        match dialled {
            Ok((dialled, summary)) => {
                let (out, out_rx) = mpsc::unbounded_channel();
                let task = tokio::spawn(watching(
                    self.clone(),
                    id.to_string(),
                    epoch,
                    m.gauge.hold(),
                    dialled,
                    out_rx,
                ));
                m.watch = Some(WatchRef {
                    epoch,
                    of: incarnation(&info),
                    task,
                    out,
                    summary_rev: summary.rev,
                });
                m.stop_retry();
                m.project = summary
                    .project
                    .clone()
                    .or(info.project.clone())
                    .or(m.project.take());
                m.summary = Some(summary);
                m.published = Some(info);
                m.set(WorkspaceState::Running, "");
                t.epochs = epoch;
            }
            Err(DialError::Protocol(_)) => {
                m.stop_retry();
                m.published = None;
                m.set(
                    WorkspaceState::Incompatible,
                    "started by another version of crew: restart the team",
                );
            }
            Err(DialError::Refused(why)) => {
                m.stop_retry();
                m.published = None;
                m.set(WorkspaceState::Unreachable, why);
            }
            Err(DialError::NoAnswer(why)) => {
                m.published = None;
                m.set(WorkspaceState::Unreachable, why);
                // Published but not answering (going, coming, hanging): the
                // registry is asked again shortly, while it publishes it.
                if m.retry.as_ref().is_none_or(|r| r.is_finished()) {
                    let delay = m.retry_delay;
                    m.retry_delay = (delay * 2).min(RETRY_MAX);
                    let (events, id) = (self.events.clone(), id.to_string());
                    m.retry = Some(tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = events.send(Event::Reconcile(id));
                    }));
                }
            }
        }
        self.changed(&mut t);
    }

    /// Dial a daemon for the server's watch: `hello`, `web_attach`,
    /// `summary {watch: true}`.
    async fn watch_dial(
        &self,
        info: &DaemonInfo,
    ) -> Result<(Dialled, WorkspaceSummary), DialError> {
        let mut dialled = self.daemons.dial(info, false).await?;
        let url = lock(&self.url).clone();
        let answered = tokio::time::timeout(DIAL_WITHIN, async {
            let attach = call(&mut dialled, 1, Call::WebAttach { url }).await?;
            if let Some(error) = attach.error {
                return Err(error.message);
            }
            let summary = call(&mut dialled, 2, Call::Summary { watch: true }).await?;
            summary
                .result
                .and_then(|r| serde_json::from_value::<WorkspaceSummary>(r).ok())
                .ok_or_else(|| "it gave no summary".to_string())
        })
        .await;
        match answered {
            Ok(Ok(summary)) => Ok((dialled, summary)),
            Ok(Err(why)) => Err(DialError::NoAnswer(why)),
            Err(_) => Err(DialError::NoAnswer(format!(
                "it did not answer within {} s",
                DIAL_WITHIN.as_secs()
            ))),
        }
    }

    /// One scan: the registry read once, every membership against it; under
    /// `join = "all"` what runs and was not taken out is joined first.
    async fn scan(self: &Arc<Self>) {
        #[cfg(test)]
        self.hooks
            .scans
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let published = self.daemons.sessions().await;
        #[cfg(test)]
        {
            let hold = lock(&self.hooks.scan_hold).clone();
            if let Some(hold) = hold {
                if let Ok(permit) = hold.acquire().await {
                    permit.forget();
                }
            }
        }
        let ids: Vec<String> = {
            let mut t = self.table();
            if t.closed {
                return;
            }
            if self.join_all {
                for info in &published {
                    let id = daemons::id_of(info);
                    if t.removed.contains(&id) || t.member(&id).is_some() {
                        continue;
                    }
                    if self.daemons.check(info).is_ok() && t.members.len() < MAX_JOINED {
                        self.add(
                            &mut t,
                            &id,
                            Path::new(&info.state_dir),
                            info.project.clone(),
                        );
                    }
                }
            }
            let others: Vec<OtherSession> = published
                .iter()
                .filter(|info| {
                    let id = daemons::id_of(info);
                    !t.members.iter().any(|m| m.id == id)
                })
                .map(|info| OtherSession {
                    id: daemons::id_of(info),
                    project: info.project.clone(),
                    workspace: info.workspace.clone(),
                })
                .collect();
            if others != t.others {
                t.others = others;
                self.changed(&mut t);
            }
            t.members.iter().map(|m| m.id.clone()).collect()
        };
        for id in ids {
            let info = published
                .iter()
                .find(|info| daemons::id_of(info) == id)
                .cloned();
            self.reconcile_one(&id, info).await;
        }
    }

    /// A new membership, `stopped` until reconciled.
    fn add(&self, t: &mut Table, id: &str, state_dir: &Path, project: Option<String>) {
        // Joined: no longer one of the others.
        t.others.retain(|o| o.id != id);
        t.generations += 1;
        t.members.push(Member {
            id: id.to_string(),
            generation: t.generations,
            workspace: daemons::workspace_of(state_dir),
            project,
            state: WorkspaceState::Stopped,
            detail: String::new(),
            summary: None,
            joined_at: now_ms(),
            published: None,
            watch: None,
            retry_delay: RETRY_FIRST,
            retry: None,
            revoked: watch::channel(false).0,
            gauge: Gauge::new(),
            conns: Vec::new(),
        });
        self.changed(t);
    }

    /// Join, as the reconciler: the ids this call joined.
    async fn join_now(self: &Arc<Self>, how: Join) -> Result<Vec<String>, RpcError> {
        let published = self.daemons.sessions().await;
        let find = |id: &str| published.iter().find(|i| daemons::id_of(i) == id).cloned();
        // What to join: (id, state dir, its entry if it runs).
        let mut wanted: Vec<(String, PathBuf, Option<DaemonInfo>)> = Vec::new();
        match how {
            Join::One(state_dir) => {
                let id = paths::session_id(&state_dir);
                // 1. A live registry entry; 2. the socket the server derives.
                let info = find(&id).ok_or_else(|| {
                    RpcError::unavailable(format!(
                        "no crew daemon is running for {}",
                        daemons::workspace_of(&state_dir)
                    ))
                })?;
                self.daemons.check(&info).map_err(RpcError::invalid)?;
                wanted.push((id, state_dir, Some(info)));
            }
            Join::All => {
                for info in &published {
                    if self.daemons.check(info).is_ok() {
                        wanted.push((
                            daemons::id_of(info),
                            PathBuf::from(&info.state_dir),
                            Some(info.clone()),
                        ));
                    }
                }
            }
            Join::Restore(dirs) => {
                for state_dir in dirs {
                    let id = paths::session_id(&state_dir);
                    let info = find(&id).filter(|info| self.daemons.check(info).is_ok());
                    wanted.push((id, state_dir, info));
                }
            }
        }
        let mut joined = Vec::new();
        {
            let mut t = self.table();
            if t.closed {
                return Err(RpcError::unavailable("the dashboard server is stopping"));
            }
            for (id, state_dir, info) in &wanted {
                // An explicit command takes it out of the removed set.
                t.removed.remove(id);
                if t.member(id).is_some() {
                    continue;
                }
                if t.members.len() >= MAX_JOINED {
                    return Err(RpcError::conflict(format!(
                        "{MAX_JOINED} workspaces are in this dashboard: `crew web --remove` one first"
                    )));
                }
                let project = info.as_ref().and_then(|i| i.project.clone());
                self.add(&mut t, id, state_dir, project);
                joined.push(id.clone());
            }
        }
        for (id, _, info) in wanted {
            self.reconcile_one(&id, info).await;
        }
        Ok(joined)
    }
}

/// A workspace named on the command line: its id, or a state directory.
fn id_for(which: &str) -> String {
    if which.len() == 16 && which.bytes().all(|b| b.is_ascii_hexdigit()) {
        which.to_ascii_lowercase()
    } else {
        paths::session_id(Path::new(which))
    }
}

fn request_line(id: u64, call: Call) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Request { id, call }).unwrap_or_default();
    line.push(b'\n');
    line
}

/// One call on a fresh daemon connection, its reply (no pushes come before
/// it: the watch never subscribes).
async fn call(dialled: &mut Dialled, id: u64, c: Call) -> Result<ServerFrame, String> {
    dialled
        .writer
        .write_all(&request_line(id, c))
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    loop {
        daemons::read_line(&mut dialled.lines, &mut buf, MAX_LINE)
            .await
            .ok_or_else(|| "it closed the connection".to_string())?;
        let frame: ServerFrame =
            serde_json::from_slice(&buf).map_err(|_| "it sent something else".to_string())?;
        if frame.id == Some(id) {
            return Ok(frame);
        }
    }
}

/// The watch on one daemon: summaries come in, `web_event`s go out; its end
/// is the news.
async fn watching(
    hub: Arc<Hub>,
    id: String,
    epoch: u64,
    guard: Guard,
    mut dialled: Dialled,
    mut out: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            line = daemons::read_line(&mut dialled.lines, &mut buf, MAX_LINE) => {
                if line.is_none() {
                    break;
                }
                let Ok(frame) = serde_json::from_slice::<ServerFrame>(&buf) else { continue };
                if let Some(Push::Summary(summary)) = frame.push {
                    hub.apply_summary(&id, epoch, summary);
                }
            }
            line = out.recv() => {
                let Some(line) = line else { break };
                if dialled.writer.write_all(&line).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(dialled);
    drop(guard);
    let _ = hub.events.send(Event::WatchEnded { id, epoch });
}

/// The one task that changes what the server believes of its memberships.
async fn reconciler(hub: Arc<Hub>, mut events: mpsc::UnboundedReceiver<Event>) {
    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => Some(event),
                None => return,
            },
            _ = hub.wake.notified() => None,
        };
        match event {
            None => {}
            Some(Event::Reconcile(id)) => {
                let published = published(&hub, &id).await;
                hub.reconcile_one(&id, published).await;
            }
            Some(Event::WatchEnded { id, epoch }) => {
                let current = {
                    let mut t = hub.table();
                    match t.member(&id) {
                        Some(m) if m.watch.as_ref().is_some_and(|w| w.epoch == epoch) => {
                            m.watch = None;
                            true
                        }
                        // The end of a superseded watch: not news.
                        _ => false,
                    }
                };
                if current {
                    let published = published(&hub, &id).await;
                    hub.reconcile_one(&id, published).await;
                }
            }
            Some(Event::Join(how, done)) => {
                let _ = done.send(hub.join_now(how).await);
            }
        }
        // Each request is followed by a reading of the registry that began
        // after it: the flag is cleared before the scan reads.
        while hub.rescan_wanted.swap(false, Ordering::SeqCst) {
            hub.scan().await;
        }
    }
}

/// What the registry publishes for `id` now.
async fn published(hub: &Hub, id: &str) -> Option<DaemonInfo> {
    hub.daemons
        .sessions()
        .await
        .into_iter()
        .find(|info| daemons::id_of(info) == id)
}

#[cfg(test)]
mod tests;
