//! Shared context for the negotiated feature services.
//!
//! These records describe routing and cancellation. They do not grant access:
//! the daemon checks authentication, feature readiness and scope before calling
//! a service. Defining this interface does not advertise any feature version.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, RwLock};

use crew_core::rpc::features::{
    Feature, FeatureAccess, FeatureSession, FeatureUnavailable, FeatureVersions, FeaturesNegotiate,
    Id, UnavailableFeatures, FEATURE_VERSION,
};
use crew_core::rpc::{Call, RpcError};
use serde_json::Value;
use tokio::sync::Notify;

use super::server::{Conn, Who};
use super::Daemon;

mod audit;
mod tasks;
mod watch;
pub use audit::OperatorAudit;
pub use tasks::{CommittedTasks, TaskChange, TaskModel, TaskProjection, TaskPublication, TaskView};
pub use watch::FeatureWatch;
pub(crate) use watch::PendingWatch;

/// A complete domain implementation. Registration alone grants no access;
/// readiness, authenticated role, negotiated version and scope are checked by
/// the central dispatcher before this port is invoked.
#[async_trait::async_trait]
pub trait FeatureService: Send + Sync {
    fn feature(&self) -> Feature;
    fn readiness(&self) -> Readiness;
    async fn handle(&self, context: &ConnCtx, call: Call) -> Result<Value, FeatureError>;
    fn watch_stream(
        &self,
        _context: ConnCtx,
        _watch_id: Id,
    ) -> Result<Box<dyn FeatureWatch>, FeatureError> {
        Err(RpcError::new(
            "unsupported_feature",
            "observer watches are unavailable",
        ))
    }
    fn connection_closed(&self, _connection: &ConnId) {}
    fn run_archived(&self, _run: &RunCtx) {}
    /// Cancel and await owned jobs/reaping before daemon teardown. A disconnected
    /// observer does not use this hook and cannot cancel workspace-owned work.
    async fn shutdown(&self) {}
}

#[derive(Default)]
pub struct FeatureServices {
    services: RwLock<BTreeMap<Feature, Arc<dyn FeatureService>>>,
    connections: Mutex<HashMap<ConnId, ConnCtx>>,
    task_projection: RwLock<Option<Arc<dyn TaskProjection>>>,
    stopping: AtomicBool,
    shutdown_complete: tokio::sync::Mutex<bool>,
}

impl FeatureServices {
    pub fn register_task_projection(&self, projection: Arc<dyn TaskProjection>) {
        *self
            .task_projection
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(projection);
    }

    pub(crate) fn task_projection(&self) -> Option<Arc<dyn TaskProjection>> {
        self.task_projection
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn register(&self, service: Arc<dyn FeatureService>) {
        self.services
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(service.feature(), service);
    }

    fn get(&self, feature: Feature) -> Option<Arc<dyn FeatureService>> {
        self.services
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&feature)
            .cloned()
    }

    pub(crate) fn connection_closed(&self, id: &ConnId) {
        if let Some(context) = self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
        {
            context.connection_closed.cancel();
            context.cancellation.cancel();
        }
        self.close_connection_resources(id);
    }

    fn connection_scope_changed(&self, id: &ConnId) {
        if let Some(context) = self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
        {
            context.cancellation.cancel();
        }
        self.close_connection_resources(id);
    }

    fn close_connection_resources(&self, id: &ConnId) {
        let services: Vec<_> = self
            .services
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for service in services {
            service.connection_closed(id);
        }
    }

