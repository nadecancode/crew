//! GTK host adapter. Protected discovery and every service call run on a Rust
//! worker. A bounded typed channel marshals results to the GTK main context.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{mpsc, Arc},
    time::Duration,
};

use crew_client::api::*;
use crew_client::engine::ClientCore;
use crew_client_transport::{
    local::{discover_profiles, LocalWorkspace},
    ClientObserver, DriverConfig, NativeSession, Protocol6Codec,
};

pub enum Request {
    Context,
    RefreshProfiles,
    Connect {
        profile_id: String,
        endpoint_id: String,
        workspace_id: Option<String>,
    },
    SelectWorkspace(String),
    Query {
        token: u64,
        expected_scope: Option<ClientScope>,
        query: ViewQuery,
    },
    Plan {
        token: u64,
        intent: ActionIntent,
    },
    Confirm(u64),
    Cancel(u64),
    Reconnect,
    Disconnect,
    Lifecycle(bool),
    Close,
    /// Remote profile setup and sessions, delivered by `native-linux-gateway`.
    #[cfg(feature = "gateway")]
    Remote(crate::remote::RemoteRequest),
}

pub enum Response {
    Session(u64),
    Profiles(Result<Vec<LocalWorkspace>, ClientReason>),
    Notice {
        instance: u64,
        notice: ClientNotice,
    },
    Terminal {
        instance: u64,
        chunk: TerminalChunk,
    },
    View {
        instance: u64,
        token: u64,
        expected_scope: Option<ClientScope>,
        result: Result<ViewData, ClientError>,
    },
    Action {
        instance: u64,
        token: u64,
        scope: ClientScope,
        result: Result<ActionPlan, ClientError>,
    },
    Context {
        instance: u64,
        connection: Option<ConnectionContext>,
        workspaces: Option<crew_protocol::rpc::Workspaces>,
    },
    Error(ClientError),
    #[cfg(feature = "gateway")]
    Remote(crate::remote::RemoteResponse),
}

/// UI-only service interface over the real native service. A view test double
/// uses these same typed requests/results; production always starts NativeSession.
pub trait ClientService {
    fn submit(&self, request: Request) -> Result<(), ClientReason>;
}

/// Requests that may wait on the GTK side while the worker is behind.
const BACKLOG: usize = 1024;

pub struct ServiceHandle {
    requests: mpsc::SyncSender<Request>,
    /// One GTK handler can submit more requests than the worker channel holds
    /// while the worker waits for GTK to take its responses. Those requests
    /// wait here, in order, and move to the worker from the main loop.
    backlog: Rc<RefCell<VecDeque<Request>>>,
    flushing: Rc<Cell<bool>>,
    delivery: gtk::glib::JoinHandle<()>,
}

fn busy() -> ClientReason {
    ClientReason {
        code: "busy".into(),
        message: "The client is busy. Try again.".into(),
    }
}

fn closed() -> ClientReason {
    ClientReason {
        code: "closed".into(),
        message: "The client has closed.".into(),
    }
}

impl ClientService for ServiceHandle {
    fn submit(&self, request: Request) -> Result<(), ClientReason> {
        {
            let mut backlog = self.backlog.borrow_mut();
            if !backlog.is_empty() {
                if backlog.len() >= BACKLOG {
                    return Err(busy());
                }
                backlog.push_back(request);
            } else {
                match self.requests.try_send(request) {
                    Ok(()) => return Ok(()),
                    Err(mpsc::TrySendError::Disconnected(_)) => return Err(closed()),
                    Err(mpsc::TrySendError::Full(request)) => backlog.push_back(request),
                }
            }
        }
        self.flush_later();
        Ok(())
    }
}

impl ServiceHandle {
    fn flush_later(&self) {
        if self.flushing.replace(true) {
            return;
        }
        let backlog = Rc::downgrade(&self.backlog);
        let requests = self.requests.clone();
        let flushing = self.flushing.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(1), move || {
            let Some(backlog) = backlog.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            let mut backlog = backlog.borrow_mut();
            while let Some(request) = backlog.pop_front() {
                match requests.try_send(request) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(request)) => {
                        backlog.push_front(request);
                        return gtk::glib::ControlFlow::Continue;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => backlog.clear(),
                }
            }
            flushing.set(false);
            gtk::glib::ControlFlow::Break
        });
    }

    /// Requests waiting on the GTK side.
    pub fn backlog(&self) -> usize {
        self.backlog.borrow().len()
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        self.delivery.abort();
        let _ = self.requests.try_send(Request::Close);
    }
}

