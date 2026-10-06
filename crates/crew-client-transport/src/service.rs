//! Rust owns the serial event loop. Synchronous pure calls belong on a platform
//! worker/actor; connecting never blocks them on a socket. Typed callbacks run
//! on a separate worker, outside every driver/queue mutex, and may query again.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self as sync, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crew_client::api::*;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::framing::MAX_FRAME_BYTES;
#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
use crate::local::{LocalChannel, LocalProfile, LocalTransport};
use crate::queue::{EventQueue, QUEUE_BYTES, QUEUE_FRAMES};
use crate::{
    ChannelId, Clock, CommandCodec, DriverConfig, HostEffect, HostEvent, HostUpdate, SessionDriver,
    SystemClock, TransportEnd,
};

pub trait ClientObserver: Send + Sync + 'static {
    /// Enqueue the invalidation in the platform executor. Query only the
    /// affected projections, not a full history on every unrelated push.
    fn changed(&self, notice: ClientNotice);
    fn terminal(&self, chunk: TerminalChunk);
}

pub trait ChannelHandle: Send {
    fn send(&self, request_id: u64, text: String) -> Result<(), TransportEnd>;
    fn close(&self);
    fn fail(&self, reason: TransportEnd);
    fn wait_closed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
impl ChannelHandle for LocalChannel {
    fn send(&self, request_id: u64, text: String) -> Result<(), TransportEnd> {
        self.send(request_id, text)
    }
    fn close(&self) {
        self.close();
    }
    fn fail(&self, reason: TransportEnd) {
        self.fail(reason);
    }
    fn wait_closed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.wait_closed())
    }
}

/// Internal Rust connector seam. It cannot be called by a view to assert a
/// grant. Authentication and profile/credential ownership stay in adapters.
pub trait Connector: Send + 'static {
    fn open(
        &self,
        effect: &HostEffect,
        events: &EventQueue,
    ) -> Result<Box<dyn ChannelHandle>, TransportEnd>;
}

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
pub struct LocalConnector {
    profiles: BTreeMap<String, LocalProfile>,
}

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
impl LocalConnector {
    pub fn new(profiles: Vec<LocalProfile>) -> Result<Self, ClientError> {
        if profiles.len() > 128 {
            return Err(internal("profile_capacity"));
        }
        let mut selected = BTreeMap::new();
        for profile in profiles {
            profile.validate().map_err(|_| ClientError::InvalidInput {
                field: "local_profile".into(),
                reason: "invalid local profile".into(),
            })?;
            if selected.insert(profile.id.clone(), profile).is_some() {
                return Err(ClientError::InvalidInput {
                    field: "profile_id".into(),
                    reason: "duplicate local profile".into(),
                });
            }
        }
        Ok(Self { profiles: selected })
    }
}

#[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
impl Connector for LocalConnector {
    fn open(
        &self,
        effect: &HostEffect,
        events: &EventQueue,
    ) -> Result<Box<dyn ChannelHandle>, TransportEnd> {
        let HostEffect::Open { profile_id, .. } = effect else {
            return Err(TransportEnd::Permission);
        };
        let profile = self
            .profiles
            .get(profile_id)
            .ok_or(TransportEnd::Permission)?
            .clone();
        Ok(Box::new(LocalTransport::open(profile, effect, events)?))
    }
}

type Reply<T> = SyncSender<Result<T, ClientError>>;

enum Call {
    Query(ViewQuery, Reply<ViewData>),
    Context(Reply<Option<ConnectionContext>>),
    Workspaces(Reply<Option<crew_protocol::rpc::Workspaces>>),
    Plan(ActionIntent, Reply<ActionPlan>),
    Confirm(u64, Reply<()>),
    Cancel(u64, Reply<()>),
    Connect(String, String, Option<String>, Reply<()>),
    Select(String, Reply<()>),
    Reconnect(Reply<()>),
    Disconnect(Reply<()>),
    Lifecycle(bool, Reply<()>),
    Observe(Option<Arc<dyn ClientObserver>>, Reply<()>),
    Close(Reply<()>),
}

