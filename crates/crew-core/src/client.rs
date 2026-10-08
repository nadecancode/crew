//! Blocking client for the daemon socket.
//!
//! Everything outside the daemon is synchronous: short-lived CLI commands, the
//! MCP bridge (one request at a time over stdio), hooks, and the dashboard,
//! which reads pushes on a dedicated thread.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::paths::Identity;
use crate::rpc::{
    Call, Hello, HelloInfo, Push, Request, Role, RpcError, ServerFrame, PROTOCOL_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot reach the crew daemon at {path}: {source}")]
    Connect { path: String, source: io::Error },
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("connection to the crew daemon failed: {0}")]
    Io(#[from] io::Error),
    #[error("the crew daemon did not answer in time")]
    Timeout,
    #[error("the crew daemon closed the connection")]
    Closed,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Mismatch(Box<Mismatch>),
    #[error("cannot read operator token at {path}: {message}")]
    OperatorToken { path: String, message: String },
    #[error("no operator token at {path}: operator commands must run as the user who started the daemon, outside agent sandboxes")]
    MissingOperatorToken { path: String },
    #[error("this process runs as agent '{agent_id}' (CREW_IDENTITY is set): operator commands are not available to agents; use your crew tools or crew ctl")]
    AgentContext { agent_id: String },
}

/// Optional discovery on an already authenticated connection. An unavailable
/// result permits legacy reads; it never permits replaying a scoped mutation
/// as an unscoped one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeatureDiscovery {
    Supported(crate::rpc::features::FeatureSession),
    Unsupported,
    Unavailable { code: String },
}

impl ClientError {
    /// The stream is unavailable or unsafe to continue. This does not make
    /// replaying a mutation safe: a request may have run before a read failed.
    /// A response decode mismatch means the reply, and thus the executed request, arrived.
    pub fn is_disconnect(&self) -> bool {
        matches!(
            self,
            Self::Connect { .. } | Self::Io(_) | Self::Timeout | Self::Closed | Self::Protocol(_)
        )
    }
    pub fn is_protocol_mismatch(&self) -> bool {
        matches!(self, Self::Mismatch(_))
    }
    /// An operator credential failure needs a visible error, not a reconnect loop.
    pub fn is_operator_auth_error(&self) -> bool {
        matches!(
            self,
            Self::OperatorToken { .. }
                | Self::MissingOperatorToken { .. }
                | Self::AgentContext { .. }
        ) || matches!(self, Self::Rpc(error) if error.code == "unauthorized")
    }
}

/// This build of crew and a daemon of another protocol: what is known of
/// each side, said as one sentence that names both and what to do
/// (notes/binary-pinning.md B8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub client_version: String,
    pub client_protocol: u32,
    /// Absent from a daemon older than this sentence.
    pub daemon_version: Option<String>,
    /// Absent when the daemon's refusal did not say it in a known form.
    pub daemon_protocol: Option<u32>,
    pub workspace: Option<String>,
    /// Named when the workspace is not known.
    pub socket: String,
    /// The process that holds the daemon's socket, as the kernel says (not
    /// as the daemon does): what `crew stop` signals, since a daemon of
    /// another protocol cannot be asked.
    pub daemon_pid: Option<u32>,
}

impl Mismatch {
    /// From a daemon's answer to `hello`: its refusal (`code` `protocol`,
    /// or the generic `invalid` of a protocol 1 daemon), or a `HelloInfo`
    /// of another protocol. `None` for any other answer.
    fn from_hello(result: &ClientResult<HelloInfo>, socket: &Path) -> Option<Self> {
        let (daemon_version, daemon_protocol, workspace) = match result {
            Err(ClientError::Rpc(error))
                if error.code == "protocol"
                    || (error.code == "invalid"
                        && error.message.starts_with("client speaks protocol ")) =>
            {
                let data = error.data.as_ref();
                let field = |name: &str| data.and_then(|data| data.get(name));
                (
                    field("version").and_then(Value::as_str).map(str::to_string),
                    field("protocol")
                        .and_then(Value::as_u64)
                        .and_then(|p| u32::try_from(p).ok())
                        .or_else(|| refused_protocol(&error.message)),
                    field("workspace")
                        .and_then(Value::as_str)
                        .filter(|w| !w.is_empty())
                        .map(str::to_string),
                )
            }
            Ok(info) if info.protocol != PROTOCOL_VERSION => {
                (Some(info.version.clone()), Some(info.protocol), None)
            }
            _ => return None,
        };
        Some(Self {
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_protocol: PROTOCOL_VERSION,
            daemon_version,
            daemon_protocol,
            workspace: workspace.or_else(|| registered_workspace(socket)),
            socket: socket.display().to_string(),
            daemon_pid: None,
        })
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "crew {} (protocol {}) cannot talk to the daemon ",
            self.client_version, self.client_protocol
        )?;
        match &self.workspace {
            Some(workspace) => write!(f, "of {workspace}")?,
            None => write!(f, "at {}", self.socket)?,
        }
        match (&self.daemon_version, self.daemon_protocol) {
            (Some(version), Some(protocol)) => {
                write!(f, ", which is crew {version} (protocol {protocol})")?
            }
            (Some(version), None) => write!(f, ", which is crew {version}")?,
            (None, Some(protocol)) => write!(f, ", which speaks protocol {protocol}")?,
            (None, None) => write!(f, ", which speaks another protocol")?,
        }
        write!(f, ": restart the team (`crew stop`, then `crew run`)")
    }
}

