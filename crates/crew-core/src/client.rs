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
use crate::rpc::{Call, Hello, HelloInfo, Push, Request, Role, RpcError, ServerFrame, PROTOCOL_VERSION};

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
}

pub type ClientResult<T> = Result<T, ClientError>;

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Pushes that arrived while waiting for a response.
    pushes: VecDeque<Push>,
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
        })
    }

    /// Connect and authenticate as the human operator.
    pub fn operator(socket: &Path, client: &str) -> ClientResult<(Self, HelloInfo)> {
        let mut c = Self::connect(socket)?;
        let info = c.call(Call::Hello(Hello {
            role: Role::Operator,
            agent_id: None,
            token: None,
            client: client.to_string(),
            protocol: PROTOCOL_VERSION,
        }))?;
        Ok((c, info))
    }

    /// Connect and authenticate as an agent.
    pub fn agent(identity: &Identity, client: &str) -> ClientResult<(Self, HelloInfo)> {
        let mut c = Self::connect(Path::new(&identity.socket))?;
        let info = c.call(Call::Hello(Hello {
            role: Role::Agent,
            agent_id: Some(identity.agent_id.clone()),
            token: Some(identity.token.clone()),
            client: client.to_string(),
            protocol: PROTOCOL_VERSION,
        }))?;
        Ok((c, info))
    }

    /// Bound how long a call may wait for the daemon (`None` waits forever).
    pub fn set_timeout(&self, timeout: Option<Duration>) -> ClientResult<()> {
        self.reader.get_ref().set_read_timeout(timeout)?;
        self.writer.set_write_timeout(timeout)?;
        Ok(())
    }

    /// Write a request without waiting for its response.
    pub fn send(&mut self, call: Call) -> ClientResult<u64> {
        let id = self.next_id;
        self.next_id += 1;
        write_request(&mut self.writer, id, call)?;
        Ok(id)
    }

    /// Read the next frame, blocking until one arrives.
    pub fn next_frame(&mut self) -> ClientResult<ServerFrame> {
        read_frame(&mut self.reader)
    }

    pub fn call_value(&mut self, call: Call) -> ClientResult<Value> {
        let id = self.send(call)?;
        loop {
            let frame = self.next_frame()?;
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
        serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("unexpected `{method}` response: {e}")))
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
        if let Some(push) = self.buffered.pop_front() {
            return Ok(push);
        }
        loop {
            let frame = read_frame(&mut self.reader)?;
            if let Some(push) = frame.push {
                return Ok(push);
            }
        }
    }
}

pub struct FrameReader {
    reader: BufReader<UnixStream>,
}

impl FrameReader {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> ClientResult<ServerFrame> {
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
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
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