    pub(crate) fn run_archived(&self, run: &RunCtx) {
        let contexts: Vec<_> = self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|context| &context.run == run)
            .cloned()
            .collect();
        for context in contexts {
            context.cancellation.cancel();
        }
        let services: Vec<_> = self
            .services
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for service in services {
            service.run_archived(run);
        }
    }

    pub(crate) fn connection_opened(&self, context: ConnCtx) {
        self.connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(context.connection_epoch.clone(), context);
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let mut completed = self.shutdown_complete.lock().await;
        if *completed {
            return;
        }
        let contexts: Vec<_> = self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for context in contexts {
            context.cancellation.cancel();
        }
        let services: Vec<_> = self
            .services
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for service in services {
            service.shutdown().await;
        }
        *completed = true;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Negotiated {
    requested: FeatureVersions,
    accepted: FeatureVersions,
    unavailable: UnavailableFeatures,
}

impl Negotiated {
    pub(crate) fn accepted(&self, feature: Feature) -> bool {
        self.accepted.get(feature.name()) == Some(&FEATURE_VERSION)
    }
}

impl Daemon {
    pub(crate) fn run_context(&self) -> Result<RunCtx, RpcError> {
        Ok(RunCtx {
            workspace_run_id: self.feature_db.current_run()?.id,
        })
    }

    pub(crate) fn negotiate_features(
        &self,
        conn: &mut Conn,
        request: FeaturesNegotiate,
    ) -> Result<Value, RpcError> {
        let who = conn
            .who
            .as_ref()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?;
        if self.feature_services.stopping.load(Ordering::Acquire) {
            return Err(RpcError::unavailable("feature services are stopping"));
        }
        self.check_feature_member(conn)?;
        if let Some(previous) = &conn.negotiated {
            if previous.requested != request.versions {
                return Err(RpcError::conflict(
                    "feature versions are fixed on this connection",
                ));
            }
        } else {
            if conn.representation_started {
                return Err(RpcError::conflict(
                    "negotiate before Snapshot, Subscribe or feature operations",
                ));
            }
            let mut accepted = BTreeMap::new();
            let mut unavailable = BTreeMap::new();
            for (name, version) in request.versions.iter() {
                let feature = Feature::ALL
                    .into_iter()
                    .find(|feature| feature.name() == name);
                let reason = match feature {
                    None => Some(FeatureUnavailable::Unsupported),
                    Some(_) if *version != FEATURE_VERSION => {
                        Some(FeatureUnavailable::UnsupportedVersion)
                    }
                    Some(feature) if matches!(who, Who::Agent(_)) && feature != Feature::Epics => {
                        Some(FeatureUnavailable::NotAuthorized)
                    }
                    Some(Feature::Epics) if self.feature_services.task_projection().is_none() => {
                        Some(FeatureUnavailable::NotReady)
                    }
                    Some(feature) => match self.feature_services.get(feature) {
                        None => Some(FeatureUnavailable::Unsupported),
                        Some(service) => match service.readiness() {
                            Readiness::Ready => None,
                            Readiness::Unavailable(reason) => Some(reason),
                        },
                    },
                };
                if let Some(reason) = reason {
                    unavailable.insert(name.clone(), reason);
                } else {
                    accepted.insert(name.clone(), *version);
                }
            }
            conn.negotiated = Some(Negotiated {
                requested: request.versions,
                accepted: FeatureVersions::try_from(accepted).map_err(RpcError::invalid)?,
                unavailable: UnavailableFeatures::try_from(unavailable)
                    .map_err(RpcError::invalid)?,
            });
        }
        let negotiated = conn.negotiated.as_ref().expect("negotiation stored");
        let epoch = conn
            .feature_context
            .as_ref()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?
            .connection_epoch
            .clone();
        serde_json::to_value(FeatureSession {
            accepted: negotiated.accepted.clone(),
            connection_epoch: epoch,
            workspace_run_id: Some(self.run_context()?.workspace_run_id),
            unavailable: negotiated.unavailable.clone(),
        })
        .map_err(|_| RpcError::new("internal", "feature response encoding failed"))
    }

    pub(crate) async fn dispatch_feature(
        &self,
        conn: &mut Conn,
        call: Call,
    ) -> Result<Value, RpcError> {
        conn.representation_started = true;
        let method =
            crew_core::rpc::features::feature_method(call.method()).expect("known feature method");
        let who = conn
            .who
            .as_ref()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?;
        if self.feature_services.stopping.load(Ordering::Acquire) {
            return Err(RpcError::unavailable("feature services are stopping"));
        }
        self.check_feature_member(conn)?;
        let allowed = match method.access {
            FeatureAccess::CollaborativeRead => true,
            FeatureAccess::Administration => {
                matches!(who, Who::Operator)
                    || matches!(who, Who::Agent(id) if self.is_orchestrator(id))
            }
            FeatureAccess::ObserverRead
            | FeatureAccess::ObserverAction
            | FeatureAccess::ObserverWatch => matches!(who, Who::Operator),
            FeatureAccess::Discovery => false,
        };
        if !allowed {
            return Err(RpcError::forbidden(
                "feature operation is not available to this caller",
            ));
        }
        let feature = method.feature.expect("domain feature method");
        let unsupported = || {
            RpcError::new(
                "unsupported_feature",
                "feature is unavailable on this connection",
            )
        };
        if !conn.has_feature(feature) {
            return Err(unsupported());
        }
        let service = self.feature_services.get(feature).ok_or_else(unsupported)?;
        if service.readiness() != Readiness::Ready {
            return Err(unsupported());
        }
        let mut context = conn
            .feature_context
            .as_ref()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?
            .clone();
        let current = self.run_context()?;
        if context.run != current {
            context.cancellation.cancel();
            self.feature_services
                .connection_scope_changed(&context.connection_epoch);
            context = context.for_run(current);
            self.feature_services.connection_opened(context.clone());
            conn.feature_context = Some(context.clone());
        }
        if expected_current_run(&call).is_some_and(|run| run != &context.run.workspace_run_id) {
            return Err(RpcError::new("stale_scope", "workspace run changed"));
        }
        if let Some(run) = metrics_run(&call) {
            if !self.feature_db.has_run(run)? {
                return Err(RpcError::not_found("workspace run unavailable"));
            }
        }
        let starts_watch = matches!(
            call,
            Call::AsideWatch(_) | Call::DocsWatch(_) | Call::UsageWatch(_)
        );
        let stops_watch = match &call {
            Call::AsideUnwatch(p) | Call::DocsUnwatch(p) | Call::UsageUnwatch(p) => {
                Some(p.watch_id.clone())
            }
            _ => None,
        };
        let watch_limit = match feature {
            Feature::Asides | Feature::Docs => 1,
            Feature::UsageMetrics => 4,
            Feature::Epics => 0,
        };
        if starts_watch
            && feature != Feature::Docs
            && conn.feature_watch_count(feature) >= watch_limit
        {
            return Err(RpcError::new("busy", "feature watch limit reached"));
        }
        let result = service.handle(&context, call).await?;
        if context.cancellation.is_cancelled() || self.run_context()? != context.run {
            return Err(RpcError::new(
                "stale_scope",
                "feature operation scope expired",
            ));
        }
        let envelope = serde_json::json!({"id":u64::MAX,"result":&result});
        let encoded = crew_core::rpc::features::encode_frame(&envelope)
            .map_err(|_| RpcError::too_large("feature response exceeds transport limit"))?;
        if encoded.len() > crew_core::rpc::features::MAX_PAGE_BYTES {
            return Err(RpcError::too_large("feature response exceeds page limit"));
        }
        if starts_watch {
            let pending = PendingWatch::new(feature, &result, context.clone(), service.as_ref())?;
            if conn.feature_watch_exists(&pending.id)
                && !(feature == Feature::Docs && conn.watch_belongs_to(Feature::Docs, &pending.id))
            {
                return Err(RpcError::conflict(
                    "watch id is already registered on this connection",
                ));
            }
            if feature == Feature::Docs {
                // The domain validates the whole new selection before replacing
                // it. Failed preparation leaves the existing forwarder alone.
                // Successful replacement retires it before the new reply barrier.
                conn.stop_feature_family(Feature::Docs).await;
            }
            conn.feature_after_reply.push(pending);
        }
        if let Some(id) = stops_watch {
            conn.stop_feature_watch(feature, &id);
        }
        Ok(result)
    }

    fn check_feature_member(&self, conn: &Conn) -> Result<(), RpcError> {
        let context = conn
            .feature_context
            .as_ref()
            .ok_or_else(|| RpcError::unauthorized("say hello first"))?;
        let model = self.model();
        self.feature_db
            .read(|db| check_member_at(db, &model, context))
    }
}

pub(crate) fn check_member_at(
    db: &rusqlite::Connection,
    model: &super::Model,
    context: &ConnCtx,
) -> Result<(), RpcError> {
    use rusqlite::OptionalExtension;
    if context.connection_closed.is_cancelled() {
        return Err(RpcError::unauthorized("connection closed"));
    }
    if let Caller::Agent { id } = &context.caller {
        let current: Option<String> = db.query_row(
            "SELECT incarnation FROM feature_members WHERE run_id=?1 AND agent_id=?2 AND retired_at IS NULL",
            rusqlite::params![context.run.workspace_run_id.as_str(), id], |row| row.get(0),
        ).optional().map_err(crate::persistence::features::storage_error)?;
        if model.agent(id).is_none()
            || current.as_deref().is_none()
            || current.as_deref() != context.member_incarnation.as_ref().map(Id::as_str)
        {
            return Err(RpcError::unauthorized("member identity expired; reconnect"));
        }
    }
    Ok(())
}

fn expected_current_run(call: &Call) -> Option<&Id> {
    match call {
        Call::EpicsList(scope) => Some(&scope.workspace_run_id),
        Call::EpicGet(p) => Some(&p.workspace_run_id),
        Call::EpicCreate(p) => Some(&p.workspace_run_id),
        Call::EpicEdit(p) => Some(&p.workspace_run_id),
        Call::TasksQuery(p) => Some(&p.workspace_run_id),
        Call::TaskGetScoped(p) => Some(&p.workspace_run_id),
        Call::TaskCreateScoped(p) => Some(&p.workspace_run_id),
        Call::TaskRevise(p) => Some(&p.workspace_run_id),
        Call::EpicComplete(p) => Some(&p.workspace_run_id),
        Call::AsideMembers(p) | Call::AsideList(p) | Call::AsideWatch(p) => {
            Some(&p.workspace_run_id)
        }
        Call::AsideGet(p) => Some(&p.workspace_run_id),
        Call::AsideAsk(p) => Some(&p.workspace_run_id),
        Call::AsideCancel(p) => Some(&p.workspace_run_id),
        Call::AsideRemove(p) => Some(&p.workspace_run_id),
        _ => None,
    }
}

fn metrics_run(call: &Call) -> Option<&Id> {
    match call {
        Call::UsageSummary(query)
        | Call::UsageSeries(query)
        | Call::UsageBreakdown(query)
        | Call::UsageObservations(query)
        | Call::UsageWatch(query) => Some(&query.scope.workspace_run_id),
        _ => None,
    }
}

/// The physical authenticated connection's epoch, not its reusable principal.
pub type ConnId = Id;

/// Services use the existing finite RPC error envelope.
pub type FeatureError = RpcError;

/// The principal authenticated by the dispatcher. Request bodies cannot
/// choose a sender or turn an agent into the operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    Operator,
    Agent { id: String },
}