/// The daemon's protocol from the refusal every daemon so far has sent:
/// "client speaks protocol N, daemon speaks M".
fn refused_protocol(message: &str) -> Option<u32> {
    let (_, rest) = message.split_once("daemon speaks ")?;
    let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

/// The workspace of the daemon registered with this socket, for a daemon
/// that does not say it. Read only: stale entries are left to
/// `list_sessions`.
fn registered_workspace(socket: &Path) -> Option<String> {
    workspace_in_registry(&crate::paths::registry_dir(), socket)
}

fn workspace_in_registry(registry: &Path, socket: &Path) -> Option<String> {
    let entries = std::fs::read_dir(registry).ok()?;
    entries.flatten().find_map(|entry| {
        let text = std::fs::read_to_string(entry.path()).ok()?;
        let info: crate::paths::DaemonInfo = serde_json::from_str(&text).ok()?;
        (Path::new(&info.socket) == socket && !info.workspace.is_empty()).then_some(info.workspace)
    })
}

fn check_hello(result: ClientResult<HelloInfo>, socket: &Path) -> ClientResult<HelloInfo> {
    match Mismatch::from_hello(&result, socket) {
        Some(mismatch) => Err(ClientError::Mismatch(Box::new(mismatch))),
        None => result,
    }
}

/// The pid of the process at the other end of `stream`, from the kernel.
#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the length given.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (rc == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

pub type ClientResult<T> = Result<T, ClientError>;

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Pushes that arrived while waiting for a response.
    pushes: VecDeque<Push>,
    closed: bool,
}

