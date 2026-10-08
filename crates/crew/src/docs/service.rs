//! Authenticated observer service port. P2 authenticates and negotiates before
//! calling this API; this layer rechecks run, connection, epoch and source fences.
//! It holds no planner/model/VCS mutation locks and owns no agent-delivery path.

use super::{
    bounded_size,
    config::{Policy, Relative},
    diff, history,
    index::{self, Index, Registration},
    markdown,
    render::{self, Content, Rendered},
    watch,
};
use crate::daemon::features::{
    Caller, ConnCtx, ConnId, FeatureError, FeatureService, FeatureWatch, Readiness, RunCtx,
};
use base64::Engine;
use crew_core::rpc::docs::*;
use crew_core::rpc::features::{
    Feature, FeatureUnavailable, Id, List, PageRequest, Text, Unwatch, UnwatchResult, Wide,
};
use crew_core::rpc::{Call, RpcError};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

// Conservative retained-memory partitions sum below the design's 128 MiB cap.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CURSOR_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const MAX_VERSIONS: usize = 256;
const MAX_RECORDED: usize = 128;
const MAX_CURSORS: usize = 4;

#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<State>,
    reads: Arc<Semaphore>,
    history: Arc<Semaphore>,
    worker: AtomicBool,
}
struct State {
    active: bool,
    run: RunCtx,
    index: Index,
    connections: BTreeMap<Id, Connection>,
    clock: u64,
    cache_bytes: usize,
    cursor_bytes: usize,
    native: Option<watch::Native>,
    watch_mode: DocWatchMode,
    dirty: Arc<AtomicBool>,
    last_scan: Instant,
    last_publish: Instant,
    image_hashes: BTreeMap<(Id, Relative), Option<Id>>,
    image_cursor: usize,
    image_changed: BTreeSet<(Id, Id)>,
    status_dirty: bool,
}
struct Connection {
    context: ConnCtx,
    versions: BTreeMap<Id, Cached>,
    recorded: BTreeMap<Id, Recorded>,
    cursors: BTreeMap<Id, Cursor>,
    watch: Option<Watch>,
}
struct Cached {
    value: Arc<Rendered>,
    age: u64,
}
#[derive(Clone)]
struct Recorded {
    version: DocVersion,
    reader: Arc<history::Reader>,
    path: Relative,
}
struct Watch {
    id: Id,
    epoch: Id,
    revision: Wide,
    selected: Vec<DocRef>,
    queue: Arc<watch::Queue>,
}
struct Cursor {
    epoch: Id,
    created: Instant,
    charge: usize,
    data: CursorData,
}
enum CursorData {
    Index {
        page: DocIndexPage,
        entries: Arc<Vec<DocEntry>>,
        offset: usize,
    },
    History(HistoryCursor),
}
#[derive(Clone)]
struct Head {
    reference: DocRef,
    commit: Id,
    reader: Arc<history::Reader>,
    path: Relative,
}
#[derive(Clone)]
struct HistoryCursor {
    doc_id: Id,
    heads: Vec<Head>,
    positions: Vec<usize>,
    pending: Vec<VecDeque<history::Record>>,
    done: Vec<bool>,
    seen: BTreeSet<Id>,
    selection: Option<BTreeSet<Id>>,
}
type ImageDependencies = BTreeMap<(Id, Relative), (Option<Id>, BTreeSet<(Id, Id)>)>;

struct Plan {
    epoch: Id,
    path: Relative,
    policy: Policy,
    content: Content,
    version: DocVersion,
    links: render::Links,
}