impl Caller {
    pub fn sender(&self) -> &str {
        match self {
            Self::Operator => crew_core::OPERATOR_ID,
            Self::Agent { id } => id,
        }
    }
}

pub fn check_task_message_size(task: &crew_core::Task, limit: usize) -> Result<(), FeatureError> {
    super::orchestrate::check_task_message_size(task, limit)
}

pub fn check_message_body(body: &str, limit: usize) -> Result<(), FeatureError> {
    crate::message_bus::check_message_body(body, limit)
        .map_err(|error| RpcError::invalid(error.to_string()))
}

pub fn check_report_decisions(
    caller: &Caller,
    decisions: Vec<String>,
    max_decisions: usize,
    message_limit: usize,
) -> Result<Vec<String>, FeatureError> {
    let who = match caller {
        Caller::Operator => Who::Operator,
        Caller::Agent { id } => Who::Agent(id.clone()),
    };
    let decisions = super::questions::check_report_decisions(&who, decisions, max_decisions)?;
    for body in &decisions {
        check_message_body(body, message_limit)?;
    }
    Ok(decisions)
}

/// Whether the complete service contract can be negotiated on a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Unavailable(FeatureUnavailable),
}

/// A captured workspace run. Mutations still validate the run under the same
/// transaction or model lock as their write; this value is not a write fence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCtx {
    pub workspace_run_id: Id,
}