struct Mail {
    call: Call,
    _budget: Budget,
}

struct Budget {
    bytes: usize,
    counter: Arc<AtomicUsize>,
}

impl Drop for Budget {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct Inner {
    sender: mpsc::Sender<Mail>,
    budget: Arc<AtomicUsize>,
    closed: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// One reference-counted native service, independent of GTK/SwiftUI. Public
/// methods expose only typed policy, views and lifecycle; never frame JSON.
#[derive(Clone)]
pub struct NativeSession(Arc<Inner>);

impl NativeSession {
    #[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
    pub fn local<C: ClientApi + Send + 'static, W: CommandCodec + 'static>(
        core: C,
        codec: W,
        profiles: Vec<LocalProfile>,
        config: DriverConfig,
    ) -> Result<Self, ClientError> {
        Self::with_connector(core, codec, LocalConnector::new(profiles)?, config)
    }

    pub fn with_connector<
        C: ClientApi + Send + 'static,
        W: CommandCodec + 'static,
        A: Connector,
    >(
        core: C,
        codec: W,
        connector: A,
        config: DriverConfig,
    ) -> Result<Self, ClientError> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let driver = SessionDriver::with_clock(core, codec, config, clock.clone())?;
        let events =
            EventQueue::new(config.max_channels).map_err(|_| internal("event_capacity"))?;
        let (sender, receiver) = mpsc::channel(64);
        let callbacks = Arc::new(Callbacks::new());
        let (started, startup) = sync::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("crew-native-io".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = started.send(Err(internal("runtime_start")));
                        return;
                    }
                };
                let dispatcher = callbacks.clone();
                let callback_worker = std::thread::Builder::new()
                    .name("crew-native-observer".into())
                    .spawn(move || dispatcher.dispatch());
                if callback_worker.is_err() {
                    let _ = started.send(Err(internal("observer_start")));
                    return;
                }
                let _ = started.send(Ok(()));
                runtime.block_on(actor(
                    driver,
                    connector,
                    events,
                    receiver,
                    callbacks.clone(),
                    clock,
                ));
                callbacks.finish();
                // IO tasks are canceled before this point. A slow protected-file
                // lookup can retain only its private bounded temporary buffer.
                runtime.shutdown_timeout(Duration::from_secs(1));
                // Do not await a foreign callback that is itself disposing/querying
                // the service. Its Arc retains ownership until it returns.
                drop(callback_worker);
            })
            .map_err(|_| internal("worker_start"))?;
        startup.recv().map_err(|_| ClientError::ClosedClient)??;
        Ok(Self(Arc::new(Inner {
            sender,
            budget: Arc::new(AtomicUsize::new(0)),
            closed: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
        })))
    }

    fn call<T>(&self, bytes: usize, make: impl FnOnce(Reply<T>) -> Call) -> Result<T, ClientError> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(ClientError::ClosedClient);
        }
        if bytes > MAX_FRAME_BYTES {
            return Err(ClientError::TooLarge {
                actual: bytes as u64,
                limit: MAX_FRAME_BYTES as u64,
            });
        }
        self.0
            .budget
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= QUEUE_BYTES)
            })
            .map_err(|_| internal("command_capacity"))?;
        let budget = Budget {
            bytes,
            counter: self.0.budget.clone(),
        };
        let (reply, received) = sync::sync_channel(1);
        self.0
            .sender
            .try_send(Mail {
                call: make(reply),
                _budget: budget,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => internal("command_capacity"),
                mpsc::error::TrySendError::Closed(_) => ClientError::ClosedClient,
            })?;
        received.recv().map_err(|_| ClientError::ClosedClient)?
    }

    pub fn query(&self, query: ViewQuery) -> Result<ViewData, ClientError> {
        let bytes = bounded_size(&query)?;
        self.call(bytes, |reply| Call::Query(query, reply))
    }
    pub fn connection_context(&self) -> Result<Option<ConnectionContext>, ClientError> {
        self.call(0, Call::Context)
    }
    pub fn workspaces(&self) -> Result<Option<crew_protocol::rpc::Workspaces>, ClientError> {
        self.call(0, Call::Workspaces)
    }
    pub fn plan(&self, intent: ActionIntent) -> Result<ActionPlan, ClientError> {
        let bytes = bounded_size(&intent)?;
        self.call(bytes, |reply| Call::Plan(intent, reply))
    }
    pub fn confirm(&self, id: u64) -> Result<(), ClientError> {
        self.call(0, |reply| Call::Confirm(id, reply))
    }
    pub fn cancel_confirmation(&self, id: u64) -> Result<(), ClientError> {
        self.call(0, |reply| Call::Cancel(id, reply))
    }
    pub fn connect(
        &self,
        profile_id: String,
        endpoint_id: String,
        workspace_id: Option<String>,
    ) -> Result<(), ClientError> {
        let bytes = profile_id
            .len()
            .saturating_add(endpoint_id.len())
            .saturating_add(workspace_id.as_ref().map_or(0, String::len));
        self.call(bytes, |reply| {
            Call::Connect(profile_id, endpoint_id, workspace_id, reply)
        })
    }
    pub fn select_workspace(&self, id: String) -> Result<(), ClientError> {
        self.call(id.len(), |reply| Call::Select(id, reply))
    }
    pub fn reconnect(&self) -> Result<(), ClientError> {
        self.call(0, Call::Reconnect)
    }
    pub fn disconnect(&self) -> Result<(), ClientError> {
        self.call(0, Call::Disconnect)
    }
    pub fn lifecycle(&self, active: bool) -> Result<(), ClientError> {
        self.call(0, |reply| Call::Lifecycle(active, reply))
    }
    pub fn observe(&self, observer: Arc<dyn ClientObserver>) -> Result<(), ClientError> {
        self.call(0, |reply| Call::Observe(Some(observer), reply))
    }
    pub fn unregister_observer(&self) -> Result<(), ClientError> {
        self.call(0, |reply| Call::Observe(None, reply))
    }

    pub fn close(&self) -> Result<(), ClientError> {
        if self.0.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = self.call(0, Call::Close);
        self.0.closed.store(true, Ordering::Release);
        if let Ok(mut worker) = self.0.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        result
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        // Closing the final sender ends the actor. No observer holds this
        // object automatically and no shutdown RPC is ever sent on disposal.
    }
}