/// A close-only handle for a host that must interrupt a blocking reader on
/// shutdown. It exposes no socket IO or request allocator. Closing does not
/// establish whether a request ran, so hosts must retain their outcome policy.
#[derive(Clone)]
pub struct ShutdownHandle {
    stream: Arc<UnixStream>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Operator commands are not available to an agent: refuse when this process
/// runs as one (`CREW_IDENTITY` is set). Commands that start a daemon call
/// this before they start it.
pub fn refuse_in_agent_context() -> ClientResult<()> {
    if std::env::var_os(crate::paths::IDENTITY_ENV).is_none() {
        return Ok(());
    }
    // Presence decides authority; loading only improves the safe agent label.
    let agent_id = Identity::from_env()
        .ok()
        .flatten()
        .map(|identity| identity.agent_id)
        .filter(|id| crate::util::is_valid_id(id))
        .unwrap_or_else(|| "unknown".into());
    Err(ClientError::AgentContext { agent_id })
}

impl Client {
    pub fn connect(socket: &Path) -> ClientResult<Self> {
        let stream = UnixStream::connect(socket).map_err(|source| ClientError::Connect {
            path: socket.display().to_string(),
            source,
        })?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            pushes: VecDeque::new(),
            closed: false,
        })
    }

    /// Connect and authenticate as the human operator.
    pub fn operator(socket: &Path, client: &str) -> ClientResult<(Self, HelloInfo)> {
        Self::operator_connected(socket, client, |_| Ok(()))
    }

    /// Register cancellation immediately after connection, before the first
    /// hello write. Token reads and a connect still in progress are outside
    /// this handle's lifetime. Existing operator authentication is unchanged.
    pub fn operator_with_control(
        socket: &Path,
        client: &str,
        connected: impl FnOnce(ShutdownHandle),
    ) -> ClientResult<(Self, HelloInfo)> {
        Self::operator_connected(socket, client, |connection| {
            connected(connection.shutdown_handle()?);
            Ok(())
        })
    }

    fn operator_connected(
        socket: &Path,
        client: &str,
        connected: impl FnOnce(&Self) -> ClientResult<()>,
    ) -> ClientResult<(Self, HelloInfo)> {
        refuse_in_agent_context()?;
        let token = crate::paths::read_operator_token(socket).map_err(|error| {
            ClientError::OperatorToken {
                path: crate::paths::operator_token_path(socket)
                    .display()
                    .to_string(),
                message: format!("{error:#}"),
            }
        })?;
        let missing_token = token.is_none();
        let mut c = Self::connect(socket)?;
        connected(&c)?;
        // Negotiate even without credentials so old daemons keep H8's
        // actionable upgrade error. A current daemon still cannot grant us
        // operator access without the token.
        let result = check_hello(
            c.call(Call::Hello(Hello {
                role: Role::Operator,
                agent_id: None,
                token,
                client: client.to_string(),
                protocol: PROTOCOL_VERSION,
                web: false,
                workspace: None,
            })),
            socket,
        )
        .map_err(|error| c.with_peer(error));
        let missing = || ClientError::MissingOperatorToken {
            path: crate::paths::operator_token_path(socket)
                .display()
                .to_string(),
        };
        let info = match result {
            Err(ClientError::Rpc(error)) if missing_token && error.code == "unauthorized" => {
                return Err(missing());
            }
            result => result?,
        };
        // A peer claiming our version must not let a credential-less client
        // escape either, even if that peer incorrectly accepts the Hello.
        if missing_token {
            return Err(missing());
        }
        Ok((c, info))
    }

    /// Connect and authenticate as an agent.
    pub fn agent(identity: &Identity, client: &str) -> ClientResult<(Self, HelloInfo)> {
        let socket = Path::new(&identity.socket);
        let mut c = Self::connect(socket)?;
        let info = check_hello(
            c.call(Call::Hello(Hello {
                role: Role::Agent,
                agent_id: Some(identity.agent_id.clone()),
                token: Some(identity.token.clone()),
                client: client.to_string(),
                protocol: PROTOCOL_VERSION,
                web: false,
                workspace: None,
            })),
            socket,
        )
        .map_err(|error| c.with_peer(error))?;
        Ok((c, info))
    }

    /// A mismatch, with the pid of the process that answered.
    fn with_peer(&self, error: ClientError) -> ClientError {
        match error {
            ClientError::Mismatch(mut mismatch) => {
                mismatch.daemon_pid = peer_pid(&self.writer);
                ClientError::Mismatch(mismatch)
            }
            error => error,
        }
    }

    /// Clone a close-only cancellation handle; dropping it leaves the client
    /// open. Call `shutdown` to interrupt a blocked call from another thread.
    pub fn shutdown_handle(&self) -> ClientResult<ShutdownHandle> {
        Ok(ShutdownHandle {
            stream: Arc::new(self.writer.try_clone()?),
        })
    }

    /// Bound how long a call may wait for the daemon (`None` waits forever).
    pub fn set_timeout(&self, timeout: Option<Duration>) -> ClientResult<()> {
        if self.closed {
            return Err(ClientError::Closed);
        }
        self.reader.get_ref().set_read_timeout(timeout)?;
        self.writer.set_write_timeout(timeout)?;
        Ok(())
    }

    /// Write a request without waiting for its response.
    pub fn send(&mut self, call: Call) -> ClientResult<u64> {
        if self.closed {
            return Err(ClientError::Closed);
        }
        let id = self.next_id;
        self.next_id += 1;
        if let Err(error) = write_request(&mut self.writer, id, call) {
            self.poison();
            return Err(error);
        }
        Ok(id)
    }

    /// Read buffered pushes first, then block until the next socket frame arrives.
    pub fn next_frame(&mut self) -> ClientResult<ServerFrame> {
        if self.closed {
            return Err(ClientError::Closed);
        }
        if let Some(push) = self.pushes.pop_front() {
            return Ok(ServerFrame::push(push));
        }
        self.read_socket()
    }

    fn poison(&mut self) {
        self.closed = true;
        self.pushes.clear();
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }

    fn read_socket(&mut self) -> ClientResult<ServerFrame> {
        if self.closed {
            return Err(ClientError::Closed);
        }
        let result = read_frame(&mut self.reader);
        if result.is_err() {
            self.poison();
        }
        result
    }

    pub fn call_value(&mut self, call: Call) -> ClientResult<Value> {
        let id = self.send(call)?;
        loop {
            // Only read the socket here: queued pushes belong to the caller,
            // and draining/requeueing them would never reach this reply.
            let frame = self.read_socket()?;
            if let Some(push) = frame.push {
                self.pushes.push_back(push);
                continue;
            }
            if frame.id != Some(id) {
                continue;
            }
            if let Some(error) = frame.error {
                return Err(ClientError::Rpc(error));
            }
            return Ok(frame.result.unwrap_or(Value::Null));
        }
    }

    pub fn call<T: DeserializeOwned>(&mut self, call: Call) -> ClientResult<T> {
        let method = call.method();
        let value = self.call_value(call)?;
        serde_json::from_value(value).map_err(|e| {
            self.poison();
            ClientError::Protocol(format!("unexpected `{method}` response: {e}"))
        })
    }

    /// Discover feature support before Snapshot/Subscribe or a feature call.
    /// Actual old v6 decoders/intermediaries use invalid/forbidden for an
    /// unknown negotiation method. Authentication and transport errors retain
    /// their normal recovery path; no operation is retried by this helper.
    pub fn negotiate_features(
        &mut self,
        request: crate::rpc::features::FeaturesNegotiate,
    ) -> ClientResult<FeatureDiscovery> {
        let requested = request.versions.clone();
        match self.call_value(Call::FeaturesNegotiate(request)) {
            Ok(value) => {
                let session: crate::rpc::features::FeatureSession = serde_json::from_value(value)
                    .map_err(|_| {
                    self.poison();
                    ClientError::Protocol("invalid feature negotiation response".into())
                })?;
                let valid = session.accepted.iter().all(|(name, version)| {
                    requested.get(name) == Some(version)
                        && *version == crate::rpc::features::FEATURE_VERSION
                        && crate::rpc::features::Feature::ALL
                            .iter()
                            .any(|feature| feature.name() == name)
                }) && session
                    .unavailable
                    .keys()
                    .all(|name| requested.contains_key(name))
                    && session
                        .accepted
                        .keys()
                        .all(|name| !session.unavailable.contains_key(name))
                    && requested.keys().all(|name| {
                        session.accepted.contains_key(name)
                            || session.unavailable.contains_key(name)
                    });
                if !valid {
                    self.poison();
                    return Err(ClientError::Protocol(
                        "feature negotiation changed requested versions".into(),
                    ));
                }
                Ok(FeatureDiscovery::Supported(session))
            }
            Err(ClientError::Rpc(error))
                if matches!(
                    error.code.as_str(),
                    "invalid" | "forbidden" | "unsupported_feature"
                ) =>
            {
                Ok(FeatureDiscovery::Unsupported)
            }
            Err(ClientError::Rpc(error))
                if !matches!(error.code.as_str(), "unauthorized" | "protocol") =>
            {
                Ok(FeatureDiscovery::Unavailable { code: error.code })
            }
            Err(error) => Err(error),
        }
    }

    /// Subscribe and turn the connection into a stream of pushes.
    pub fn subscribe(mut self) -> ClientResult<PushStream> {
        self.call_value(Call::Subscribe)?;
        Ok(PushStream {
            reader: self.reader,
            buffered: self.pushes,
        })
    }

    /// Split into independent halves (used by the raw terminal attachment,
    /// which reads output and writes input concurrently).
    pub fn split(self) -> (FrameReader, FrameWriter) {
        (
            FrameReader {
                reader: self.reader,
                buffered: self.pushes,
            },
            FrameWriter {
                writer: self.writer,
                next_id: self.next_id,
            },
        )
    }
}

pub struct PushStream {
    reader: BufReader<UnixStream>,
    buffered: VecDeque<Push>,
}