/// Context shared with one connection's feature jobs and independent watches.
/// A cloned context retains its original epoch; it cannot be retagged onto a
/// replacement connection. Dropping a job's clone does not cancel its peers.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub connection_epoch: ConnId,
    pub run: RunCtx,
    pub caller: Caller,
    /// Captured at Hello under the member/token fence. Reusing a public id
    /// cannot promote an old authenticated connection into the new member.
    pub member_incarnation: Option<Id>,
    pub cancellation: Cancellation,
    /// Physical connection closure, distinct from expiration of one run's
    /// jobs. A still-open connection receives a final watch resync on expiry.
    pub connection_closed: Cancellation,
}

impl ConnCtx {
    pub fn new(connection_epoch: ConnId, run: RunCtx, caller: Caller) -> Self {
        Self {
            connection_epoch,
            run,
            caller,
            member_incarnation: None,
            cancellation: Cancellation::default(),
            connection_closed: Cancellation::default(),
        }
    }

    fn for_run(&self, run: RunCtx) -> Self {
        Self {
            run,
            cancellation: Cancellation::default(),
            ..self.clone()
        }
    }
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

/// Cancellation is a signal, not a release of a worker permit. A blocking job
/// keeps its permit and resources until it actually finishes.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<CancellationState>);

impl Cancellation {
    pub fn cancel(&self) {
        if !self.0.cancelled.swap(true, Ordering::AcqRel) {
            self.0.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        // Register before checking the sticky flag. notify_waiters does not
        // retain a permit for a future that has not registered yet.
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::server::Session;
    use super::*;
    use crew_core::rpc::ServerFrame;
    use serde_json::json;
    use std::time::Duration;

    struct ReadyDocs;

    struct MetricsSpy {
        ready: AtomicBool,
        runs: Mutex<Vec<Id>>,
    }

    #[async_trait::async_trait]
    impl FeatureService for MetricsSpy {
        fn feature(&self) -> Feature {
            Feature::UsageMetrics
        }
        fn readiness(&self) -> Readiness {
            if self.ready.load(Ordering::Acquire) {
                Readiness::Ready
            } else {
                Readiness::Unavailable(FeatureUnavailable::NotReady)
            }
        }
        async fn handle(&self, _: &ConnCtx, call: Call) -> Result<Value, RpcError> {
            if let Some(run) = metrics_run(&call) {
                self.runs.lock().unwrap().push(run.clone());
            }
            Ok(json!({}))
        }
    }

    fn metrics_query(run: &Id) -> Value {
        let fixtures: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../crew-core/tests/fixtures/features/requests-v1.json"
        ))
        .unwrap();
        let mut query = fixtures
            .into_iter()
            .find(|call| call["method"] == "usage_summary")
            .unwrap()["params"]
            .clone();
        query["scope"]["workspace_run_id"] = json!(run);
        query
    }

    fn install_member(daemon: &Daemon, orchestrator: bool, token: &str) {
        let agent: crew_core::Agent=serde_json::from_value(json!({"id":"worker","name":"worker","role":"work","runtime":"shell",
            "status":"waiting","workdir":"/tmp","backend":"pty","last_activity_at":0,"orchestrator":orchestrator})).unwrap();
        daemon.store.save_agent(&agent).unwrap();
        daemon.store.save_token("worker", token).unwrap();
        let mut model = daemon.model();
        model.agents.retain(|member| member.id != "worker");
        model.agents.push(agent);
        model.tokens.insert("worker".into(), token.into());
    }