struct Observer {
    responses: tokio::sync::mpsc::Sender<Response>,
    instance: u64,
}
impl ClientObserver for Observer {
    fn changed(&self, notice: ClientNotice) {
        let _ = self.responses.blocking_send(Response::Notice {
            instance: self.instance,
            notice,
        });
    }
    fn terminal(&self, chunk: TerminalChunk) {
        let _ = self.responses.blocking_send(Response::Terminal {
            instance: self.instance,
            chunk,
        });
    }
}

impl ServiceHandle {
    pub fn start(handler: impl Fn(Response) + 'static) -> Self {
        let (requests, receiver) = mpsc::sync_channel(16);
        // At most sixteen <=65,536-byte terminal chunks can wait for GTK. Query
        // payloads retain the service's own bounded-size check.
        let (responses, mut results) = tokio::sync::mpsc::channel(16);
        let delivery = gtk::glib::MainContext::default().spawn_local(async move {
            while let Some(response) = results.recv().await {
                handler(response);
            }
        });
        std::thread::Builder::new()
            .name("crew-gtk-service".into())
            .spawn(move || {
                worker(receiver, responses);
            })
            .expect("native service worker");
        Self {
            requests,
            backlog: Rc::new(RefCell::new(VecDeque::new())),
            flushing: Rc::new(Cell::new(false)),
            delivery,
        }
    }
}

pub(crate) fn core() -> Result<ClientCore, ClientError> {
    ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
}

fn make_session(profiles: &[LocalWorkspace]) -> Result<NativeSession, ClientError> {
    core().and_then(|core| {
        NativeSession::local(
            core,
            Protocol6Codec,
            profiles
                .iter()
                .map(|workspace| workspace.profile.clone())
                .collect(),
            DriverConfig::default(),
        )
    })
}

/// Replace the worker's session. A new instance tag rejects every late
/// callback, query and plan result from the previous session.
fn bind(
    session: &mut NativeSession,
    next: NativeSession,
    instance: &mut u64,
    responses: &tokio::sync::mpsc::Sender<Response>,
) -> Result<(), ClientError> {
    let _ = session.unregister_observer();
    let _ = session.close();
    *session = next;
    *instance = instance
        .checked_add(1)
        .expect("native session instance range");
    let _ = responses.blocking_send(Response::Session(*instance));
    session.observe(Arc::new(Observer {
        responses: responses.clone(),
        instance: *instance,
    }))
}

#[cfg(feature = "gateway")]
type RemoteWorker = Option<crate::remote::Worker>;
#[cfg(not(feature = "gateway"))]
type RemoteWorker = ();

#[cfg(feature = "gateway")]
fn is_remote(request: &Request) -> bool {
    matches!(request, Request::Remote(_))
}
#[cfg(not(feature = "gateway"))]
fn is_remote(_: &Request) -> bool {
    false
}

#[cfg(feature = "gateway")]
fn remote_active(remote: &RemoteWorker) -> bool {
    remote
        .as_ref()
        .is_some_and(|worker| worker.active().is_some())
}
#[cfg(not(feature = "gateway"))]
fn remote_active(_: &RemoteWorker) -> bool {
    false
}

/// Return from a bound remote profile to the protected local service.
#[cfg(feature = "gateway")]
fn leave_remote(
    remote: &mut RemoteWorker,
    profiles: &[LocalWorkspace],
    session: &mut NativeSession,
    instance: &mut u64,
    responses: &tokio::sync::mpsc::Sender<Response>,
) -> Result<(), ClientError> {
    let Some(worker) = remote.as_mut().filter(|worker| worker.active().is_some()) else {
        return Ok(());
    };
    worker.left();
    make_session(profiles).and_then(|next| bind(session, next, instance, responses))?;
    let _ = responses.blocking_send(Response::Remote(crate::remote::RemoteResponse::Active(
        None,
    )));
    Ok(())
}