struct Publication {
    generation: u64,
    notice: ClientNotice,
    terminal: Vec<TerminalChunk>,
    bytes: usize,
}

struct CallbackState {
    generation: u64,
    observer: Option<Arc<dyn ClientObserver>>,
    queue: VecDeque<Publication>,
    bytes: usize,
    critical: Option<Publication>,
    stopped: bool,
    finishing: bool,
}

struct Callbacks {
    state: Mutex<CallbackState>,
    ready: Condvar,
    faulted: AtomicBool,
}

impl Callbacks {
    fn new() -> Self {
        Self {
            state: Mutex::new(CallbackState {
                generation: 0,
                observer: None,
                queue: VecDeque::new(),
                bytes: 0,
                critical: None,
                stopped: false,
                finishing: false,
            }),
            ready: Condvar::new(),
            faulted: AtomicBool::new(false),
        }
    }

    fn observe(&self, observer: Option<Arc<dyn ClientObserver>>) -> Result<(), ClientError> {
        let mut state = self.state.lock().map_err(|_| internal("observer_lock"))?;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| internal("observer_generation"))?;
        state.observer = observer;
        state.queue.clear();
        state.bytes = 0;
        state.critical = None;
        self.faulted.store(false, Ordering::Release);
        Ok(())
    }

    fn publish(&self, update: &HostUpdate) -> Result<(), ClientError> {
        let bytes = bounded_size(&update.notice)?.saturating_add(
            update
                .terminal
                .iter()
                .map(|chunk| chunk.data.len())
                .sum::<usize>(),
        );
        let mut state = self.state.lock().map_err(|_| internal("observer_lock"))?;
        if state.observer.is_none() || state.stopped {
            return Ok(());
        }
        if state.queue.len() >= QUEUE_FRAMES
            || bytes > QUEUE_BYTES.saturating_sub(state.bytes)
            || self.faulted.load(Ordering::Acquire)
        {
            self.faulted.store(true, Ordering::Release);
            return Err(internal("observer_capacity"));
        }
        let generation = state.generation;
        state.queue.push_back(Publication {
            generation,
            notice: update.notice.clone(),
            terminal: update.terminal.clone(),
            bytes,
        });
        state.bytes += bytes;
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    fn final_update(&self, mut update: HostUpdate) {
        update.notice.changes = ChangeSet {
            revision: update.notice.revision,
            reset: true,
            availability: true,
            pending: true,
            ..ChangeSet::default()
        };
        if let Ok(mut state) = self.state.lock() {
            let generation = state.generation;
            state.critical = Some(Publication {
                generation,
                notice: update.notice,
                terminal: update.terminal,
                bytes: 0,
            });
        }
        self.ready.notify_one();
    }

    fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.stopped = true;
            state.observer = None;
            state.queue.clear();
            state.bytes = 0;
            state.critical = None;
        }
        self.ready.notify_all();
    }

    fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.finishing = true;
        }
        self.ready.notify_all();
    }

    fn dispatch(&self) {
        loop {
            let delivery = {
                let Ok(mut state) = self.state.lock() else {
                    return;
                };
                loop {
                    if state.stopped
                        || (state.finishing && state.queue.is_empty() && state.critical.is_none())
                    {
                        state.observer = None;
                        return;
                    }
                    let publication = if let Some(publication) = state.queue.pop_front() {
                        state.bytes -= publication.bytes;
                        Some(publication)
                    } else if let Some(publication) = state.critical.take() {
                        self.faulted.store(false, Ordering::Release);
                        Some(publication)
                    } else {
                        None
                    };
                    if let Some(publication) = publication {
                        if publication.generation == state.generation {
                            if let Some(observer) = state.observer.clone() {
                                break (observer, publication);
                            }
                        }
                        continue;
                    }
                    state = match self.ready.wait(state) {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                }
            };
            let (observer, publication) = delivery;
            // No mutex is held across callbacks, including foreign callbacks.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer.changed(publication.notice);
                for chunk in publication.terminal {
                    observer.terminal(chunk);
                }
            }));
            if outcome.is_err() {
                self.stop();
                return;
            }
        }
    }
}