    #[tokio::test]
    async fn metrics_dispatch_checks_auth_capability_readiness_and_workspace_history_before_handler(
    ) {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let spy = Arc::new(MetricsSpy {
            ready: AtomicBool::new(true),
            runs: Mutex::new(Vec::new()),
        });
        daemon.feature_services.register(spy.clone());
        let old = daemon.run_context().unwrap();
        let (mut session, mut rx) = Session::new(daemon);
        let unauthorized = request(
            &mut session,
            &mut rx,
            "usage_summary",
            metrics_query(&old.workspace_run_id),
        )
        .await;
        assert_eq!(unauthorized.error.unwrap().code, "unauthorized");
        hello(daemon, &mut session, &mut rx).await;
        let absent = request(
            &mut session,
            &mut rx,
            "usage_summary",
            metrics_query(&old.workspace_run_id),
        )
        .await;
        assert_eq!(absent.error.unwrap().code, "unsupported_feature");
        // A separate connection negotiates before its first feature operation.
        let (mut scoped, mut scoped_rx) = Session::new(daemon);
        hello(daemon, &mut scoped, &mut scoped_rx).await;
        let accepted = request(
            &mut scoped,
            &mut scoped_rx,
            "features_negotiate",
            json!({"versions":{"usage_metrics":1}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(accepted["accepted"], json!({"usage_metrics":1}));
        spy.ready.store(false, Ordering::Release);
        let unready = request(
            &mut scoped,
            &mut scoped_rx,
            "usage_summary",
            metrics_query(&old.workspace_run_id),
        )
        .await;
        assert_eq!(unready.error.unwrap().code, "unsupported_feature");
        spy.ready.store(true, Ordering::Release);
        for method in [
            "usage_summary",
            "usage_series",
            "usage_breakdown",
            "usage_observations",
        ] {
            let foreign = request(
                &mut scoped,
                &mut scoped_rx,
                method,
                metrics_query(&Id::new("another-workspace-run").unwrap()),
            )
            .await;
            assert_eq!(foreign.error.unwrap().code, "not_found", "{method}");
        }
        assert!(spy.runs.lock().unwrap().is_empty());
        daemon
            .feature_db
            .clear_session(&old.workspace_run_id, |_| Ok(()))
            .unwrap();
        daemon.feature_services.run_archived(&old);
        let historical = request(
            &mut scoped,
            &mut scoped_rx,
            "usage_summary",
            metrics_query(&old.workspace_run_id),
        )
        .await;
        assert!(historical.error.is_none(), "{:?}", historical.error);
        assert_eq!(
            *spy.runs.lock().unwrap(),
            vec![old.workspace_run_id.clone()]
        );
        let repeated = request(
            &mut scoped,
            &mut scoped_rx,
            "features_negotiate",
            json!({"versions":{"usage_metrics":1}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(repeated["connection_epoch"], accepted["connection_epoch"]);
        assert_ne!(repeated["workspace_run_id"], accepted["workspace_run_id"]);
        daemon.shutdown(false).await;
    }

    #[tokio::test]
    async fn removed_reused_member_connections_never_acquire_new_role_or_observer_authority() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        install_member(daemon, false, "first-token");
        let spy = Arc::new(MetricsSpy {
            ready: AtomicBool::new(true),
            runs: Mutex::new(Vec::new()),
        });
        daemon.feature_services.register(spy.clone());
        let run = daemon.run_context().unwrap();
        let (mut session, mut rx) = Session::new(daemon);
        let hello=request(&mut session,&mut rx,"hello",json!({"role":"agent","agent_id":"worker","token":"first-token","protocol":6,"client":"fixture"})).await;
        assert!(hello.error.is_none(), "{:?}", hello.error);
        let negotiated = request(
            &mut session,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"usage_metrics":1}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(negotiated["unavailable"]["usage_metrics"], "not_authorized");
        let forbidden = request(
            &mut session,
            &mut rx,
            "usage_summary",
            metrics_query(&run.workspace_run_id),
        )
        .await;
        assert_eq!(forbidden.error.unwrap().code, "forbidden");
        let incarnation = daemon
            .feature_db
            .incarnation(&run.workspace_run_id, "worker")
            .unwrap()
            .unwrap();
        {
            let mut model = daemon.model();
            daemon.store.delete_agent("worker").unwrap();
            model.agents.clear();
            model.tokens.clear();
        }
        install_member(daemon, true, "new-token");
        assert_ne!(
            daemon
                .feature_db
                .incarnation(&run.workspace_run_id, "worker")
                .unwrap(),
            Some(incarnation)
        );
        for (method, params) in [
            (
                "features_negotiate",
                json!({"versions":{"usage_metrics":1}}),
            ),
            ("usage_summary", metrics_query(&run.workspace_run_id)),
        ] {
            let expired = request(&mut session, &mut rx, method, params).await;
            assert_eq!(expired.error.unwrap().code, "unauthorized", "{method}");
        }
        assert!(spy.runs.lock().unwrap().is_empty());
        daemon.shutdown(false).await;
    }

    struct ShutdownSpy {
        entered: Notify,
        release: Notify,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[tokio::test]
    async fn operator_audit_is_body_free_model_independent_and_absent_from_agent_events() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        install_member(daemon, false, "token");
        let run = daemon.run_context().unwrap();
        let event = {
            let _model = daemon.model();
            daemon
                .record_operator_audit(OperatorAudit::AsideAsked(
                    crew_core::rpc::asides::AsideAsked {
                        workspace_run_id: run.workspace_run_id.clone(),
                        id: Id::new("aside").unwrap(),
                        agents: Default::default(),
                    },
                ))
                .unwrap()
        };
        assert_eq!(event.event_type, "aside.asked");
        assert_eq!(event.agent_id, None);
        assert_eq!(event.task_id, None);
        assert_eq!(event.message, "aside.asked");
        assert!(event.data.get("body").is_none());
        // An old/imported row must not bypass the closed operator event family.
        daemon
            .store
            .append_event(
                crate::persistence::NewEvent::new("asides", "aside.answered", "metadata")
                    .agent("worker"),
            )
            .unwrap();
        daemon
            .store
            .append_event(
                crate::persistence::NewEvent::new("test", "task.fixture", "visible")
                    .agent("worker"),
            )
            .unwrap();
        let (mut agent, mut rx) = Session::new(daemon);
        let auth=request(&mut agent,&mut rx,"hello",json!({"role":"agent","agent_id":"worker","token":"token","protocol":6,"client":"fixture"})).await;
        assert!(auth.error.is_none());
        let events = request(
            &mut agent,
            &mut rx,
            "events",
            json!({"agent":"operator","limit":5000}),
        )
        .await
        .result
        .unwrap();
        let events = events.as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event_type"], "task.fixture");
        for method in ["snapshot", "subscribe"] {
            assert_eq!(
                request(&mut agent, &mut rx, method, json!({}))
                    .await
                    .error
                    .unwrap()
                    .code,
                "forbidden"
            );
        }
        let (mut operator, mut operator_rx) = Session::new(daemon);
        hello(daemon, &mut operator, &mut operator_rx).await;
        let all = request(
            &mut operator,
            &mut operator_rx,
            "events",
            json!({"limit":5000}),
        )
        .await
        .result
        .unwrap();
        assert!(all
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event_type"] == "aside.asked"));
        assert_eq!(daemon.model().agent("worker").unwrap().last_activity_at, 0);
        daemon.shutdown(false).await;
    }

    struct BoundWatchService {
        feature: Feature,
        next: std::sync::atomic::AtomicUsize,
    }
    struct Forever;
    #[async_trait::async_trait]
    impl FeatureWatch for Forever {
        async fn next(&mut self) -> Result<Option<crew_core::rpc::Push>, RpcError> {
            std::future::pending().await
        }
    }
    #[async_trait::async_trait]
    impl FeatureService for BoundWatchService {
        fn feature(&self) -> Feature {
            self.feature
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        async fn handle(&self, context: &ConnCtx, call: Call) -> Result<Value, RpcError> {
            let watch = format!(
                "{}-{}",
                self.feature.name(),
                self.next.fetch_add(1, Ordering::AcqRel)
            );
            match call {
                Call::AsideWatch(_) => Ok(
                    json!({"watch_id":watch,"initial":{"workspace_run_id":context.run.workspace_run_id,"aside_epoch":"aside-epoch","revision":"0"}}),
                ),
                Call::UsageWatch(query) => Ok(
                    json!({"watch_id":watch,"epoch":"usage-epoch","initial":{"query":query,"revision":"0"}}),
                ),
                _ => Err(RpcError::invalid("fixture watch only")),
            }
        }
        fn watch_stream(&self, _: ConnCtx, _: Id) -> Result<Box<dyn FeatureWatch>, RpcError> {
            Ok(Box::new(Forever))
        }
    }

    #[tokio::test]
    async fn arbitrary_clients_cannot_exceed_aside_and_metrics_watch_bounds() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let aside = Arc::new(BoundWatchService {
            feature: Feature::Asides,
            next: std::sync::atomic::AtomicUsize::new(0),
        });
        let metrics = Arc::new(BoundWatchService {
            feature: Feature::UsageMetrics,
            next: std::sync::atomic::AtomicUsize::new(0),
        });
        daemon.feature_services.register(aside.clone());
        daemon.feature_services.register(metrics.clone());
        let run = daemon.run_context().unwrap();
        let (mut session, mut rx) = Session::new(daemon);
        hello(daemon, &mut session, &mut rx).await;
        request(
            &mut session,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"asides":1,"usage_metrics":1}}),
        )
        .await;
        let params = json!({"workspace_run_id":run.workspace_run_id,"page":{}});
        assert!(
            request(&mut session, &mut rx, "aside_watch", params.clone())
                .await
                .error
                .is_none()
        );
        assert_eq!(
            request(&mut session, &mut rx, "aside_watch", params)
                .await
                .error
                .unwrap()
                .code,
            "busy"
        );
        for _ in 0..4 {
            assert!(request(
                &mut session,
                &mut rx,
                "usage_watch",
                metrics_query(&run.workspace_run_id)
            )
            .await
            .error
            .is_none());
        }
        assert_eq!(
            request(
                &mut session,
                &mut rx,
                "usage_watch",
                metrics_query(&run.workspace_run_id)
            )
            .await
            .error
            .unwrap()
            .code,
            "busy"
        );
        assert_eq!(aside.next.load(Ordering::Acquire), 1);
        assert_eq!(metrics.next.load(Ordering::Acquire), 4);
        assert_eq!(session.feature_watch_count(Feature::Asides), 1);
        assert_eq!(session.feature_watch_count(Feature::UsageMetrics), 4);
        drop(session);
        daemon.shutdown(false).await;
    }
    #[async_trait::async_trait]
    impl FeatureService for ShutdownSpy {
        fn feature(&self) -> Feature {
            Feature::Docs
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        async fn handle(&self, _: &ConnCtx, _: Call) -> Result<Value, RpcError> {
            Ok(json!({}))
        }
        async fn shutdown(&self) {
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    #[tokio::test]
    async fn shutdown_refuses_new_features_and_waits_for_owned_retirement_once() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let _shutdown_receiver = daemon.shutdown.subscribe();
        let spy = Arc::new(ShutdownSpy {
            entered: Notify::new(),
            release: Notify::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        daemon.feature_services.register(spy.clone());
        let first = daemon.clone();
        let first = tokio::spawn(async move { first.shutdown(false).await });
        tokio::time::timeout(Duration::from_secs(1), spy.entered.notified())
            .await
            .unwrap();
        let second = daemon.clone();
        let second = tokio::spawn(async move { second.shutdown(false).await });
        assert!(!*daemon.shutdown.borrow());
        assert!(!first.is_finished());
        let (mut session, mut rx) = Session::new(daemon);
        hello(daemon, &mut session, &mut rx).await;
        let stopping = request(
            &mut session,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1}}),
        )
        .await;
        assert_eq!(stopping.error.unwrap().code, "unavailable");
        spy.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap();
        assert!(*daemon.shutdown.borrow());
        assert_eq!(spy.calls.load(Ordering::Acquire), 1);
    }

    struct WatchedDocs {
        wake: Arc<Notify>,
    }
    struct OneChange {
        revision: u128,
        wake: Arc<Notify>,
    }

    #[async_trait::async_trait]
    impl FeatureWatch for OneChange {
        async fn next(&mut self) -> Result<Option<crew_core::rpc::Push>, RpcError> {
            if self.revision > 0 {
                self.wake.notified().await;
            }
            self.revision += 1;
            Ok(Some(crew_core::rpc::Push::DocsChanged(Box::new(
                crew_core::rpc::docs::DocsChanged {
                    epoch: Id::new("docs-epoch").unwrap(),
                    watch_id: Id::new("watch-1").unwrap(),
                    from_revision: crew_core::rpc::features::Wide(self.revision),
                    to_revision: crew_core::rpc::features::Wide(self.revision + 1),
                    upserts: Default::default(),
                    removed_doc_ids: Default::default(),
                    changed_selected: Default::default(),
                    history_sources_changed: Default::default(),
                    expired_seen_versions: Default::default(),
                    resync_required: false,
                },
            ))))
        }
    }

    #[async_trait::async_trait]
    impl FeatureService for WatchedDocs {
        fn feature(&self) -> Feature {
            Feature::Docs
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        async fn handle(&self, _: &ConnCtx, call: Call) -> Result<Value, RpcError> {
            match call {
                Call::DocsWatch(request) if !request.selected.is_empty() => {
                    Err(RpcError::invalid("fixture invalid selection"))
                }
                Call::DocsWatch(_) => {
                    Ok(json!({"watch_id":"watch-1","initial_index_page":{"epoch":"docs-epoch"}}))
                }
                _ => Ok(json!({})),
            }
        }
        fn watch_stream(&self, _: ConnCtx, _: Id) -> Result<Box<dyn FeatureWatch>, RpcError> {
            Ok(Box::new(OneChange {
                revision: 0,
                wake: self.wake.clone(),
            }))
        }
    }

    #[test]
    fn report_preflight_uses_actual_caller_and_shared_byte_limit() {
        let agent = Caller::Agent {
            id: "orchestrator".into(),
        };
        assert_eq!(agent.sender(), "orchestrator");
        assert!(check_report_decisions(&Caller::Operator, vec!["choose".into()], 64, 100).is_err());
        assert_eq!(
            check_report_decisions(
                &agent,
                vec![" choose ".into(), "choose".into(), "".into()],
                64,
                100
            )
            .unwrap(),
            vec!["choose"]
        );
        assert!(check_report_decisions(&agent, vec!["éé".into()], 64, 3).is_err());
    }

    #[async_trait::async_trait]
    impl FeatureService for ReadyDocs {
        fn feature(&self) -> Feature {
            Feature::Docs
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        async fn handle(&self, _: &ConnCtx, _: Call) -> Result<Value, RpcError> {
            Ok(json!({}))
        }
    }

    async fn request(
        session: &mut Session,
        rx: &mut tokio::sync::mpsc::Receiver<ServerFrame>,
        method: &str,
        params: Value,
    ) -> ServerFrame {
        assert!(
            session
                .request(&json!({"id":1,"method":method,"params":params}).to_string())
                .await
        );
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn fixture() -> (tempfile::TempDir, super::super::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        (temp, running)
    }

    async fn hello(
        daemon: &Daemon,
        session: &mut Session,
        rx: &mut tokio::sync::mpsc::Receiver<ServerFrame>,
    ) {
        let frame = request(session, rx, "hello", json!({"role":"operator","token":daemon.operator_token,"protocol":6,"client":"feature-fixture"})).await;
        assert!(frame.error.is_none(), "{:?}", frame.error);
    }

    #[tokio::test]
    async fn negotiation_is_authenticated_frozen_and_exactly_connection_scoped() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        daemon.feature_services.register(Arc::new(ReadyDocs));
        let (mut first, mut rx) = Session::new(daemon);
        let frame = request(
            &mut first,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1}}),
        )
        .await;
        assert_eq!(frame.error.unwrap().code, "unauthorized");
        hello(daemon, &mut first, &mut rx).await;
        let initial = request(
            &mut first,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1,"future":7}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(initial["accepted"], json!({"docs":1}));
        assert_eq!(initial["unavailable"]["future"], "unsupported");
        let repeated = request(
            &mut first,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1,"future":7}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(initial, repeated);
        let changed = request(
            &mut first,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1}}),
        )
        .await;
        assert_eq!(changed.error.unwrap().code, "conflict");
        let (mut second, mut rx) = Session::new(daemon);
        hello(daemon, &mut second, &mut rx).await;
        let replacement = request(
            &mut second,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1,"future":7}}),
        )
        .await
        .result
        .unwrap();
        assert_ne!(initial["connection_epoch"], replacement["connection_epoch"]);
        daemon.shutdown(true).await;
    }

    #[tokio::test]
    async fn snapshot_or_feature_operation_prevents_first_late_negotiation() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        for (method, params) in [
            ("snapshot", json!({})),
            ("docs_index", json!({"cursor":null,"limit":10})),
        ] {
            let (mut session, mut rx) = Session::new(daemon);
            hello(daemon, &mut session, &mut rx).await;
            request(&mut session, &mut rx, method, params).await;
            let late = request(
                &mut session,
                &mut rx,
                "features_negotiate",
                json!({"versions":{"docs":1}}),
            )
            .await;
            assert_eq!(late.error.unwrap().code, "conflict", "{method}");
        }
        daemon.shutdown(true).await;
    }

    #[tokio::test]
    async fn known_unready_and_unknown_features_never_gain_capabilities() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let (mut session, mut rx) = Session::new(daemon);
        hello(daemon, &mut session, &mut rx).await;
        let result = request(
            &mut session,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1,"future":1,"epics":99}}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(result["accepted"], json!({}));
        assert_eq!(result["unavailable"]["docs"], "unsupported");
        assert_eq!(result["unavailable"]["future"], "unsupported");
        assert_eq!(result["unavailable"]["epics"], "unsupported_version");
        daemon.shutdown(true).await;
    }

    #[tokio::test]
    async fn observer_initial_reply_precedes_metadata_and_snapshot_does_not_abort_it() {
        let (_temp, running) = fixture().await;
        let daemon = &running.daemon;
        let service = Arc::new(WatchedDocs {
            wake: Arc::new(Notify::new()),
        });
        daemon.feature_services.register(service.clone());
        let (mut session, mut rx) = Session::new(daemon);
        hello(daemon, &mut session, &mut rx).await;
        request(
            &mut session,
            &mut rx,
            "features_negotiate",
            json!({"versions":{"docs":1}}),
        )
        .await;
        assert!(
            session
                .request(
                    &json!({"id":2,"method":"docs_watch","params":{"selected":[]}}).to_string()
                )
                .await
        );
        let reply = rx.recv().await.unwrap();
        assert_eq!(reply.id, Some(2));
        assert!(reply.error.is_none(), "{:?}", reply.error);
        let metadata = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            metadata.push,
            Some(crew_core::rpc::Push::DocsChanged(_))
        ));
        request(&mut session, &mut rx, "snapshot", json!({})).await;
        assert_eq!(session.feature_watch_count(Feature::Docs), 1);
        // A Docs watch atomically replaces selection while retaining its bound.
        let duplicate = request(&mut session, &mut rx, "docs_watch", json!({"selected":[]})).await;
        assert!(duplicate.error.is_none());
        assert_eq!(session.feature_watch_count(Feature::Docs), 1);
        assert!(matches!(
            rx.recv().await.unwrap().push,
            Some(crew_core::rpc::Push::DocsChanged(_))
        ));
        let invalid = request(
            &mut session,
            &mut rx,
            "docs_watch",
            json!({"selected":[{"doc_id":"bad","source_id":"bad","source_generation":"0"}]}),
        )
        .await;
        assert_eq!(invalid.error.unwrap().code, "invalid");
        assert_eq!(session.feature_watch_count(Feature::Docs), 1);
        service.wake.notify_one();
        let still_live = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let Some(crew_core::rpc::Push::DocsChanged(change)) = still_live.push else {
            panic!("refused replacement stopped the old watch")
        };
        assert_eq!(change.to_revision, crew_core::rpc::features::Wide(3));
        drop(session);
        daemon.shutdown(true).await;
    }

    #[tokio::test]
    async fn cancellation_is_sticky_and_wakes_all_job_clones() {
        let cancellation = Cancellation::default();
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let token = cancellation.clone();
            jobs.push(tokio::spawn(async move { token.cancelled().await }));
        }
        tokio::task::yield_now().await;
        cancellation.cancel();
        cancellation.cancel();
        for job in jobs {
            tokio::time::timeout(Duration::from_secs(1), job)
                .await
                .expect("a job missed cancellation")
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("a later job missed sticky cancellation");
    }

    #[tokio::test]
    async fn dropping_a_job_does_not_cancel_another_connection() {
        let run = RunCtx {
            workspace_run_id: Id::new("run-1").unwrap(),
        };
        let first = ConnCtx::new(
            Id::new("connection-1").unwrap(),
            run.clone(),
            Caller::Operator,
        );
        let second = ConnCtx::new(Id::new("connection-2").unwrap(), run, Caller::Operator);
        drop(first.clone());
        assert!(!first.cancellation.is_cancelled());
        first.cancellation.cancel();
        assert!(first.cancellation.is_cancelled());
        assert!(!second.cancellation.is_cancelled());
        assert_ne!(first.connection_epoch, second.connection_epoch);
    }
}
