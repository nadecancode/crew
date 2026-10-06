//! Thin native object over the shared Rust session. No domain state lives here.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crew_client_transport::NativeSession;

use crate::callbacks::ClientObserver;
use crate::records::{
    ActionIntent, ActionPlan, ClientError, ClientNotice, ConnectionContext, TerminalChunk,
    ViewData, ViewQuery, Workspaces,
};

/// A connector-owned selection. Foreign callers choose only its opaque ID;
/// authenticated endpoint and workspace facts never come from an action view.
pub struct ProfileSelection {
    pub profile_id: String,
    pub endpoint_id: String,
    pub workspace_hint: Option<String>,
}

/// Discovery metadata for the connection picker. These facts do not establish
/// a live daemon identity or confer access to a workspace.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalProfileInfo {
    pub profile_id: String,
    pub workspace: String,
    pub project: Option<String>,
}

struct Subscription {
    active: AtomicBool,
    observer: Arc<dyn ClientObserver>,
}

impl crew_client_transport::ClientObserver for Subscription {
    fn changed(&self, notice: ClientNotice) {
        if self.active.load(Ordering::Acquire) {
            // No wrapper mutex is acquired or held across a foreign callback.
            self.observer.changed(notice);
        }
    }

    fn terminal(&self, chunk: TerminalChunk) {
        if self.active.load(Ordering::Acquire) {
            self.observer.terminal(chunk);
        }
    }
}

#[derive(Default)]
struct Registrations {
    last_id: u64,
    current: Option<(u64, Arc<Subscription>)>,
}

/// Reference-counted facade. Calls belong on the platform's worker or actor;
/// observers enqueue work on the UI executor. `close` never stops the team.
#[derive(uniffi::Object)]
pub struct CrewClient {
    session: NativeSession,
    profiles: BTreeMap<String, ProfileSelection>,
    local_profiles: Vec<LocalProfileInfo>,
    registrations: Mutex<Registrations>,
    closed: AtomicBool,
}

impl CrewClient {
    /// Rust composition only. Platform factories supply the real shared core
    /// and approved connector; the foreign interface cannot inject an engine.
    pub fn from_session(
        session: NativeSession,
        profiles: Vec<ProfileSelection>,
    ) -> Result<Arc<Self>, ClientError> {
        Self::compose(session, profiles, Vec::new())
    }

    fn compose(
        session: NativeSession,
        profiles: Vec<ProfileSelection>,
        local_profiles: Vec<LocalProfileInfo>,
    ) -> Result<Arc<Self>, ClientError> {
        if profiles.len() > 128 {
            return Err(ClientError::TooLarge {
                actual: profiles.len() as u64,
                limit: 128,
            });
        }
        let mut selections = BTreeMap::new();
        for profile in profiles {
            if profile.profile_id.is_empty()
                || profile.profile_id.len() > 128
                || profile.endpoint_id.is_empty()
                || profile.endpoint_id.len() > 128
                || profile
                    .workspace_hint
                    .as_ref()
                    .is_some_and(|id| id.is_empty() || id.len() > 4096)
            {
                return Err(ClientError::InvalidInput {
                    field: "profile".into(),
                    reason: "invalid configured profile".into(),
                });
            }
            if selections
                .insert(profile.profile_id.clone(), profile)
                .is_some()
            {
                return Err(ClientError::InvalidInput {
                    field: "profile".into(),
                    reason: "duplicate configured profile".into(),
                });
            }
        }
        Ok(Arc::new(Self {
            session,
            profiles: selections,
            local_profiles,
            registrations: Mutex::new(Registrations::default()),
            closed: AtomicBool::new(false),
        }))
    }

    fn call<T>(&self, work: impl FnOnce() -> Result<T, ClientError>) -> Result<T, ClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ClientError::ClosedClient);
        }
        boundary(work)
    }
}

#[uniffi::export]
impl CrewClient {
    /// Discover this operator's protected local profiles without connecting.
    /// Call on the platform worker: discovery can read local registry files.
    #[uniffi::constructor]
    pub fn local() -> Result<Arc<Self>, ClientError> {
        #[cfg(all(feature = "local", any(target_os = "linux", target_os = "macos")))]
        {
            boundary(|| {
                let discovered =
                    crew_client_transport::local::discover_profiles().map_err(|error| {
                        ClientError::InvalidInput {
                            field: "local".into(),
                            reason: error.reason().code,
                        }
                    })?;
                let mut infos = Vec::with_capacity(discovered.len());
                let mut profiles = Vec::with_capacity(discovered.len());
                let mut selections = Vec::with_capacity(discovered.len());
                for workspace in discovered {
                    infos.push(LocalProfileInfo {
                        profile_id: workspace.profile.id.clone(),
                        workspace: workspace.workspace,
                        project: workspace.project,
                    });
                    selections.push(ProfileSelection {
                        profile_id: workspace.profile.id.clone(),
                        endpoint_id: workspace.profile.endpoint_id.clone(),
                        // Discovery metadata must not become verified scope.
                        workspace_hint: None,
                    });
                    profiles.push(workspace.profile);
                }
                let core = crew_client::ClientCore::new(crew_client::ClientConfig {
                    limits: crew_client::policy::NATIVE_LIMITS,
                    snapshot_mode: crew_client::SnapshotMode::OrderedConnection,
                })?;
                let session = NativeSession::local(
                    core,
                    crew_client_transport::Protocol6Codec,
                    profiles,
                    crew_client_transport::DriverConfig::default(),
                )?;
                Self::compose(session, selections, infos)
            })
        }
        #[cfg(not(all(feature = "local", any(target_os = "linux", target_os = "macos"))))]
        {
            Err(ClientError::InvalidInput {
                field: "local".into(),
                reason: "local_unavailable".into(),
            })
        }
    }