async fn actor<C: ClientApi, W: CommandCodec, A: Connector>(
    mut driver: SessionDriver<C, W>,
    connector: A,
    events: EventQueue,
    mut calls: mpsc::Receiver<Mail>,
    callbacks: Arc<Callbacks>,
    clock: Arc<dyn Clock>,
) {
    let (timer_tx, mut timer_rx) = mpsc::channel::<u64>(256);
    let mut channels: BTreeMap<ChannelId, Box<dyn ChannelHandle>> = BTreeMap::new();
    let mut retired: BTreeMap<ChannelId, Box<dyn ChannelHandle>> = BTreeMap::new();
    let mut timers = BTreeMap::new();
    let mut running = true;
    while running {
        enum Input {
            Host(HostEvent),
            Timer(u64),
            Call(Mail),
            End,
        }
        let input = tokio::select! {
            event = events.recv() => match event { Some(event) => Input::Host(event), None => Input::End },
            timer = timer_rx.recv() => match timer { Some(id) => Input::Timer(id), None => Input::End },
            call = calls.recv() => match call { Some(call) => Input::Call(call), None => Input::End },
        };
        let mut updates = Vec::new();
        match input {
            Input::Host(event) => {
                if let HostEvent::Ended { channel_id, .. } = &event {
                    retired.remove(channel_id);
                }
                match driver.handle(event) {
                    Ok(update) => updates.push(update),
                    Err(_) => {
                        updates.push(driver.state_failed());
                        running = false;
                    }
                }
            }
            Input::Timer(id) => {
                timers.remove(&id);
                match driver.handle(HostEvent::Wake { timer_id: id }) {
                    Ok(update) => updates.push(update),
                    Err(_) => {
                        updates.push(driver.state_failed());
                        running = false;
                    }
                }
            }
            Input::Call(mail) => {
                // All IO tasks share this current-thread runtime. Drain prior
                // send progress before cancel/detach decisions: an initiated
                // write must never be mislabeled as not sent.
                while let Some(event) = events.try_recv() {
                    if let HostEvent::Ended { channel_id, .. } = &event {
                        retired.remove(channel_id);
                    }
                    match driver.handle(event) {
                        Ok(update) => updates.push(update),
                        Err(_) => {
                            updates.push(driver.state_failed());
                            running = false;
                            break;
                        }
                    }
                }
                macro_rules! respond {
                    ($reply:expr, $result:expr) => {
                        match $result {
                            Ok(update) => {
                                updates.push(update);
                                let _ = $reply.send(Ok(()));
                            }
                            Err(error) => {
                                if matches!(
                                    error,
                                    ClientError::InternalFailure { .. } | ClientError::ClosedClient
                                ) {
                                    updates.push(driver.state_failed());
                                    running = false;
                                }
                                let _ = $reply.send(Err(error));
                            }
                        }
                    };
                }
                match mail.call {
                    Call::Query(query, reply) => {
                        let _ = reply.send(driver.query(query));
                    }
                    Call::Context(reply) => {
                        let _ = reply.send(Ok(driver.connection_context()));
                    }
                    Call::Workspaces(reply) => {
                        let _ = reply.send(Ok(driver.workspaces()));
                    }
                    Call::Plan(intent, reply) => {
                        if callbacks.faulted.load(Ordering::Acquire) {
                            let _ = reply.send(Err(internal("observer_capacity")));
                        } else {
                            match driver.plan(intent) {
                                Ok(plan) => {
                                    updates.push(plan.update);
                                    let _ = reply.send(Ok(plan.plan));
                                }
                                Err(error) => {
                                    if matches!(
                                        error,
                                        ClientError::InternalFailure { .. }
                                            | ClientError::ClosedClient
                                    ) {
                                        updates.push(driver.state_failed());
                                        running = false;
                                    }
                                    let _ = reply.send(Err(error));
                                }
                            }
                        }
                    }
                    Call::Confirm(id, reply) => respond!(reply, driver.confirm(id)),
                    Call::Cancel(id, reply) => respond!(reply, driver.cancel_confirmation(id)),
                    Call::Connect(profile, endpoint, workspace, reply) => {
                        respond!(reply, driver.connect(profile, endpoint, workspace))
                    }
                    Call::Select(id, reply) => respond!(reply, driver.select_workspace(id)),
                    Call::Reconnect(reply) => respond!(reply, driver.reconnect()),
                    Call::Disconnect(reply) => respond!(reply, driver.disconnect()),
                    Call::Lifecycle(active, reply) => {
                        respond!(reply, driver.handle(HostEvent::Lifecycle { active }))
                    }
                    Call::Observe(observer, reply) => {
                        let result = callbacks.observe(observer);
                        if result.is_ok() {
                            let mut update = HostUpdate {
                                notice: driver.notice(),
                                effects: Vec::new(),
                                terminal: Vec::new(),
                            };
                            update.notice.changes.reset = true;
                            updates.push(update);
                        }
                        let _ = reply.send(result);
                    }
                    Call::Close(reply) => {
                        callbacks.stop();
                        match driver.close() {
                            Ok(update) => {
                                updates.push(update);
                                let _ = reply.send(Ok(()));
                            }
                            Err(error) => {
                                updates.push(driver.state_failed());
                                let _ = reply.send(Err(error));
                            }
                        }
                        running = false;
                    }
                }
            }
            Input::End => {
                running = false;
                if let Ok(update) = driver.close() {
                    updates.push(update);
                }
            }
        }
        let mut updates = updates.into_iter();
        while let Some(update) = updates.next() {
            let HostUpdate {
                notice,
                effects,
                terminal,
            } = update;
            execute(
                effects
                    .into_iter()
                    .filter(|effect| driver.effect_is_current(effect))
                    .collect(),
                &connector,
                &events,
                &mut channels,
                &mut retired,
                &mut timers,
                &timer_tx,
                &clock,
            );
            let update = HostUpdate {
                notice,
                effects: Vec::new(),
                terminal,
            };
            if callbacks.publish(&update).is_err() {
                let mut outcomes = update.notice.outcomes;
                for skipped in updates.by_ref() {
                    outcomes.extend(skipped.notice.outcomes);
                }
                // Preserve queued write progress before fencing. State/data
                // frames can be discarded only because this is a disconnect.
                while let Some(event) = events.try_recv() {
                    if matches!(event, HostEvent::SendProgress { .. }) {
                        if let Ok(progress) = driver.handle(event) {
                            outcomes.extend(progress.notice.outcomes);
                        }
                    }
                }
                if let Ok(mut ended) = driver.observer_overflow() {
                    outcomes.append(&mut ended.notice.outcomes);
                    ended.notice.outcomes = outcomes;
                    execute(
                        std::mem::take(&mut ended.effects),
                        &connector,
                        &events,
                        &mut channels,
                        &mut retired,
                        &mut timers,
                        &timer_tx,
                        &clock,
                    );
                    callbacks.final_update(ended);
                }
                break;
            }
        }
    }
    for (_, timer) in timers {
        timer.abort();
    }
    for (_, mut channel) in channels.into_iter().chain(retired) {
        channel.close();
        channel.wait_closed().await;
    }
    events.close();
}