fn worker(requests: mpsc::Receiver<Request>, responses: tokio::sync::mpsc::Sender<Response>) {
    let discover = || discover_profiles().map_err(|failure| failure.reason());
    let mut profiles = match discover() {
        Ok(profiles) => {
            let _ = responses.blocking_send(Response::Profiles(Ok(profiles.clone())));
            profiles
        }
        Err(reason) => {
            let _ = responses.blocking_send(Response::Profiles(Err(reason)));
            Vec::new()
        }
    };
    let mut session = match make_session(&profiles) {
        Ok(session) => session,
        Err(error) => {
            let _ = responses.blocking_send(Response::Error(error));
            return;
        }
    };
    let mut instance = 1u64;
    let _ = responses.blocking_send(Response::Session(instance));
    if let Err(error) = session.observe(Arc::new(Observer {
        responses: responses.clone(),
        instance,
    })) {
        let _ = responses.blocking_send(Response::Error(error));
        return;
    }
    let mut connection_requested = false;
    #[cfg(feature = "gateway")]
    let mut remote: RemoteWorker = crate::remote::Worker::new();
    #[cfg(not(feature = "gateway"))]
    let remote: RemoteWorker = ();
    while let Ok(request) = requests.recv() {
        if responses.is_closed() {
            break;
        }
        let update_context = matches!(
            &request,
            Request::Context
                | Request::RefreshProfiles
                | Request::Connect { .. }
                | Request::SelectWorkspace(_)
                | Request::Reconnect
                | Request::Disconnect
        ) || is_remote(&request);
        let result = match request {
            Request::Context => Ok(()),
            Request::RefreshProfiles => {
                match discover() {
                    Ok(next) => {
                        profiles = next;
                        let _ = responses.blocking_send(Response::Profiles(Ok(profiles.clone())));
                        // Rebuild only while explicitly disconnected; discovery
                        // must not silently rebind a verified active workspace.
                        if !connection_requested && !remote_active(&remote) {
                            make_session(&profiles).and_then(|next| {
                                bind(&mut session, next, &mut instance, &responses)
                            })
                        } else {
                            Ok(())
                        }
                    }
                    Err(reason) => {
                        let _ = responses.blocking_send(Response::Profiles(Err(reason)));
                        Ok(())
                    }
                }
            }
            Request::Connect {
                profile_id,
                endpoint_id,
                workspace_id,
            } => {
                // A local profile replaces any bound remote profile first.
                #[cfg(feature = "gateway")]
                let result = leave_remote(
                    &mut remote,
                    &profiles,
                    &mut session,
                    &mut instance,
                    &responses,
                )
                .and_then(|()| session.connect(profile_id, endpoint_id, workspace_id));
                #[cfg(not(feature = "gateway"))]
                let result = session.connect(profile_id, endpoint_id, workspace_id);
                if result.is_ok() {
                    connection_requested = true;
                }
                result
            }
            Request::SelectWorkspace(id) => session.select_workspace(id),
            Request::Query {
                token,
                expected_scope,
                query,
            } => {
                let result = session.query(query);
                let _ = responses.blocking_send(Response::View {
                    instance,
                    token,
                    expected_scope,
                    result,
                });
                Ok(())
            }
            Request::Plan { token, intent } => {
                let scope = crew_client::actions::action_scope(&intent).clone();
                let result = session.plan(intent);
                let _ = responses.blocking_send(Response::Action {
                    instance,
                    token,
                    scope,
                    result,
                });
                Ok(())
            }
            Request::Confirm(id) => session.confirm(id),
            Request::Cancel(id) => session.cancel_confirmation(id),
            Request::Reconnect => session.reconnect(),
            Request::Disconnect => {
                let result = session.disconnect();
                if result.is_ok() {
                    connection_requested = false;
                }
                result
            }
            Request::Lifecycle(active) => session.lifecycle(active),
            Request::Close => break,
            #[cfg(feature = "gateway")]
            Request::Remote(request) => {
                use crate::remote::{RemoteResponse, Step};
                match remote
                    .as_mut()
                    .map(|worker| worker.handle(request, &responses))
                {
                    None => Err(ClientError::InternalFailure {
                        code: "remote_worker".into(),
                    }),
                    Some(Step::Done) => Ok(()),
                    Some(Step::Bind {
                        profile,
                        session: next,
                    }) => bind(&mut session, next, &mut instance, &responses).and_then(|()| {
                        connection_requested = true;
                        let _ = responses.blocking_send(Response::Remote(RemoteResponse::Active(
                            Some(profile.id.clone()),
                        )));
                        remote
                            .as_mut()
                            .map_or(Ok(()), |worker| worker.bound(&profile, &session))
                    }),
                    Some(Step::Local) => {
                        let result = leave_remote(
                            &mut remote,
                            &profiles,
                            &mut session,
                            &mut instance,
                            &responses,
                        );
                        if result.is_ok() {
                            connection_requested = false;
                        }
                        result
                    }
                }
            }
        };
        if let Err(error) = result {
            let _ = responses.blocking_send(Response::Error(error));
        }
        if update_context {
            match (session.connection_context(), session.workspaces()) {
                (Ok(connection), Ok(workspaces)) => {
                    if responses
                        .blocking_send(Response::Context {
                            instance,
                            connection,
                            workspaces,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    let _ = responses.blocking_send(Response::Error(error));
                }
            }
        }
    }
    let _ = session.unregister_observer();
    let _ = session.close();
}