impl PushStream {
    /// Block until the next push. Returns `Err(Closed)` when the daemon goes away.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> ClientResult<Push> {
        loop {
            if let Some(push) = self.buffered.pop_front() {
                if push != Push::Unknown {
                    return Ok(push);
                }
                continue;
            }
            let frame = read_frame(&mut self.reader)?;
            if let Some(push) = frame.push {
                if push != Push::Unknown {
                    return Ok(push);
                }
            }
        }
    }
}

pub struct FrameReader {
    reader: BufReader<UnixStream>,
    buffered: VecDeque<Push>,
}

impl FrameReader {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> ClientResult<ServerFrame> {
        if let Some(push) = self.buffered.pop_front() {
            return Ok(ServerFrame::push(push));
        }
        read_frame(&mut self.reader)
    }
}

pub struct FrameWriter {
    writer: UnixStream,
    next_id: u64,
}

impl FrameWriter {
    pub fn send(&mut self, call: Call) -> ClientResult<u64> {
        let id = self.next_id;
        self.next_id += 1;
        write_request(&mut self.writer, id, call)?;
        Ok(id)
    }

    /// Close the connection, which also unblocks the reading half.
    pub fn shutdown(&self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

fn write_request(writer: &mut UnixStream, id: u64, call: Call) -> ClientResult<()> {
    let mut line = serde_json::to_vec(&Request { id, call })
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> ClientResult<ServerFrame> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            // A read that ran into the connection's timeout reports
            // "resource temporarily unavailable", which says nothing.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(ClientError::Timeout);
            }
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            return Err(ClientError::Closed);
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(&line).map_err(|e| ClientError::Protocol(e.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn negotiation_request() -> crate::rpc::features::FeaturesNegotiate {
        serde_json::from_value(json!({"versions":{"docs":1}})).unwrap()
    }

    fn negotiation_peer(
        mut server: UnixStream,
        response: ServerFrame,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            assert!(matches!(request.call, Call::FeaturesNegotiate(_)));
            let mut response = response;
            response.id = Some(request.id);
            send_frame(&mut server, response);
        })
    }

    #[test]
    fn feature_discovery_classifies_old_peer_refusals_without_replaying_intent() {
        for code in [
            "invalid",
            "forbidden",
            "unsupported_feature",
            "invalid_feature_request",
            "unauthorized",
        ] {
            let (mut client, server) = paired_client();
            let peer = negotiation_peer(
                server,
                ServerFrame::err(1, RpcError::new(code, "synthetic refusal")),
            );
            let result = client.negotiate_features(negotiation_request());
            match code {
                "unauthorized" => {
                    assert!(matches!(result, Err(ClientError::Rpc(error)) if error.code == code))
                }
                "invalid_feature_request" => assert!(
                    matches!(result, Ok(FeatureDiscovery::Unavailable {code: reason}) if reason == code)
                ),
                _ => assert_eq!(result.unwrap(), FeatureDiscovery::Unsupported),
            }
            assert_eq!(client.next_id, 2, "discovery sent an extra request");
            peer.join().unwrap();
        }
    }

    #[test]
    fn feature_discovery_rejects_added_versions_and_poisoned_decoding() {
        for accepted in [json!({"docs":2}), json!({"docs":1,"epics":1})] {
            let (mut client, server) = paired_client();
            let peer = negotiation_peer(
                server,
                ServerFrame::ok(
                    1,
                    json!({
                        "accepted":accepted,"connection_epoch":"connection-1","workspace_run_id":"run-1","unavailable":{}
                    }),
                ),
            );
            assert!(matches!(
                client.negotiate_features(negotiation_request()),
                Err(ClientError::Protocol(_))
            ));
            assert!(client.closed);
            peer.join().unwrap();
        }
    }

    #[test]
    fn feature_discovery_preserves_valid_connection_epoch_and_transport_failures() {
        let (mut client, server) = paired_client();
        let peer = negotiation_peer(
            server,
            ServerFrame::ok(
                1,
                json!({
                    "accepted":{"docs":1},"connection_epoch":"exact-connection","workspace_run_id":"run-1","unavailable":{}
                }),
            ),
        );
        let FeatureDiscovery::Supported(session) =
            client.negotiate_features(negotiation_request()).unwrap()
        else {
            panic!("feature support lost")
        };
        assert_eq!(session.connection_epoch.as_str(), "exact-connection");
        peer.join().unwrap();
        let (mut client, server) = paired_client();
        drop(server);
        assert!(client
            .negotiate_features(negotiation_request())
            .unwrap_err()
            .is_disconnect());
    }

    #[test]
    fn missing_token_preserves_old_daemon_protocol_diagnostics() {
        if crate::test_support::isolate_operator_test(
            "client::tests::missing_token_preserves_old_daemon_protocol_diagnostics",
        ) {
            return;
        }
        for response in [
            ServerFrame::err(
                0,
                RpcError::new("protocol", "client speaks protocol 3, daemon speaks 2"),
            ),
            ServerFrame::err(
                0,
                RpcError::invalid("client speaks protocol 3, daemon speaks 1"),
            ),
            ServerFrame::ok(
                0,
                json!({"role":"operator", "version":"old", "protocol":2, "pid":1}),
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("daemon.sock");
            let peer = hello_peer(&socket, response);
            let error = Client::operator(&socket, "test")
                .err()
                .expect("old daemon must fail");
            let hello = peer.join().unwrap();
            assert!(error.is_protocol_mismatch(), "{error}");
            let ClientError::Mismatch(mismatch) = &error else {
                unreachable!()
            };
            // The peer runs in this process: that is the pid `crew stop` signals.
            if cfg!(target_os = "linux") {
                assert_eq!(mismatch.daemon_pid, Some(std::process::id()));
            }
            let text = error.to_string();
            assert!(
                text.contains(&format!(
                    "cannot talk to the daemon at {}",
                    socket.display()
                )),
                "{text}"
            );
            assert!(
                text.ends_with(": restart the team (`crew stop`, then `crew run`)"),
                "{text}"
            );
            assert_eq!(
                hello
                    .expect("a tokenless Hello must negotiate the version")
                    .1
                    .token,
                None
            );
        }
    }

    fn hello_peer(
        socket: &Path,
        mut response: ServerFrame,
    ) -> std::thread::JoinHandle<Option<(String, Hello)>> {
        use std::os::unix::net::UnixListener;
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(200);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(stream.try_clone().unwrap())
                            .read_line(&mut line)
                            .unwrap();
                        let request: Request = serde_json::from_str(&line).unwrap();
                        let Call::Hello(hello) = request.call else {
                            panic!("expected Hello")
                        };
                        response.id = Some(request.id);
                        send_frame(&mut stream, response);
                        return Some((line, hello));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
            None
        })
    }