#[allow(clippy::too_many_arguments)]
fn execute<A: Connector>(
    effects: Vec<HostEffect>,
    connector: &A,
    events: &EventQueue,
    channels: &mut BTreeMap<ChannelId, Box<dyn ChannelHandle>>,
    retired: &mut BTreeMap<ChannelId, Box<dyn ChannelHandle>>,
    timers: &mut BTreeMap<u64, tokio::task::JoinHandle<()>>,
    timer_tx: &mpsc::Sender<u64>,
    clock: &Arc<dyn Clock>,
) {
    for effect in effects {
        match effect {
            HostEffect::Open { channel_id, .. } => match connector.open(&effect, events) {
                Ok(channel) => {
                    channels.insert(channel_id, channel);
                }
                Err(reason) => {
                    if let Ok(sink) = events.register(channel_id) {
                        sink.end(reason);
                    }
                }
            },
            HostEffect::Send {
                channel_id,
                request_id,
                text,
            } => {
                if let Some(channel) = channels.get(&channel_id) {
                    if let Err(reason) = channel.send(request_id, text) {
                        channel.fail(reason);
                    }
                }
            }
            HostEffect::Close { channel_id } => {
                if let Some(channel) = channels.remove(&channel_id) {
                    channel.close();
                    retired.insert(channel_id, channel);
                }
            }
            HostEffect::Schedule {
                timer_id,
                deadline_ms,
            } => {
                if let Some(timer) = timers.remove(&timer_id) {
                    timer.abort();
                }
                let sender = timer_tx.clone();
                let delay = deadline_ms.saturating_sub(clock.sample().monotonic_ms);
                timers.insert(
                    timer_id,
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        let _ = sender.send(timer_id).await;
                    }),
                );
            }
            HostEffect::CancelTimer { timer_id } => {
                if let Some(timer) = timers.remove(&timer_id) {
                    timer.abort();
                }
            }
        }
    }
}