impl Service {
    pub fn new(run: RunCtx, policy: Policy) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    active: true,
                    run,
                    index: Index::new(policy),
                    connections: BTreeMap::new(),
                    clock: 0,
                    cache_bytes: 0,
                    cursor_bytes: 0,
                    native: None,
                    watch_mode: DocWatchMode::None,
                    dirty: Arc::new(AtomicBool::new(false)),
                    last_scan: Instant::now(),
                    last_publish: Instant::now() - Duration::from_secs(1),
                    image_hashes: BTreeMap::new(),
                    image_cursor: 0,
                    image_changed: BTreeSet::new(),
                    status_dirty: false,
                }),
                reads: Arc::new(Semaphore::new(2)),
                history: Arc::new(Semaphore::new(1)),
                worker: AtomicBool::new(false),
            }),
        }
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Host lifecycle port. This never discovers roots from text or scans .crew.
    /// Call outside the async executor if registration can touch slow storage.
    pub fn replace_sources(&self, sources: Vec<Registration>) -> Result<(), DocFailureCode> {
        let mut state = self.state();
        let before: Vec<_> = state
            .index
            .sources
            .values()
            .map(|s| (s.record.id.clone(), s.record.generation))
            .collect();
        if state.index.register(sources)? {
            let after: Vec<_> = state
                .index
                .sources
                .values()
                .map(|s| (s.record.id.clone(), s.record.generation))
                .collect();
            if before != after {
                state.index.epoch = index::id();
                state.finish_watches(DocChangeKind::Availability);
                state.invalidate_leases();
            } else {
                state.publish(true);
            }
            state.dirty.store(true, Ordering::Release);
            state.sync_native();
        }
        Ok(())
    }
    /// Parse before replacement: a malformed update leaves the last policy live.
    pub fn configure<'a>(
        &self,
        layers: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), &'static str> {
        let policy = match Policy::from_toml_layers(layers) {
            Ok(p) => p,
            Err(e) => {
                self.state().index.config_error = true;
                return Err(e);
            }
        };
        self.replace_policy(policy);
        Ok(())
    }
    pub fn replace_policy(&self, policy: Policy) {
        let mut state = self.state();
        if state.index.set_policy(policy) {
            state.finish_watches(DocChangeKind::Policy);
            state.invalidate_leases();
            state.image_hashes.clear();
        }
        state.index.config_error = false;
    }

    fn start_worker(&self) {
        if self.inner.worker.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let service = Self { inner };
                {
                    let state = service.state();
                    if !state.connections.values().any(|c| c.watch.is_some()) {
                        service.inner.worker.store(false, Ordering::Release);
                        return;
                    }
                }
                let Ok(permit) = service.inner.reads.clone().try_acquire_owned() else {
                    continue;
                };
                let _ = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    service.tick();
                })
                .await;
            }
        });
    }
    fn tick(&self) {
        let mut state = self.state();
        let dead: Vec<_> = state
            .connections
            .iter()
            .filter(|(_, c)| c.context.cancellation.is_cancelled())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            state.disconnect(&id);
        }
        if !state.connections.values().any(|c| c.watch.is_some()) {
            state.native = None;
            state.watch_mode = DocWatchMode::None;
            return;
        }
        let now = Instant::now();
        let period = if state.watch_mode == DocWatchMode::Events {
            Duration::from_secs(15)
        } else {
            Duration::from_secs(5)
        };
        if state.index.phase == DocPhase::Ready
            && ((state.dirty.load(Ordering::Acquire)
                && now.duration_since(state.last_scan) >= Duration::from_secs(1))
                || now.duration_since(state.last_scan) >= period)
        {
            state.dirty.store(false, Ordering::Release);
            state.index.rescan();
            state.last_scan = now;
        }
        state.advance();
        state.sync_native();
        state.check_images();
        if now.duration_since(state.last_publish) >= Duration::from_secs(1) {
            state.publish(false);
            state.last_publish = now;
        }
    }

    async fn execute(&self, context: &ConnCtx, call: Call) -> Result<Value, RpcError> {
        if context.cancellation.is_cancelled() {
            return Err(RpcError::forbidden("Docs connection is closed"));
        }
        let permit = self
            .inner
            .reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| failure_error(DocFailureCode::Busy))?;
        let history_permit = if matches!(
            call,
            Call::DocsHistory(_)
                | Call::DocsDiff(_)
                | Call::DocsRead(DocsRead {
                    version: DocVersionSelector::Id(_),
                    ..
                })
        ) {
            Some(
                self.inner
                    .history
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| failure_error(DocFailureCode::Busy))?,
            )
        } else {
            None
        };
        let service = self.clone();
        let context = context.clone();
        let cancel = context.cancellation.clone();
        // Permits live inside the blocking closure, even if its awaiter goes away.
        let job = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _history = history_permit;
            service.dispatch(&context, call)
        });
        tokio::select! {_ = cancel.cancelled()=>Err(RpcError::forbidden("Docs connection is closed")),result=job=>result.map_err(|_|RpcError::new("internal","Docs worker failed"))?}
    }
    fn dispatch(&self, context: &ConnCtx, call: Call) -> Result<Value, RpcError> {
        {
            let mut state = self.state();
            state.admit(context)?;
        }
        match call {
            Call::DocsIndex(request) => {
                let mut state = self.state();
                state.advance();
                if state.last_publish.elapsed() >= Duration::from_secs(1) {
                    state.publish(false);
                    state.last_publish = Instant::now();
                }
                let page = state.index_page(context, &request).map_err(failure_error)?;
                encode(&page)
            }
            Call::DocsWatch(request) => {
                let mut state = self.state();
                state.watch(context, request).and_then(|r| encode(&r))
            }
            Call::DocsUnwatch(request) => {
                let mut state = self.state();
                encode(&state.unwatch(context, request))
            }
            Call::DocsRead(request) => {
                let document = Some(request.document.clone());
                encode(&match self.read(context, request) {
                    Ok(snapshot) => DocReadResult::Available(Box::new(snapshot)),
                    Err(code) => DocReadResult::Unavailable(failure(document, code)),
                })
            }
            Call::DocsAsset(request) => {
                let state = self.state();
                encode(&match state.asset(context, request) {
                    Ok(chunk) => DocAssetResult::Available(Box::new(chunk)),
                    Err(code) => DocAssetResult::Unavailable(failure(None, code)),
                })
            }
            Call::DocsHistory(request) => encode(&match self.history_page(context, request) {
                Ok(page) => DocHistoryResult::Available(Box::new(page)),
                Err(code) => DocHistoryResult::Unavailable(failure(None, code)),
            }),
            Call::DocsDiff(request) => encode(&match self.diff(context, request) {
                Ok(value) => DocDiffResult::Available(Box::new(value)),
                Err(code) => DocDiffResult::Unavailable(failure(None, code)),
            }),
            _ => Err(RpcError::invalid("not a Docs request")),
        }
    }
    fn diff(&self, context: &ConnCtx, request: DocsDiff) -> Result<DocDiff, DocFailureCode> {
        let documents = {
            let state = self.state();
            state.check(context)?;
            let connection = state
                .connections
                .get(&context.connection_epoch)
                .ok_or(DocFailureCode::SnapshotExpired)?;
            [&request.left, &request.right]
                .into_iter()
                .map(|id| {
                    connection
                        .versions
                        .get(id)
                        .map(|c| c.value.snapshot.version.document.clone())
                        .or_else(|| {
                            connection
                                .recorded
                                .get(id)
                                .map(|c| c.version.document.clone())
                        })
                        .ok_or(DocFailureCode::SnapshotExpired)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        if documents[0].doc_id != documents[1].doc_id {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let left = self.read(
            context,
            DocsRead {
                document: documents[0].clone(),
                version: DocVersionSelector::Id(request.left),
            },
        )?;
        let right = self.read(
            context,
            DocsRead {
                document: documents[1].clone(),
                version: DocVersionSelector::Id(request.right),
            },
        )?;
        self.state().check(context)?;
        diff::compare(
            left.version.clone(),
            &left.markdown,
            right.version.clone(),
            &right.markdown,
        )
    }
    fn read(&self, context: &ConnCtx, request: DocsRead) -> Result<DocSnapshot, DocFailureCode> {
        let plan = {
            let state = self.state();
            state.check(context)?;
            state.index.resolve(&request.document)?;
            if let DocVersionSelector::Id(id) = &request.version {
                if let Some(cached) = state
                    .connections
                    .get(&context.connection_epoch)
                    .and_then(|c| c.versions.get(id))
                {
                    if cached.value.snapshot.version.document != request.document {
                        return Err(DocFailureCode::ForbiddenPath);
                    }
                    return Ok(cached.value.snapshot.clone());
                }
            }
            state.plan(context, request)?
        };
        if context.cancellation.is_cancelled() {
            return Err(DocFailureCode::SnapshotExpired);
        }
        let bytes = plan
            .content
            .read(&plan.path, &plan.policy, MAX_MARKDOWN_BYTES)?;
        let markdown = String::from_utf8(bytes).map_err(|_| DocFailureCode::InvalidUtf8)?;
        let mut version = (*plan.version).clone();
        if version.kind == DocVersionKind::Recorded
            && (version.markdown_hash != markdown::hash(markdown.as_bytes())
                || version.bytes != markdown.len() as u32)
        {
            return Err(DocFailureCode::ObjectMissing);
        }
        version.markdown_hash = markdown::hash(markdown.as_bytes());
        version.bytes = markdown.len() as u32;
        let version = DocVersion::try_from(version).map_err(|_| DocFailureCode::TooLarge)?;
        let rendered = render::capture(
            plan.epoch.clone(),
            version,
            markdown,
            plan.path,
            plan.policy,
            plan.content,
            plan.links,
        )?;
        let mut state = self.state();
        state.check(context)?;
        if state.index.epoch != plan.epoch {
            return Err(DocFailureCode::SnapshotExpired);
        }
        state.index.resolve(&rendered.snapshot.version.document)?;
        let snapshot = rendered.snapshot.clone();
        state.retain(context, rendered)?;
        Ok(snapshot)
    }

    fn history_page(
        &self,
        context: &ConnCtx,
        request: DocsHistory,
    ) -> Result<DocHistoryPage, DocFailureCode> {
        request
            .page
            .validate(50)
            .map_err(|_| DocFailureCode::TooLarge)?;
        let limit = request.page.limit.unwrap_or(50) as usize;
        let (epoch, mut cursor) = {
            let mut state = self.state();
            state.check(context)?;
            if let Some(token) = &request.page.cursor {
                let cursor = state.take_cursor(context, token)?;
                let CursorData::History(cursor) = cursor.data else {
                    return Err(DocFailureCode::StaleCursor);
                };
                if cursor.doc_id != request.doc_id
                    || cursor.selection
                        != request
                            .source_ids
                            .as_ref()
                            .map(|ids| ids.iter().cloned().collect())
                {
                    return Err(DocFailureCode::StaleCursor);
                }
                (state.index.epoch.clone(), cursor)
            } else {
                let (path, entry) = state
                    .index
                    .known(&request.doc_id)
                    .ok_or(DocFailureCode::ForbiddenPath)?;
                let mut heads = Vec::new();
                for variant in entry.variants.iter() {
                    if request
                        .source_ids
                        .as_ref()
                        .is_some_and(|ids| !ids.contains(&variant.source_id))
                    {
                        continue;
                    }
                    let source = state
                        .index
                        .sources
                        .get(&variant.source_id)
                        .ok_or(DocFailureCode::SourceGone)?;
                    source.root.check_identity()?;
                    let reader = source
                        .history
                        .clone()
                        .ok_or(DocFailureCode::HistoryUnavailable)?;
                    let commit = source
                        .record
                        .head_commit
                        .clone()
                        .ok_or(DocFailureCode::HistoryUnavailable)?;
                    heads.push(Head {
                        reference: DocRef {
                            doc_id: request.doc_id.clone(),
                            source_id: variant.source_id.clone(),
                            source_generation: variant.source_generation,
                        },
                        commit,
                        reader,
                        path: path.clone(),
                    });
                }
                if heads.is_empty()
                    || request.source_ids.as_ref().is_some_and(|ids| {
                        ids.iter()
                            .any(|id| !heads.iter().any(|h| &h.reference.source_id == id))
                    })
                {
                    return Err(DocFailureCode::ForbiddenPath);
                }
                heads.sort_by_key(|h| {
                    state
                        .index
                        .sources
                        .get(&h.reference.source_id)
                        .map(|source| {
                            (
                                source.record.kind != DocSourceKind::Workspace,
                                source.key.clone(),
                            )
                        })
                });
                (
                    state.index.epoch.clone(),
                    HistoryCursor {
                        doc_id: request.doc_id.clone(),
                        positions: vec![0; heads.len()],
                        pending: vec![VecDeque::new(); heads.len()],
                        done: vec![false; heads.len()],
                        heads,
                        seen: BTreeSet::new(),
                        selection: request
                            .source_ids
                            .as_ref()
                            .map(|ids| ids.iter().cloned().collect()),
                    },
                )
            }
        };
        let deadline = Instant::now() + history::DEADLINE;
        let mut rows = Vec::new();
        let mut recorded = Vec::new();
        while rows.len() < limit {
            if context.cancellation.is_cancelled() {
                return Err(DocFailureCode::SnapshotExpired);
            }
            for (i, head) in cursor.heads.iter().enumerate() {
                while cursor.pending[i].is_empty() && !cursor.done[i] {
                    let fetch = limit.min((200 / cursor.heads.len()).max(1));
                    let page = head.reader.records(
                        &head.commit,
                        &head.path,
                        cursor.positions[i],
                        fetch,
                        deadline,
                    )?;
                    cursor.positions[i] += page.examined;
                    cursor.done[i] = page.examined < fetch;
                    cursor.pending[i].extend(page.rows);
                }
            }
            let next = cursor
                .pending
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.front().map(|r| (r.recorded_at, std::cmp::Reverse(i))))
                .max();
            let Some((_, std::cmp::Reverse(i))) = next else {
                break;
            };
            let head = &cursor.heads[i];
            if let Some(record) = cursor.pending[i].pop_front() {
                if !cursor.seen.insert(record.commit.clone()) {
                    continue;
                }
                if cursor.seen.len() > 4000 {
                    return Err(DocFailureCode::TooLarge);
                }
                let version = DocVersion::try_from(DocVersionData {
                    id: index::id(),
                    document: head.reference.clone(),
                    kind: DocVersionKind::Recorded,
                    markdown_hash: record.hash,
                    bytes: record.bytes,
                    observed_at_ms: None,
                    recorded_at_ms: Some(record.recorded_at),
                    commit_id: Some(record.commit),
                    change_id: None,
                    author: Some(record.author),
                    subject: Some(record.subject),
                })
                .map_err(|_| DocFailureCode::TooLarge)?;
                recorded.push(Recorded {
                    version: version.clone(),
                    reader: head.reader.clone(),
                    path: head.path.clone(),
                });
                rows.push(DocHistoryRow {
                    version,
                    sources: List::default(),
                });
            }
        }
        let commits: Vec<_> = rows
            .iter()
            .filter_map(|r| r.version.commit_id.clone())
            .collect();
        let mut reached = Vec::new();
        for head in &cursor.heads {
            reached.push(head.reader.reachable(&head.commit, &commits, deadline)?);
        }
        for row in &mut rows {
            row.sources = List::new(
                cursor
                    .heads
                    .iter()
                    .zip(&reached)
                    .filter(|(_, ids)| {
                        row.version
                            .commit_id
                            .as_ref()
                            .is_some_and(|id| ids.contains(id))
                    })
                    .map(|(h, _)| h.reference.clone())
                    .collect(),
            )
            .expect("source bound");
            if row.sources.is_empty() {
                return Err(DocFailureCode::HistoryUnavailable);
            }
        }
        let mut state = self.state();
        state.check(context)?;
        if state.index.epoch != epoch {
            return Err(DocFailureCode::SnapshotExpired);
        }
        for head in &cursor.heads {
            state.index.resolve(&head.reference)?;
        }
        let next_cursor = if cursor.pending.iter().any(|rows| !rows.is_empty())
            || cursor.done.iter().any(|done| !done)
        {
            Some(state.put_cursor(
                context,
                CursorData::History(cursor.clone()),
                history_charge(&cursor),
            )?)
        } else {
            None
        };
        let connection = state
            .connections
            .get_mut(&context.connection_epoch)
            .ok_or(DocFailureCode::SnapshotExpired)?;
        for value in recorded {
            while connection.recorded.len() >= MAX_RECORDED {
                let first = connection
                    .recorded
                    .keys()
                    .next()
                    .cloned()
                    .expect("nonempty");
                connection.recorded.remove(&first);
            }
            connection.recorded.insert(value.version.id.clone(), value);
        }
        Ok(DocHistoryPage {
            epoch,
            doc_id: cursor.doc_id,
            captured_heads: List::new(
                cursor
                    .heads
                    .iter()
                    .map(|h| DocCapturedHead {
                        source_id: h.reference.source_id.clone(),
                        source_generation: h.reference.source_generation,
                        commit_id: h.commit.clone(),
                    })
                    .collect(),
            )
            .expect("registered sources"),
            recorded_versions: List::new(rows).expect("page bound"),
            next_cursor,
        })
    }
}

impl State {
    fn advance(&mut self) {
        let before = self.index.phase;
        let changed = self.index.advance();
        if before != self.index.phase || (changed && self.index.changed.is_empty()) {
            self.status_dirty = true;
            self.index.revision.0 += 1;
        }
    }
    fn admit(&mut self, context: &ConnCtx) -> Result<(), RpcError> {
        if !self.active
            || context.caller != Caller::Operator
            || context.cancellation.is_cancelled()
            || context.run != self.run
        {
            return Err(RpcError::forbidden(
                "Docs connection scope is no longer active",
            ));
        }
        if !self.connections.contains_key(&context.connection_epoch) {
            if self.connections.len() >= MAX_CONNECTIONS {
                return Err(failure_error(DocFailureCode::Busy));
            }
            self.connections.insert(
                context.connection_epoch.clone(),
                Connection {
                    context: context.clone(),
                    versions: BTreeMap::new(),
                    recorded: BTreeMap::new(),
                    cursors: BTreeMap::new(),
                    watch: None,
                },
            );
        }
        self.check(context).map_err(failure_error)
    }
    fn check(&self, context: &ConnCtx) -> Result<(), DocFailureCode> {
        if !self.active
            || context.caller != Caller::Operator
            || context.cancellation.is_cancelled()
            || context.run != self.run
            || !self
                .connections
                .get(&context.connection_epoch)
                .is_some_and(|c| {
                    c.context.run == context.run && !c.context.cancellation.is_cancelled()
                })
        {
            return Err(DocFailureCode::SnapshotExpired);
        }
        Ok(())
    }
    fn disconnect(&mut self, id: &Id) {
        if let Some(connection) = self.connections.remove(id) {
            connection.context.cancellation.cancel();
            self.cache_bytes = self
                .cache_bytes
                .saturating_sub(connection.versions.values().map(|c| c.value.charge).sum());
            self.cursor_bytes = self
                .cursor_bytes
                .saturating_sub(connection.cursors.values().map(|c| c.charge).sum());
            if let Some(w) = connection.watch {
                w.queue.close();
            }
        }
        if !self.connections.values().any(|c| c.watch.is_some()) {
            self.native = None;
            self.watch_mode = DocWatchMode::None;
            self.image_hashes.clear();
        }
    }
    fn invalidate_leases(&mut self) {
        for connection in self.connections.values_mut() {
            connection.versions.clear();
            connection.recorded.clear();
            connection.cursors.clear();
        }
        self.cache_bytes = 0;
        self.cursor_bytes = 0;
        self.publish(true);
    }
    fn finish_watches(&mut self, cause: DocChangeKind) {
        for connection in self.connections.values_mut() {
            if let Some(w) = connection.watch.take() {
                w.queue.finish(terminal_change(&self.index, &w, cause));
            }
        }
        self.native = None;
        self.watch_mode = DocWatchMode::None;
    }

    fn plan(&self, context: &ConnCtx, request: DocsRead) -> Result<Plan, DocFailureCode> {
        let (path, source) = self.index.resolve(&request.document)?;
        let (version, content) = match request.version {
            DocVersionSelector::Current => (
                DocVersion::try_from(DocVersionData {
                    id: index::id(),
                    document: request.document,
                    kind: DocVersionKind::Working,
                    markdown_hash: markdown::hash(&[]),
                    bytes: 0,
                    observed_at_ms: Some(index::now()),
                    recorded_at_ms: None,
                    commit_id: None,
                    change_id: None,
                    author: None,
                    subject: None,
                })
                .expect("empty capture"),
                Content::Working(source.root.clone()),
            ),
            DocVersionSelector::Id(id) => {
                let recorded = self
                    .connections
                    .get(&context.connection_epoch)
                    .and_then(|c| c.recorded.get(&id))
                    .ok_or(DocFailureCode::SnapshotExpired)?;
                if &recorded.path != path {
                    return Err(DocFailureCode::SnapshotExpired);
                }
                if recorded.version.document != request.document {
                    return Err(DocFailureCode::ForbiddenPath);
                }
                (
                    recorded.version.clone(),
                    Content::Recorded {
                        reader: recorded.reader.clone(),
                        commit: recorded
                            .version
                            .commit_id
                            .clone()
                            .ok_or(DocFailureCode::HistoryUnavailable)?,
                        deadline: Instant::now() + history::DEADLINE,
                    },
                )
            }
        };
        Ok(Plan {
            epoch: self.index.epoch.clone(),
            path: path.clone(),
            policy: self.index.policy.clone(),
            content,
            version,
            links: render::links(&self.index),
        })
    }
    fn retain(&mut self, context: &ConnCtx, rendered: Rendered) -> Result<(), DocFailureCode> {
        if rendered.charge > CACHE_BYTES {
            return Err(DocFailureCode::Busy);
        }
        let conn = &context.connection_epoch;
        let doc = &rendered.snapshot.version.document.doc_id;
        loop {
            let connection = self
                .connections
                .get(conn)
                .ok_or(DocFailureCode::SnapshotExpired)?;
            let group: Vec<_> = connection
                .versions
                .iter()
                .filter(|(_, c)| &c.value.snapshot.version.document.doc_id == doc)
                .collect();
            let bytes: usize = group
                .iter()
                .map(|(_, c)| c.value.snapshot.markdown.len())
                .sum();
            let victim = if group.len() >= 16
                || bytes + rendered.snapshot.markdown.len() > 4 * 1024 * 1024
            {
                group
                    .iter()
                    .min_by_key(|(_, c)| c.age)
                    .map(|(id, _)| (*id).clone())
            } else if connection.versions.len() >= MAX_VERSIONS {
                connection
                    .versions
                    .iter()
                    .min_by_key(|(_, c)| c.age)
                    .map(|(id, _)| id.clone())
            } else {
                None
            };
            if let Some(victim) = victim {
                self.evict(conn, &victim);
            } else {
                break;
            }
        }
        while self.cache_bytes + rendered.charge > CACHE_BYTES {
            let oldest = self
                .connections
                .iter()
                .flat_map(|(conn, c)| {
                    c.versions
                        .iter()
                        .map(move |(id, v)| (v.age, conn.clone(), id.clone()))
                })
                .min();
            let Some((_, conn, id)) = oldest else {
                return Err(DocFailureCode::Busy);
            };
            self.evict(&conn, &id);
        }
        self.clock += 1;
        self.cache_bytes += rendered.charge;
        let connection = self
            .connections
            .get_mut(conn)
            .ok_or(DocFailureCode::SnapshotExpired)?;
        if let Some(old) = connection.versions.insert(
            rendered.snapshot.version.id.clone(),
            Cached {
                value: Arc::new(rendered),
                age: self.clock,
            },
        ) {
            self.cache_bytes = self.cache_bytes.saturating_sub(old.value.charge);
        }
        Ok(())
    }
    fn evict(&mut self, conn: &Id, id: &Id) {
        if let Some(connection) = self.connections.get_mut(conn) {
            if let Some(old) = connection.versions.remove(id) {
                self.cache_bytes = self.cache_bytes.saturating_sub(old.value.charge);
                if let Some(w) = &mut connection.watch {
                    let mut changed = empty_change(&self.index, w);
                    changed.expired_seen_versions =
                        List::new(vec![id.clone()]).expect("one expiry");
                    self.index.revision.0 += 1;
                    changed.to_revision = self.index.revision;
                    w.revision = self.index.revision;
                    w.queue.push(changed);
                }
            }
        }
    }
    fn asset(
        &self,
        context: &ConnCtx,
        request: DocsAsset,
    ) -> Result<DocAssetChunk, DocFailureCode> {
        self.check(context)?;
        request.validate().map_err(|_| DocFailureCode::TooLarge)?;
        let rendered = self
            .connections
            .get(&context.connection_epoch)
            .and_then(|c| {
                c.versions
                    .values()
                    .find(|v| v.value.snapshot.snapshot_id == request.snapshot_id)
            })
            .ok_or(DocFailureCode::SnapshotExpired)?;
        self.index
            .resolve(&rendered.value.snapshot.version.document)?;
        let asset = rendered
            .value
            .assets
            .get(&request.asset_id)
            .ok_or(DocFailureCode::ForbiddenPath)?;
        let offset = request.offset as usize;
        if offset > asset.bytes.len() {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let end = (offset + request.max_bytes as usize).min(asset.bytes.len());
        DocAssetChunk::try_from(DocAssetChunkData {
            snapshot_id: request.snapshot_id,
            asset_id: request.asset_id,
            hash: asset.hash.clone(),
            media_type: asset.raster.kind,
            offset: request.offset,
            total_bytes: asset.bytes.len() as u32,
            data_base64: Text::new(
                base64::engine::general_purpose::STANDARD.encode(&asset.bytes[offset..end]),
            )
            .map_err(|_| DocFailureCode::TooLarge)?,
            next_offset: (end < asset.bytes.len()).then_some(end as u32),
        })
        .map_err(|_| DocFailureCode::TooLarge)
    }
    fn index_page(
        &mut self,
        context: &ConnCtx,
        request: &PageRequest,
    ) -> Result<DocIndexPage, DocFailureCode> {
        self.check(context)?;
        request
            .validate(200)
            .map_err(|_| DocFailureCode::TooLarge)?;
        let limit = request.limit.unwrap_or(200) as usize;
        let (mut page, entries, mut offset, charge) = if let Some(token) = &request.cursor {
            let cursor = self.take_cursor(context, token)?;
            let CursorData::Index {
                page,
                entries,
                offset,
            } = cursor.data
            else {
                return Err(DocFailureCode::StaleCursor);
            };
            (page, entries, offset, cursor.charge)
        } else {
            let entries: Vec<_> = self.index.rows.values().cloned().collect();
            let sources = List::new(
                self.index
                    .sources
                    .values()
                    .map(|s| s.record.clone())
                    .collect(),
            )
            .expect("registered sources");
            let charge = bounded_size(&entries, CURSOR_BYTES / 3)?.saturating_mul(3)
                + bounded_size(&sources, 128 * 1024)?.saturating_mul(3)
                + 8192;
            (
                DocIndexPage {
                    epoch: self.index.epoch.clone(),
                    revision: self.index.revision,
                    sources,
                    entries: List::default(),
                    next_cursor: None,
                    phase: self.index.phase,
                    watch_mode: self.watch_mode,
                    limited: self.index.limited,
                    omitted_entries: None,
                    reason: self
                        .index
                        .config_error
                        .then_some(DocFailureCode::ForbiddenPath),
                },
                Arc::new(entries),
                0,
                charge,
            )
        };
        let mut rows = Vec::new();
        while offset < entries.len() && rows.len() < limit {
            rows.push(entries[offset].clone());
            page.entries = List::new(rows.clone()).expect("page bound");
            if bounded_size(&page, 500 * 1024).is_err() {
                rows.pop();
                break;
            }
            offset += 1;
        }
        if rows.is_empty() && offset < entries.len() {
            return Err(DocFailureCode::TooLarge);
        }
        page.entries = List::new(rows).expect("page bound");
        page.next_cursor = None;
        if offset < entries.len() {
            let mut base = page.clone();
            base.entries = List::default();
            page.next_cursor = Some(self.put_cursor(
                context,
                CursorData::Index {
                    page: base,
                    entries,
                    offset,
                },
                charge,
            )?);
        }
        Ok(page)
    }
    fn put_cursor(
        &mut self,
        context: &ConnCtx,
        data: CursorData,
        charge: usize,
    ) -> Result<Id, DocFailureCode> {
        if charge > CURSOR_BYTES {
            return Err(DocFailureCode::Busy);
        }
        let expired: Vec<_> = self
            .connections
            .iter()
            .flat_map(|(conn, c)| {
                c.cursors
                    .iter()
                    .filter(|(_, v)| v.created.elapsed() > Duration::from_secs(300))
                    .map(move |(id, _)| (conn.clone(), id.clone()))
            })
            .collect();
        for (conn, id) in expired {
            if let Some(cursor) = self
                .connections
                .get_mut(&conn)
                .and_then(|c| c.cursors.remove(&id))
            {
                self.cursor_bytes = self.cursor_bytes.saturating_sub(cursor.charge);
            }
        }
        let connection = self
            .connections
            .get_mut(&context.connection_epoch)
            .ok_or(DocFailureCode::SnapshotExpired)?;
        while connection.cursors.len() >= MAX_CURSORS {
            let id = connection
                .cursors
                .iter()
                .min_by_key(|(_, v)| v.created)
                .map(|(id, _)| id.clone())
                .expect("cursor present");
            let old = connection.cursors.remove(&id).expect("cursor present");
            self.cursor_bytes = self.cursor_bytes.saturating_sub(old.charge);
        }
        if self.cursor_bytes + charge > CURSOR_BYTES {
            return Err(DocFailureCode::Busy);
        }
        let id = index::id();
        self.cursor_bytes += charge;
        connection.cursors.insert(
            id.clone(),
            Cursor {
                epoch: self.index.epoch.clone(),
                created: Instant::now(),
                charge,
                data,
            },
        );
        Ok(id)
    }
    fn take_cursor(&mut self, context: &ConnCtx, id: &Id) -> Result<Cursor, DocFailureCode> {
        let cursor = self
            .connections
            .get_mut(&context.connection_epoch)
            .and_then(|c| c.cursors.remove(id))
            .ok_or(DocFailureCode::StaleCursor)?;
        self.cursor_bytes = self.cursor_bytes.saturating_sub(cursor.charge);
        if cursor.epoch != self.index.epoch || cursor.created.elapsed() > Duration::from_secs(300) {
            return Err(DocFailureCode::StaleCursor);
        }
        Ok(cursor)
    }

    fn watch(
        &mut self,
        context: &ConnCtx,
        request: DocsWatch,
    ) -> Result<DocsWatchResult, RpcError> {
        if !self.index.policy.enabled {
            return Err(failure_error(DocFailureCode::Disabled));
        }
        self.check(context).map_err(failure_error)?;
        for selected in request.selected.iter() {
            self.index.resolve(selected).map_err(failure_error)?;
        }
        if self.native.is_none() && self.watch_mode == DocWatchMode::None {
            self.native = watch::Native::new(self.dirty.clone()).ok();
            self.watch_mode = if self.native.is_some() {
                DocWatchMode::Events
            } else {
                DocWatchMode::ReconcileOnly
            };
        }
        let watch_id = index::id();
        let queue = Arc::new(watch::Queue::default());
        let connection = self
            .connections
            .get_mut(&context.connection_epoch)
            .expect("admitted");
        let old = connection.watch.replace(Watch {
            id: watch_id.clone(),
            epoch: self.index.epoch.clone(),
            revision: self.index.revision,
            selected: request.selected.to_vec(),
            queue,
        });
        // Register first. A guaranteed rescan also covers new directories found
        // during capture before their nonrecursive native watch was attached.
        self.sync_native();
        self.advance();
        self.sync_native();
        self.dirty.store(true, Ordering::Release);
        let initial_index_page = match self.index_page(context, &PageRequest::default()) {
            Ok(page) => page,
            Err(reason) => {
                let connection = self
                    .connections
                    .get_mut(&context.connection_epoch)
                    .expect("admitted");
                if let Some(tentative) = connection.watch.take() {
                    tentative.queue.close();
                }
                connection.watch = old;
                if !self.connections.values().any(|c| c.watch.is_some()) {
                    self.native = None;
                    self.watch_mode = DocWatchMode::None;
                }
                return Err(failure_error(reason));
            }
        };
        // P2 aborts and awaits the old forwarder after preparing the new one.
        // Closing here could race its EOF handler into sending a spurious
        // terminal frame before the successful replacement reply is queued.
        drop(old);
        if let Some(w) = self
            .connections
            .get_mut(&context.connection_epoch)
            .and_then(|c| c.watch.as_mut())
        {
            w.revision = initial_index_page.revision;
        }
        Ok(DocsWatchResult {
            watch_id,
            initial_index_page,
        })
    }
    fn unwatch(&mut self, context: &ConnCtx, request: Unwatch) -> UnwatchResult {
        let connection = self
            .connections
            .get_mut(&context.connection_epoch)
            .expect("admitted");
        let removed = connection
            .watch
            .as_ref()
            .is_some_and(|w| w.id == request.watch_id);
        if removed {
            connection
                .watch
                .take()
                .expect("watch present")
                .queue
                .close();
        }
        if !self.connections.values().any(|c| c.watch.is_some()) {
            self.native = None;
            self.watch_mode = DocWatchMode::None;
            self.image_hashes.clear();
        }
        UnwatchResult {
            watch_id: request.watch_id,
            removed,
        }
    }
    fn sync_native(&mut self) {
        if self.native.is_none() {
            return;
        }
        if self.index.sources.values().any(|s| s.watch_limited) {
            self.native = None;
            self.watch_mode = DocWatchMode::ReconcileOnly;
            self.status_dirty = true;
            return;
        }
        if self.native.is_none() {
            return;
        }
        let mut paths = BTreeSet::new();
        for source in self.index.sources.values() {
            if source.root.check_identity().is_err() {
                continue;
            }
            paths.insert(source.root.path().to_owned());
            for directory in &source.directories {
                paths.insert(source.root.path().join(directory.as_path()));
            }
        }
        if self.native.as_mut().is_some_and(|w| w.sync(paths).is_err()) {
            self.native = None;
            self.watch_mode = DocWatchMode::ReconcileOnly;
            self.dirty.store(true, Ordering::Release);
        }
    }
    fn check_images(&mut self) {
        let mut dependencies: ImageDependencies = BTreeMap::new();
        for connection in self.connections.values() {
            let Some(w) = &connection.watch else {
                continue;
            };
            for selected in &w.selected {
                let Some(latest) = connection
                    .versions
                    .values()
                    .filter(|c| {
                        c.value.snapshot.version.document == *selected
                            && c.value.snapshot.version.kind != DocVersionKind::Recorded
                    })
                    .max_by_key(|c| c.age)
                else {
                    continue;
                };
                for image in latest.value.snapshot.images.iter() {
                    let Some(path) = &image.relative_path else {
                        continue;
                    };
                    let Ok(path) = Relative::new(path) else {
                        continue;
                    };
                    let entry = dependencies
                        .entry((selected.source_id.clone(), path))
                        .or_insert_with(|| (image.hash.clone(), BTreeSet::new()));
                    entry
                        .1
                        .insert((selected.source_id.clone(), selected.doc_id.clone()));
                }
            }
        }
        self.image_hashes
            .retain(|key, _| dependencies.contains_key(key));
        let keys: Vec<_> = dependencies.keys().cloned().collect();
        if keys.is_empty() {
            return;
        }
        let mut bytes = 0;
        for _ in 0..keys.len().min(16) {
            if bytes >= 4 * 1024 * 1024 {
                break;
            }
            let key = &keys[self.image_cursor % keys.len()];
            self.image_cursor = (self.image_cursor + 1) % keys.len();
            let (initial, docs) = &dependencies[key];
            let previous = self
                .image_hashes
                .entry(key.clone())
                .or_insert_with(|| initial.clone())
                .clone();
            let next = self
                .index
                .sources
                .get(&key.0)
                .and_then(|s| {
                    s.root
                        .read(&key.1, &self.index.policy, MAX_IMAGE_BYTES as usize)
                        .ok()
                })
                .map(|r| {
                    bytes += r.bytes.len();
                    markdown::hash(&r.bytes)
                });
            if next != previous {
                self.image_changed.extend(docs.iter().cloned());
                self.image_hashes.insert(key.clone(), next);
            }
        }
    }
    fn publish(&mut self, resync: bool) {
        let resync = resync || std::mem::take(&mut self.status_dirty);
        if !resync && self.index.changed.is_empty() && self.image_changed.is_empty() {
            return;
        }
        if !self.image_changed.is_empty() {
            self.index.revision.0 += 1;
        }
        let changed = std::mem::take(&mut self.index.changed);
        let images = std::mem::take(&mut self.image_changed);
        let mut upserts: Vec<_> = self
            .index
            .rows
            .values()
            .filter(|e| changed.contains(&e.id))
            .take(201)
            .cloned()
            .collect();
        let resync = resync || upserts.len() > 200 || bounded_size(&upserts, 12 * 1024).is_err();
        if resync {
            upserts.clear();
        }
        for connection in self.connections.values_mut() {
            if connection.context.cancellation.is_cancelled() {
                continue;
            }
            if connection
                .watch
                .as_ref()
                .is_some_and(|watch| watch.epoch != self.index.epoch)
            {
                let watch = connection.watch.take().expect("present watch");
                watch.queue.finish(terminal_change(
                    &self.index,
                    &watch,
                    DocChangeKind::Availability,
                ));
                continue;
            }
            let Some(w) = &mut connection.watch else {
                continue;
            };
            let mut change = empty_change(&self.index, w);
            if w.revision >= self.index.revision {
                continue;
            }
            if resync {
                change.history_sources_changed =
                    List::new(self.index.sources.keys().cloned().collect())
                        .expect("sources bounded");
            }
            change.upserts = List::new(upserts.clone()).expect("bounded delta");
            change.resync_required = resync;
            change.changed_selected = List::new(
                w.selected
                    .iter()
                    .filter_map(|reference| {
                        if images.contains(&(reference.source_id.clone(), reference.doc_id.clone()))
                        {
                            Some(DocSelectedChange {
                                document: reference.clone(),
                                kind: DocChangeKind::Image,
                            })
                        } else if changed.contains(&reference.doc_id) || resync {
                            Some(DocSelectedChange {
                                document: reference.clone(),
                                kind: DocChangeKind::Content,
                            })
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
            .expect("two selections");
            w.revision = self.index.revision;
            w.queue.push(change);
        }
    }
}

fn terminal_change(index: &Index, watch: &Watch, cause: DocChangeKind) -> DocsChanged {
    let mut change = empty_change(index, watch);
    change.epoch = index.epoch.clone();
    change.resync_required = true;
    change.changed_selected = List::new(
        watch
            .selected
            .iter()
            .map(|document| DocSelectedChange {
                document: document.clone(),
                kind: if index.resolve(document).is_err() {
                    if cause == DocChangeKind::Policy {
                        DocChangeKind::Policy
                    } else {
                        DocChangeKind::Removed
                    }
                } else {
                    DocChangeKind::Availability
                },
            })
            .collect(),
    )
    .expect("two selections");
    change.history_sources_changed =
        List::new(index.sources.keys().cloned().collect()).expect("registered sources");
    change
}

fn empty_change(index: &Index, watch: &Watch) -> DocsChanged {
    DocsChanged {
        epoch: watch.epoch.clone(),
        watch_id: watch.id.clone(),
        from_revision: watch.revision,
        to_revision: index.revision,
        upserts: List::default(),
        removed_doc_ids: List::default(),
        changed_selected: List::default(),
        history_sources_changed: List::default(),
        expired_seen_versions: List::default(),
        resync_required: false,
    }
}
fn history_charge(cursor: &HistoryCursor) -> usize {
    cursor.heads.len() * 8192
        + cursor.seen.len() * 512
        + cursor
            .pending
            .iter()
            .flatten()
            .map(|r| 2048 + 3 * (r.author.len() + r.subject.len()))
            .sum::<usize>()
        + 4096
}
fn failure(document: Option<DocRef>, code: DocFailureCode) -> DocFailure {
    DocFailure {
        document,
        code,
        detail: markdown::label(match code {
            DocFailureCode::Disabled => "Docs is disabled",
            DocFailureCode::ForbiddenPath => "Document is not available in this scope",
            DocFailureCode::SourceGone => "Document source is no longer available",
            DocFailureCode::TooLarge => "Document exceeds the display limit",
            DocFailureCode::InvalidUtf8 => "Document is not UTF-8",
            DocFailureCode::UnsupportedImage => "Image format is not supported",
            DocFailureCode::UnstableFile => "File changed while it was being read",
            DocFailureCode::HistoryUnavailable => "Published recorded history is unavailable",
            DocFailureCode::ObjectMissing => "Document is no longer present",
            DocFailureCode::SnapshotExpired => "This version is no longer kept here",
            DocFailureCode::StaleCursor => "Document listing changed; capture a new page",
            DocFailureCode::Busy => "Docs is busy; try again",
        }),
        bytes: None,
    }
}
fn failure_error(code: DocFailureCode) -> RpcError {
    let failure = failure(None, code);
    RpcError::new(
        match code {
            DocFailureCode::Busy => "busy",
            DocFailureCode::ForbiddenPath => "forbidden_path",
            DocFailureCode::SourceGone => "source_gone",
            DocFailureCode::TooLarge => "too_large",
            DocFailureCode::StaleCursor => "stale_cursor",
            DocFailureCode::Disabled => "disabled",
            DocFailureCode::SnapshotExpired => "snapshot_expired",
            _ => "docs_unavailable",
        },
        failure.detail.to_string(),
    )
}
fn encode(value: &impl Serialize) -> Result<Value, RpcError> {
    bounded_size(value, 510 * 1024).map_err(failure_error)?;
    serde_json::to_value(value).map_err(|_| RpcError::new("internal", "Docs encoding failed"))
}

#[async_trait::async_trait]
impl FeatureService for Service {
    fn feature(&self) -> Feature {
        Feature::Docs
    }
    fn readiness(&self) -> Readiness {
        if !self.state().active {
            return Readiness::Unavailable(FeatureUnavailable::NotReady);
        }
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            Readiness::Ready
        } else {
            Readiness::Unavailable(FeatureUnavailable::Unsupported)
        }
    }
    async fn handle(&self, context: &ConnCtx, call: Call) -> Result<Value, FeatureError> {
        let watching = matches!(call, Call::DocsWatch(_));
        let result = self.execute(context, call).await;
        if watching && result.is_ok() {
            self.start_worker();
        }
        result
    }
    fn watch_stream(
        &self,
        context: ConnCtx,
        watch_id: Id,
    ) -> Result<Box<dyn FeatureWatch>, FeatureError> {
        let state = self.state();
        state.check(&context).map_err(failure_error)?;
        let w = state
            .connections
            .get(&context.connection_epoch)
            .and_then(|c| c.watch.as_ref())
            .filter(|w| w.id == watch_id)
            .ok_or_else(|| failure_error(DocFailureCode::SnapshotExpired))?;
        Ok(Box::new(watch::Stream {
            queue: w.queue.clone(),
            context,
        }))
    }
    fn connection_closed(&self, connection: &ConnId) {
        self.state().disconnect(connection);
    }
    fn run_archived(&self, run: &RunCtx) {
        let mut state = self.state();
        if &state.run == run {
            state.active = false;
            state.finish_watches(DocChangeKind::Availability);
            state.invalidate_leases();
            for connection in state.connections.values() {
                connection.context.cancellation.cancel();
            }
            state.connections.clear();
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