    #[test]
    fn operator_client_refuses_under_crew_identity() {
        const PROBE: &str = "CREW_OPERATOR_GUARD_PROBE";
        if let Some(socket) = std::env::var_os(PROBE) {
            let error = Client::operator(Path::new(&socket), "test")
                .err()
                .expect("operator client must refuse an agent environment");
            let message = error.to_string();
            assert!(message.contains("CREW_IDENTITY is set"), "{message}");
            assert!(
                message.contains("operator commands are not available to agents"),
                "{message}"
            );
            assert!(
                message.contains("use your crew tools or crew ctl"),
                "{message}"
            );
            assert!(!message.contains("identity-secret-do-not-print"));
            return;
        }
        if crate::test_support::isolate_operator_test(
            "client::tests::operator_client_refuses_under_crew_identity",
        ) {
            return;
        }
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        crate::paths::write_private_file(
            &crate::paths::operator_token_path(&socket),
            b"operator-secret",
        )
        .unwrap();
        let identity = temp.path().join("identity.json");
        crate::paths::write_private_file(
            &identity,
            &serde_json::to_vec(&Identity {
                socket: socket.to_string_lossy().into_owned(),
                agent_id: "worker".into(),
                token: "identity-secret-do-not-print".into(),
                orchestrator: false,
            })
            .unwrap(),
        )
        .unwrap();
        for identity in [
            identity.into_os_string(),
            temp.path().join("missing.json").into_os_string(),
            std::ffi::OsString::new(),
        ] {
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let peer = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            stream
                                .set_read_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            let mut line = String::new();
                            BufReader::new(stream.try_clone().unwrap())
                                .read_line(&mut line)
                                .unwrap();
                            let request: Request = serde_json::from_str(&line).unwrap();
                            send_frame(
                                &mut stream,
                                ServerFrame::ok(
                                    request.id,
                                    json!({"role":"operator", "version":"test", "protocol":PROTOCOL_VERSION, "pid":1}),
                                ),
                            );
                            return true;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                }
                false
            });
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "client::tests::operator_client_refuses_under_crew_identity",
                    "--nocapture",
                ])
                .env(PROBE, &socket)
                .env(crate::paths::IDENTITY_ENV, identity)
                .output()
                .unwrap();
            assert!(
                !peer.join().unwrap(),
                "operator connected from an agent environment"
            );
            assert!(
                child.status.success(),
                "{}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            std::fs::remove_file(&socket).unwrap();
        }
    }

    #[test]
    fn missing_token_file_is_a_clear_error() {
        if crate::test_support::isolate_operator_test(
            "client::tests::missing_token_file_is_a_clear_error",
        ) {
            return;
        }
        for response in [
            ServerFrame::err(0, RpcError::unauthorized("operator token required")),
            ServerFrame::ok(
                0,
                json!({"role":"operator", "version":"test", "protocol":PROTOCOL_VERSION, "pid":1}),
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("daemon.sock");
            let peer = hello_peer(&socket, response);
            let error = Client::operator(&socket, "test")
                .err()
                .expect("missing operator token must fail");
            let (_, hello) = peer
                .join()
                .unwrap()
                .expect("missing credentials must negotiate the protocol");
            assert_eq!(error.to_string(), format!(
                "no operator token at {}: operator commands must run as the user who started the daemon, outside agent sandboxes",
                crate::paths::operator_token_path(&socket).display()));
            assert_eq!(hello.token, None);
        }
    }

    #[test]
    fn operator_hello_carries_the_token_file() {
        if crate::test_support::isolate_operator_test(
            "client::tests::operator_hello_carries_the_token_file",
        ) {
            return;
        }
        let token = "synthetic-operator-secret";
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        crate::paths::write_private_file(
            &crate::paths::operator_token_path(&socket),
            token.as_bytes(),
        )
        .unwrap();
        let peer = hello_peer(
            &socket,
            ServerFrame::ok(
                0,
                json!({"role":"operator", "version":"test", "protocol":PROTOCOL_VERSION, "pid":1}),
            ),
        );
        let result = Client::operator(&socket, "token-test");
        let (line, hello) = peer
            .join()
            .unwrap()
            .expect("expected Hello before peer deadline");
        assert!(result.is_ok());
        assert_eq!(hello.token.as_deref(), Some(token));
        let expected = serde_json::to_string(&Request {
            id: 1,
            call: Call::Hello(Hello {
                role: Role::Operator,
                agent_id: None,
                token: Some(token.into()),
                client: "token-test".into(),
                protocol: PROTOCOL_VERSION,
                web: false,
                workspace: None,
            }),
        })
        .unwrap()
            + "\n";
        assert_eq!(line, expected);
    }

    #[test]
    fn hello_debug_does_not_reveal_operator_tokens() {
        let hello = Hello {
            role: Role::Operator,
            agent_id: None,
            token: Some("synthetic-secret-do-not-print".into()),
            client: "test".into(),
            protocol: PROTOCOL_VERSION,
            web: false,
            workspace: None,
        };
        assert!(!format!("{hello:?}").contains("synthetic-secret-do-not-print"));
    }

    #[test]
    fn token_errors_keep_the_cause_and_invalid_socket_paths_stay_connect_errors() {
        if crate::test_support::isolate_operator_test("client::tests::token_errors_keep_the_cause_and_invalid_socket_paths_stay_connect_errors") { return; }
        for socket in [Path::new(""), Path::new("/"), Path::new("..")] {
            assert!(matches!(
                Client::operator(socket, "test"),
                Err(ClientError::Connect { .. })
            ));
        }
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let token = crate::paths::operator_token_path(&socket);
        std::fs::create_dir(&token).unwrap();
        let error = Client::operator(&socket, "test").err().unwrap();
        assert!(error.to_string().contains("not a regular file"), "{error}");
        std::fs::remove_dir(&token).unwrap();
        crate::paths::write_private_file(&token, b"must-not-appear-in-message").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o000)).unwrap();
        let error = Client::operator(&socket, "test").err().unwrap();
        assert!(error.to_string().contains("Permission denied"), "{error}");
        assert!(!error.to_string().contains("must-not-appear-in-message"));
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn framing_errors_and_timeouts_close_the_client_before_reuse() {
        for response in ["{\"id\":1,\"result\":", "not-json\n"] {
            let (mut client, mut server) = paired_client();
            server
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            server.write_all(response.as_bytes()).unwrap();
            let error = client.call_value(Call::Ping).unwrap_err();
            assert!(matches!(
                error,
                ClientError::Timeout | ClientError::Protocol(_)
            ));
            let mut requests = BufReader::new(server);
            let mut request = String::new();
            requests.read_line(&mut request).unwrap();
            assert!(matches!(
                serde_json::from_str::<Request>(&request).unwrap().call,
                Call::Ping
            ));
            assert!(
                matches!(client.send(Call::Ping), Err(ClientError::Closed)),
                "reuse must fail before another request is sent"
            );
            assert!(matches!(
                client.call_value(Call::Ping),
                Err(ClientError::Closed)
            ));
            assert!(matches!(client.next_frame(), Err(ClientError::Closed)));
            request.clear();
            assert_eq!(
                requests.read_line(&mut request).unwrap(),
                0,
                "the peer must see closure, not another request"
            );
        }
    }

    #[test]
    fn rpc_errors_leave_the_client_usable() {
        let (mut client, mut server) = paired_client();
        send_frame(
            &mut server,
            ServerFrame::err(1, RpcError::invalid("bad request")),
        );
        send_frame(&mut server, ServerFrame::ok(2, json!({"pong": true})));
        assert!(matches!(
            client.call_value(Call::Ping),
            Err(ClientError::Rpc(_))
        ));
        assert_eq!(
            client.call_value(Call::Ping).unwrap(),
            json!({"pong": true})
        );
    }

    #[test]
    fn response_decode_errors_and_write_failures_poison_the_client() {
        let (mut client, mut server) = paired_client();
        send_frame(&mut server, ServerFrame::ok(1, json!({"wrong": "shape"})));
        assert!(matches!(
            client.call::<HelloInfo>(Call::Ping),
            Err(ClientError::Protocol(_))
        ));
        assert!(matches!(client.send(Call::Ping), Err(ClientError::Closed)));
        assert!(matches!(client.set_timeout(None), Err(ClientError::Closed)));
        let (mut client, server) = paired_client();
        server.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(matches!(client.send(Call::Ping), Err(ClientError::Io(_))));
        assert!(matches!(client.send(Call::Ping), Err(ClientError::Closed)));
    }

    #[test]
    fn disconnect_predicate_distinguishes_transport_from_rpc_and_version_errors() {
        for error in [
            ClientError::Closed,
            ClientError::Timeout,
            ClientError::Protocol("bad frame".into()),
            ClientError::Io(io::Error::from(io::ErrorKind::BrokenPipe)),
            ClientError::Connect {
                path: "socket".into(),
                source: io::Error::from(io::ErrorKind::NotFound),
            },
        ] {
            assert!(error.is_disconnect(), "{error}");
        }
        for error in [
            ClientError::Rpc(RpcError::invalid("bad request")),
            ClientError::Mismatch(Box::new(Mismatch {
                client_version: "0.1.0".into(),
                client_protocol: PROTOCOL_VERSION,
                daemon_version: None,
                daemon_protocol: Some(0),
                workspace: None,
                socket: "socket".into(),
                daemon_pid: None,
            })),
        ] {
            assert!(!error.is_disconnect(), "{error}");
        }
    }

    fn paired_client() -> (Client, UnixStream) {
        let (stream, server) = UnixStream::pair().unwrap();
        let client = Client {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next_id: 1,
            pushes: VecDeque::new(),
            closed: false,
        };
        client
            .set_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        (client, server)
    }

    fn send_frame(server: &mut UnixStream, frame: ServerFrame) {
        serde_json::to_writer(&mut *server, &frame).unwrap();
        server.write_all(b"\n").unwrap();
    }

    #[test]
    fn shutdown_handle_interrupts_a_held_call_without_waiting_for_its_deadline() {
        use std::sync::mpsc;
        let (mut client, mut server) = paired_client();
        client.set_timeout(Some(Duration::from_secs(90))).unwrap();
        let control = client.shutdown_handle().unwrap();
        let (done, completed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done.send(client.call_value(Call::Ping)).unwrap();
        });
        let mut request = String::new();
        BufReader::new(server.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Request>(&request).unwrap().call,
            Call::Ping
        ));
        control.shutdown();
        control.shutdown(); // Repeated host shutdown is harmless.
        let result = completed.recv_timeout(Duration::from_secs(1));
        // Even a regression must not strand a worker in this test process.
        let _ = server.shutdown(std::net::Shutdown::Both);
        worker.join().unwrap();
        assert!(matches!(
            result.unwrap(),
            Err(ClientError::Closed | ClientError::Io(_))
        ));
        // The peer never replied: cancellation cannot manufacture success.
        assert!(server.write_all(b"late response\n").is_err());
    }

    #[test]
    fn operator_control_is_registered_before_a_held_hello_or_an_already_requested_stop() {
        if crate::test_support::isolate_operator_test(
            "client::tests::operator_control_is_registered_before_a_held_hello_or_an_already_requested_stop",
        ) {
            return;
        }
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;
        for already_stopped in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("daemon.sock");
            crate::paths::write_private_file(
                &crate::paths::operator_token_path(&socket),
                b"synthetic-cancellation-token",
            )
            .unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            let (registered, controls) = mpsc::channel();
            let (finished, outcomes) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = Client::operator_with_control(&socket, "test", |control| {
                    if already_stopped {
                        control.shutdown();
                    }
                    registered.send(control).unwrap();
                });
                finished.send(result.err()).unwrap();
            });
            let (server, _) = listener.accept().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let control = controls.recv_timeout(Duration::from_secs(1)).unwrap();
            let mut hello = String::new();
            let read = BufReader::new(server.try_clone().unwrap())
                .read_line(&mut hello)
                .unwrap();
            if already_stopped {
                assert_eq!(read, 0, "stop registration must precede hello write");
            } else {
                assert!(matches!(
                    serde_json::from_str::<Request>(&hello).unwrap().call,
                    Call::Hello(_)
                ));
                control.shutdown();
            }
            let result = outcomes.recv_timeout(Duration::from_secs(1));
            let _ = server.shutdown(std::net::Shutdown::Both);
            worker.join().unwrap();
            assert!(result.unwrap().unwrap().is_disconnect());
        }
    }

    fn deliver(text: &str) -> Push {
        Push::Deliver {
            ids: vec![text.into()],
            text: text.into(),
            immediate: false,
            batch_id: None,
        }
    }

    #[test]
    fn listen_returns_pushes_buffered_before_its_reply() {
        let (mut client, mut server) = paired_client();
        send_frame(&mut server, ServerFrame::push(deliver("first")));
        send_frame(&mut server, ServerFrame::push(deliver("second")));
        send_frame(&mut server, ServerFrame::ok(1, json!({"listening": true})));
        send_frame(&mut server, ServerFrame::push(deliver("third")));
        client.call_value(Call::Listen).unwrap();
        for text in ["first", "second", "third"] {
            assert_eq!(client.next_frame().unwrap().push, Some(deliver(text)));
        }
    }

    #[test]
    fn later_calls_do_not_rebuffer_earlier_pushes() {
        let (mut client, mut server) = paired_client();
        send_frame(&mut server, ServerFrame::push(deliver("first")));
        send_frame(&mut server, ServerFrame::ok(1, json!(null)));
        send_frame(&mut server, ServerFrame::push(deliver("second")));
        send_frame(&mut server, ServerFrame::ok(2, json!({"pong": true})));
        client.call_value(Call::Listen).unwrap();
        assert_eq!(
            client.call_value(Call::Ping).unwrap(),
            json!({"pong": true})
        );
        for text in ["first", "second"] {
            assert_eq!(client.next_frame().unwrap().push, Some(deliver(text)));
        }
    }

    #[test]
    fn split_preserves_buffered_pushes() {
        let (mut client, mut server) = paired_client();
        send_frame(&mut server, ServerFrame::push(deliver("buffered")));
        send_frame(&mut server, ServerFrame::ok(1, json!(null)));
        client.call_value(Call::Listen).unwrap();
        let (mut reader, _writer) = client.split();
        assert_eq!(reader.next().unwrap().push, Some(deliver("buffered")));
    }

    #[test]
    fn subscribe_preserves_pushes_and_ignores_replies() {
        let (client, mut server) = paired_client();
        send_frame(&mut server, ServerFrame::push(deliver("buffered")));
        send_frame(&mut server, ServerFrame::ok(1, json!(null)));
        send_frame(&mut server, ServerFrame::ok(2, json!(null)));
        send_frame(&mut server, ServerFrame::push(deliver("live")));
        let mut stream = client.subscribe().unwrap();
        assert_eq!(stream.next().unwrap(), deliver("buffered"));
        assert_eq!(stream.next().unwrap(), deliver("live"));
        server.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(matches!(stream.next(), Err(ClientError::Closed)));
    }

    #[test]
    fn future_push_data_is_skipped_without_losing_known_pushes() {
        let (client, mut server) = paired_client();
        server
            .write_all(b"{\"push\":{\"kind\":\"future\",\"data\":{\"x\":1}}}\n")
            .unwrap();
        send_frame(&mut server, ServerFrame::ok(1, json!(null)));
        server
            .write_all(b"{\"push\":{\"kind\":\"future\",\"data\":[1,2]}}\n")
            .unwrap();
        send_frame(&mut server, ServerFrame::push(Push::Resync));
        let mut stream = client.subscribe().unwrap();
        assert_eq!(stream.next().unwrap(), Push::Resync);
    }

    #[test]
    fn hello_rejects_older_and_newer_daemons_for_both_roles() {
        if crate::test_support::isolate_operator_test(
            "client::tests::hello_rejects_older_and_newer_daemons_for_both_roles",
        ) {
            return;
        }
        for protocol in [PROTOCOL_VERSION - 1, PROTOCOL_VERSION, PROTOCOL_VERSION + 1] {
            for agent in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let socket = temp.path().join("hello.sock");
                crate::paths::write_private_file(
                    &crate::paths::operator_token_path(&socket),
                    b"test",
                )
                .unwrap();
                let role = if agent { Role::Agent } else { Role::Operator };
                let agent_id = agent.then_some("worker");
                let peer = hello_peer(
                    &socket,
                    ServerFrame::ok(
                        0,
                        json!({"role":role, "agent_id":agent_id, "version":"test", "protocol":protocol, "pid":1}),
                    ),
                );
                let result = if agent {
                    Client::agent(
                        &Identity {
                            socket: socket.to_string_lossy().into_owned(),
                            agent_id: "worker".into(),
                            token: "test".into(),
                            orchestrator: false,
                        },
                        "test",
                    )
                } else {
                    Client::operator(&socket, "test")
                };
                let (_, hello) = peer
                    .join()
                    .unwrap()
                    .expect("expected Hello before peer deadline");
                assert_eq!(hello.role, role);
                assert_eq!(hello.agent_id.as_deref(), agent_id);
                if protocol == PROTOCOL_VERSION {
                    assert!(result.is_ok());
                } else {
                    let error = result.err().expect("mismatched daemon version must fail");
                    assert!(error.to_string().contains("restart"), "{error}");
                }
            }
        }
    }

    #[test]
    fn a_mismatch_names_both_sides_and_says_restart_the_team() {
        let socket = Path::new("/run/user/1000/crew/test-mismatch.sock");
        let ours = format!(
            "crew {} (protocol {PROTOCOL_VERSION})",
            env!("CARGO_PKG_VERSION")
        );
        let said = |result: ClientResult<HelloInfo>| {
            let error = check_hello(result, socket).unwrap_err();
            assert!(error.is_protocol_mismatch(), "{error}");
            assert!(!error.is_disconnect(), "{error}");
            error.to_string()
        };
        // A daemon that says who it is.
        let refusal = RpcError::new("protocol", "client speaks protocol 9, daemon speaks 7")
            .with_data(json!({"version": "0.2.0", "protocol": 7, "workspace": "/w/payments"}));
        assert_eq!(
            said(Err(ClientError::Rpc(refusal))),
            format!(
                "{ours} cannot talk to the daemon of /w/payments, which is crew 0.2.0 \
(protocol 7): restart the team (`crew stop`, then `crew run`)"
            )
        );
        // Daemons from before that: the protocol is in their message.
        for (refusal, protocol) in [
            (
                RpcError::new("protocol", "client speaks protocol 2, daemon speaks 3"),
                3,
            ),
            (
                RpcError::invalid("client speaks protocol 2, daemon speaks 1"),
                1,
            ),
        ] {
            assert_eq!(
                said(Err(ClientError::Rpc(refusal))),
                format!(
                    "{ours} cannot talk to the daemon at {}, which speaks protocol {protocol}: \
restart the team (`crew stop`, then `crew run`)",
                    socket.display()
                )
            );
        }
        // Data of an unexpected shape is not taken, and does not hide the message.
        let odd = RpcError::new("protocol", "client speaks protocol 2, daemon speaks 3")
            .with_data(json!({"version": 3, "protocol": "three", "workspace": ""}));
        assert!(said(Err(ClientError::Rpc(odd))).contains(", which speaks protocol 3: "));
        let unknown = RpcError::new("protocol", "no");
        assert!(said(Err(ClientError::Rpc(unknown))).contains(", which speaks another protocol: "));
        // A daemon that accepted the hello but speaks another protocol.
        let info = |protocol| HelloInfo {
            role: Role::Agent,
            agent_id: Some("worker".into()),
            project: None,
            version: "0.0.9".into(),
            protocol,
            pid: 1,
            web_proof: None,
        };
        assert!(said(Ok(info(PROTOCOL_VERSION + 1))).contains(&format!(
            ", which is crew 0.0.9 (protocol {}): ",
            PROTOCOL_VERSION + 1
        )));
        assert!(check_hello(Ok(info(PROTOCOL_VERSION)), socket).is_ok());
        let error = check_hello(
            Err(ClientError::Rpc(RpcError::invalid("other error"))),
            socket,
        )
        .unwrap_err();
        assert!(!error.is_protocol_mismatch());
    }

    #[test]
    fn a_daemon_that_does_not_say_its_workspace_is_found_in_the_registry() {
        let registry = tempfile::tempdir().unwrap();
        let socket = registry.path().join("daemon.sock");
        let entry = |socket: &Path, workspace: &str| {
            json!({"workspace": workspace, "state_dir": format!("{workspace}/.crew"),
                "socket": socket, "pid": 1, "started_at": 0})
            .to_string()
        };
        assert_eq!(workspace_in_registry(registry.path(), &socket), None);
        let write = |name: &str, text: String| {
            std::fs::write(registry.path().join(name), text).unwrap();
        };
        write("broken.json", "{".into());
        write(
            "other.json",
            entry(Path::new("/elsewhere.sock"), "/w/other"),
        );
        assert_eq!(workspace_in_registry(registry.path(), &socket), None);
        write("this.json", entry(&socket, "/w/payments"));
        assert_eq!(
            workspace_in_registry(registry.path(), &socket).as_deref(),
            Some("/w/payments")
        );
        assert_eq!(
            workspace_in_registry(&registry.path().join("none"), &socket),
            None
        );
    }
}