fn bounded_size(value: &impl Serialize) -> Result<usize, ClientError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_FRAME_BYTES {
                return Err(std::io::Error::other("native value limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| ClientError::TooLarge {
        actual: counter.0 as u64,
        limit: MAX_FRAME_BYTES as u64,
    })?;
    Ok(counter.0)
}

fn internal(code: &str) -> ClientError {
    ClientError::InternalFailure { code: code.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SlowObserver {
        entered: SyncSender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
        first: AtomicBool,
        notices: sync::Sender<ClientNotice>,
    }

    impl ClientObserver for SlowObserver {
        fn changed(&self, notice: ClientNotice) {
            if !self.first.swap(true, Ordering::SeqCst) {
                self.entered.send(()).unwrap();
                let (lock, wake) = &*self.release;
                let mut ready = lock.lock().unwrap();
                while !*ready {
                    ready = wake.wait(ready).unwrap();
                }
            }
            self.notices.send(notice).unwrap();
        }
        fn terminal(&self, _: TerminalChunk) {}
    }

    fn update(revision: u64) -> HostUpdate {
        HostUpdate {
            notice: ClientNotice {
                scope: None,
                revision,
                changes: ChangeSet {
                    revision,
                    ..ChangeSet::default()
                },
                endpoint: EndpointStatus::Authenticated,
                workspace: WorkspaceStatus::Live,
                reason: None,
                outcomes: Vec::new(),
            },
            effects: Vec::new(),
            terminal: Vec::new(),
        }
    }

    #[test]
    fn slow_callback_queue_is_bounded_and_preserves_reserved_final_outcomes() {
        let callbacks = Arc::new(Callbacks::new());
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered, waiting) = sync::sync_channel(1);
        let (notices, received) = sync::channel();
        callbacks
            .observe(Some(Arc::new(SlowObserver {
                entered,
                release: release.clone(),
                first: AtomicBool::new(false),
                notices,
            })))
            .unwrap();
        let receiver = callbacks.clone();
        let worker = std::thread::spawn(move || receiver.dispatch());
        callbacks.publish(&update(0)).unwrap();
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        for revision in 1..=QUEUE_FRAMES {
            callbacks.publish(&update(revision as u64)).unwrap();
        }
        assert!(callbacks.publish(&update(999)).is_err());
        let mut final_update = update(1000);
        final_update.notice.endpoint = EndpointStatus::Reconnecting;
        final_update.notice.reason = Some(TransportEnd::Overflow.reason());
        final_update.notice.outcomes.push(OperationNotice {
            scope: ClientScope {
                endpoint_id: "fixture".into(),
                workspace_id: "workspace".into(),
                generation: None,
                daemon_pid: Some(1),
                daemon_started_at_ms: Some(1),
                attempt: 1,
            },
            operation_id: 7,
            kind: OperationOutcomeKind::Unknown,
            reason: Some(TransportEnd::Overflow.reason()),
            attachment_id: None,
        });
        callbacks.final_update(final_update);
        {
            let (lock, wake) = &*release;
            *lock.lock().unwrap() = true;
            wake.notify_one();
        }
        let mut notices = Vec::new();
        for _ in 0..QUEUE_FRAMES + 2 {
            notices.push(received.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        let final_notice = notices.last().unwrap();
        assert_eq!(final_notice.endpoint, EndpointStatus::Reconnecting);
        assert!(final_notice.changes.reset);
        assert_eq!(final_notice.outcomes.len(), 1);
        assert_eq!(final_notice.outcomes[0].operation_id, 7);
        assert!(!callbacks.faulted.load(Ordering::Acquire));
        callbacks.finish();
        worker.join().unwrap();
    }

    #[test]
    fn callback_byte_budget_counts_terminal_data_even_below_frame_count_limit() {
        struct Observer;
        impl ClientObserver for Observer {
            fn changed(&self, _: ClientNotice) {}
            fn terminal(&self, _: TerminalChunk) {}
        }
        let callbacks = Callbacks::new();
        callbacks.observe(Some(Arc::new(Observer))).unwrap();
        let mut frame = update(1);
        frame.terminal.push(TerminalChunk {
            scope: ClientScope {
                endpoint_id: "fixture".into(),
                workspace_id: "workspace".into(),
                generation: None,
                daemon_pid: Some(1),
                daemon_started_at_ms: Some(1),
                attempt: 1,
            },
            attachment_id: 1,
            data: vec![0; MAX_FRAME_BYTES],
            ended: None,
        });
        for _ in 0..3 {
            callbacks.publish(&frame).unwrap();
        }
        assert!(callbacks.publish(&frame).is_err());
        let state = callbacks.state.lock().unwrap();
        assert_eq!(state.queue.len(), 3);
        assert!(state.bytes <= QUEUE_BYTES);
    }
}
