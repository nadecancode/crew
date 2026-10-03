//! Blocking client for the daemon socket.
//!
//! Everything outside the daemon is synchronous: short-lived CLI commands, the
//! MCP bridge (one request at a time over stdio), hooks, and the dashboard,
//! which reads pushes on a dedicated thread.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
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
    #[error("the crew daemon speaks protocol {daemon}, this crew speaks {client}: restart the team after upgrading, or use the binary that started it")]
    Version { daemon: u32, client: u32 },
    #[error("incompatible crew protocol: {0}; restart the team after upgrading, or use the binary that started it")]
    IncompatibleProtocol(String),
}

impl ClientError {
    /// The stream is unavailable or unsafe to continue. This does not make
    /// replaying a mutation safe: a request may have run before a read failed.
    pub fn is_disconnect(&self) -> bool {
        matches!(
            self,
            Self::Connect { .. } | Self::Io(_) | Self::Timeout | Self::Closed | Self::Protocol(_)
        )
    }
    pub fn is_protocol_mismatch(&self) -> bool {
        matches!(self, Self::Version { .. } | Self::IncompatibleProtocol(_))
    }
}

fn check_hello(result: ClientResult<HelloInfo>) -> ClientResult<HelloInfo> {
    let info = match result {
        Err(ClientError::Rpc(error))
            if error.code == "protocol"
            // Legacy v1 daemons used the generic invalid code for this error.
            || (error.code == "invalid" && error.message.starts_with("client speaks protocol ")) =>
        {
            return Err(ClientError::IncompatibleProtocol(error.message));
        }
        result => result?,
    };
    if info.protocol != PROTOCOL_VERSION {
        return Err(ClientError::Version {
            daemon: info.protocol,
            client: PROTOCOL_VERSION,
        });
    }
    Ok(info)
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
        let mut c = Self::connect(socket)?;
        let info = check_hello(c.call(Call::Hello(Hello {
            role: Role::Operator,
            agent_id: None,
            token: None,
            client: client.to_string(),
            protocol: PROTOCOL_VERSION,
        })))?;
        Ok((c, info))
    }

    /// Connect and authenticate as an agent.
    pub fn agent(identity: &Identity, client: &str) -> ClientResult<(Self, HelloInfo)> {
        let mut c = Self::connect(Path::new(&identity.socket))?;
        let info = check_hello(c.call(Call::Hello(Hello {
            role: Role::Agent,
            agent_id: Some(identity.agent_id.clone()),
            token: Some(identity.token.clone()),
            client: client.to_string(),
            protocol: PROTOCOL_VERSION,
        })))?;
        Ok((c, info))
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

    /// Read the next frame, blocking until one arrives.
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

    #[test]
    fn framing_errors_and_timeouts_close_the_client_before_reuse() {
        for response in ["{\"id\":1,\"result\":", "not-json\n"] {
            let (mut client, mut server) = paired_client();
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

    fn deliver(text: &str) -> Push {
        Push::Deliver {
            ids: vec![text.into()],
            text: text.into(),
        }
    }

    #[test]
    fn listen_returns_pushes_buffered_before_its_reply() {
        let (mut client, mut server) = paired_client();
        server
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
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
        use std::os::unix::net::UnixListener;
        for protocol in [PROTOCOL_VERSION - 1, PROTOCOL_VERSION, PROTOCOL_VERSION + 1] {
            for agent in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let socket = temp.path().join("hello.sock");
                let listener = UnixListener::bind(&socket).unwrap();
                let peer = std::thread::spawn(move || {
                    let (mut server, _) = listener.accept().unwrap();
                    let mut line = String::new();
                    BufReader::new(server.try_clone().unwrap())
                        .read_line(&mut line)
                        .unwrap();
                    let request: Request = serde_json::from_str(&line).unwrap();
                    let Call::Hello(hello) = request.call else {
                        panic!("expected Hello")
                    };
                    send_frame(
                        &mut server,
                        ServerFrame::ok(
                            request.id,
                            json!({
                                "role": hello.role, "agent_id": hello.agent_id, "version": "test",
                                "protocol": protocol, "pid": 1
                            }),
                        ),
                    );
                });
                let result = if agent {
                    Client::agent(
                        &Identity {
                            socket: socket.to_string_lossy().into_owned(),
                            agent_id: "worker".into(),
                            token: "test".into(),
                        },
                        "test",
                    )
                } else {
                    Client::operator(&socket, "test")
                };
                peer.join().unwrap();
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
    fn hello_protocol_refusals_have_a_restart_hint() {
        for error in [
            RpcError::new("protocol", "client speaks protocol 2, daemon speaks 3"),
            RpcError::invalid("client speaks protocol 2, daemon speaks 1"),
        ] {
            let error = check_hello(Err(ClientError::Rpc(error))).unwrap_err();
            assert!(error.is_protocol_mismatch());
            assert!(error.to_string().contains("restart"));
        }
        let error =
            check_hello(Err(ClientError::Rpc(RpcError::invalid("other error")))).unwrap_err();
        assert!(!error.is_protocol_mismatch());
    }
}