    /// The discovery snapshot captured by `local()`. These opaque IDs are
    /// valid only for this client instance; reconnect still authenticates anew.
    pub fn local_profiles(&self) -> Result<Vec<LocalProfileInfo>, ClientError> {
        self.call(|| Ok(self.local_profiles.clone()))
    }

    pub fn connect(&self, profile_id: String) -> Result<(), ClientError> {
        self.call(|| {
            let selection =
                self.profiles
                    .get(&profile_id)
                    .ok_or_else(|| ClientError::InvalidInput {
                        field: "profile_id".into(),
                        reason: "unknown configured profile".into(),
                    })?;
            self.session.connect(
                selection.profile_id.clone(),
                selection.endpoint_id.clone(),
                selection.workspace_hint.clone(),
            )
        })
    }

    pub fn select_workspace(&self, workspace_id: String) -> Result<(), ClientError> {
        self.call(|| self.session.select_workspace(workspace_id))
    }

    pub fn connection_context(&self) -> Result<Option<ConnectionContext>, ClientError> {
        self.call(|| self.session.connection_context())
    }

    /// The authenticated endpoint's current listing. Before an index has been
    /// verified this is absent, never a fabricated workspace or daemon scope.
    pub fn workspaces(&self) -> Result<Option<Workspaces>, ClientError> {
        self.call(|| self.session.workspaces())
    }

    pub fn query(&self, query: ViewQuery) -> Result<ViewData, ClientError> {
        self.call(|| self.session.query(query).map(Into::into))
    }

    pub fn plan(&self, intent: ActionIntent) -> Result<ActionPlan, ClientError> {
        self.call(|| self.session.plan(intent))
    }

    pub fn confirm(&self, confirmation_id: u64) -> Result<(), ClientError> {
        self.call(|| self.session.confirm(confirmation_id))
    }

    pub fn cancel_confirmation(&self, confirmation_id: u64) -> Result<(), ClientError> {
        self.call(|| self.session.cancel_confirmation(confirmation_id))
    }

    pub fn set_active(&self, active: bool) -> Result<(), ClientError> {
        self.call(|| self.session.lifecycle(active))
    }

    pub fn reconnect(&self) -> Result<(), ClientError> {
        self.call(|| self.session.reconnect())
    }

    pub fn disconnect(&self) -> Result<(), ClientError> {
        self.call(|| self.session.disconnect())
    }

    /// Replace the current observer with a new subscription. The ID is never
    /// reused; removing an earlier subscription cannot remove its replacement.
    pub fn observe(&self, observer: Box<dyn ClientObserver>) -> Result<u64, ClientError> {
        self.call(|| {
            let subscription = Arc::new(Subscription {
                active: AtomicBool::new(true),
                observer: Arc::from(observer),
            });
            let mut registrations = self
                .registrations
                .lock()
                .map_err(|_| internal("observer_lock"))?;
            if self.closed.load(Ordering::Acquire) {
                // Drop the foreign callback only after releasing our mutex.
                drop(registrations);
                return Err(ClientError::ClosedClient);
            }
            let id = registrations
                .last_id
                .checked_add(1)
                .ok_or_else(|| internal("observer_ids_exhausted"))?;
            registrations.last_id = id;
            let previous = registrations.current.take();
            if let Some((_, previous)) = &previous {
                previous.active.store(false, Ordering::Release);
            }
            let result = self.session.observe(subscription.clone());
            if result.is_ok() {
                registrations.current = Some((id, subscription));
            } else {
                subscription.active.store(false, Ordering::Release);
            }
            // The service dispatcher owns its queue. This mutex serializes
            // registration changes only; it never protects callback execution.
            drop(registrations);
            drop(previous);
            result.map(|()| id)
        })
    }

    pub fn remove_observer(&self, subscription_id: u64) -> Result<(), ClientError> {
        boundary(|| {
            let mut registrations = self
                .registrations
                .lock()
                .map_err(|_| internal("observer_lock"))?;
            if registrations
                .current
                .as_ref()
                .is_none_or(|(id, _)| *id != subscription_id)
            {
                return Ok(());
            }
            let previous = registrations.current.take();
            if let Some((_, previous)) = &previous {
                previous.active.store(false, Ordering::Release);
            }
            let result = self.session.unregister_observer();
            drop(registrations);
            drop(previous);
            result
        })
    }

    /// Idempotently dispose channels, timers and callbacks without a shutdown
    /// command. The service never waits for an in-flight foreign callback.
    pub fn close(&self) -> Result<(), ClientError> {
        boundary(|| {
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let previous = match self.registrations.lock() {
                Ok(mut registrations) => registrations.current.take(),
                Err(poisoned) => poisoned.into_inner().current.take(),
            };
            if let Some((_, previous)) = &previous {
                previous.active.store(false, Ordering::Release);
            }
            // Retain the last foreign callback until the service releases it,
            // then drop it here outside every wrapper/driver state mutex.
            let _ = self.session.unregister_observer();
            let result = self.session.close();
            drop(previous);
            result
        })
    }
}

impl Drop for CrewClient {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn internal(code: &str) -> ClientError {
    ClientError::InternalFailure { code: code.into() }
}

fn boundary<T>(work: impl FnOnce() -> Result<T, ClientError>) -> Result<T, ClientError> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| Err(internal("native_panic")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_becomes_finite_error_without_returning_panic_payload() {
        let result: Result<(), ClientError> = boundary(|| panic!("synthetic-private-input"));
        assert_eq!(result, Err(internal("native_panic")));
    }
}
