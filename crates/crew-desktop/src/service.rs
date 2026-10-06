//! GTK host adapter. Protected discovery and every service call run on a Rust
//! worker. A bounded typed channel marshals results to the GTK main context.

use std::sync::{mpsc, Arc};

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
}

/// UI-only service interface over the real native service. A view test double
/// uses these same typed requests/results; production always starts NativeSession.
pub trait ClientService {
    fn submit(&self, request: Request) -> Result<(), ClientReason>;
}

pub struct ServiceHandle {
    requests: mpsc::SyncSender<Request>,
    delivery: gtk::glib::JoinHandle<()>,
}

impl ClientService for ServiceHandle {
    fn submit(&self, request: Request) -> Result<(), ClientReason> {
        self.requests
            .try_send(request)
            .map_err(|failure| ClientReason {
                code: if matches!(failure, mpsc::TrySendError::Full(_)) {
                    "busy"
                } else {
                    "closed"
                }
                .into(),
                message: if matches!(failure, mpsc::TrySendError::Full(_)) {
                    "The client is busy. Try again."
                } else {
                    "The client has closed."
                }
                .into(),
            })
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
        Self { requests, delivery }
    }
}

fn core() -> Result<ClientCore, ClientError> {
    ClientCore::new(ClientConfig {
        limits: crew_client::policy::NATIVE_LIMITS,
        snapshot_mode: SnapshotMode::OrderedConnection,
    })
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
    let make_session = |profiles: &[LocalWorkspace]| {
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
        );
        let result = match request {
            Request::Context => Ok(()),
            Request::RefreshProfiles => {
                match discover() {
                    Ok(next) => {
                        profiles = next;
                        let _ = responses.blocking_send(Response::Profiles(Ok(profiles.clone())));
                        // Rebuild only while explicitly disconnected; discovery
                        // must not silently rebind a verified active workspace.
                        if !connection_requested {
                            let _ = session.unregister_observer();
                            let _ = session.close();
                            match make_session(&profiles) {
                                Ok(next) => {
                                    session = next;
                                    instance = instance
                                        .checked_add(1)
                                        .expect("native session instance range");
                                    let _ = responses.blocking_send(Response::Session(instance));
                                    session.observe(Arc::new(Observer {
                                        responses: responses.clone(),
                                        instance,
                                    }))
                                }
                                Err(error) => Err(error),
                            }
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
                let scope = intent_scope(&intent).clone();
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

fn intent_scope(intent: &ActionIntent) -> &ClientScope {
    match intent {
        ActionIntent::SendMessage { scope, .. }
        | ActionIntent::Answer { scope, .. }
        | ActionIntent::Agent { scope, .. }
        | ActionIntent::SetModel { scope, .. }
        | ActionIntent::AgentInput { scope, .. }
        | ActionIntent::ResumeLimited { scope, .. }
        | ActionIntent::StopTeam { scope }
        | ActionIntent::Read { scope, .. }
        | ActionIntent::Attach { scope, .. }
        | ActionIntent::AttachInput { scope, .. }
        | ActionIntent::AttachResize { scope, .. }
        | ActionIntent::Detach { scope, .. } => scope,
    }
}
