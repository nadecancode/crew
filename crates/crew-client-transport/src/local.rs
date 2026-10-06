//! Authenticated Linux/macOS operator IO. Credentials stay inside this adapter.
//! No polling, unbounded line reads or callbacks under a state mutex.

use std::collections::VecDeque;
use std::io::Read;
use std::os::fd::IntoRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crew_client::api::{
    ClientScope, ConnectionContext, ConnectionMode, Grant, GrantTier, PeerInfo, SnapshotMode,
};
use crew_core::{client::refuse_in_agent_context, paths};
use crew_protocol::rpc::{Hello, HelloInfo, Role, ServerFrame, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use crate::framing::{validate_record, FrameError, LineBuffer};
use crate::queue::{ChannelSink, EventQueue, QUEUE_BYTES, QUEUE_FRAMES};
use crate::{ChannelId, ChannelPurpose, HostEffect, HostEvent, SendDisposition, TransportEnd};

const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct LocalProfile {
    pub id: String,
    pub endpoint_id: String,
    pub workspace_key: String,
    pub socket: PathBuf,
}

impl LocalProfile {
    pub fn validate(&self) -> Result<(), TransportEnd> {
        if self.id.is_empty()
            || self.id.len() > 128
            || self.endpoint_id.is_empty()
            || self.endpoint_id.len() > 128
            || self.workspace_key.is_empty()
            || self.workspace_key.len() > 4096
            || !self.socket.is_absolute()
            || self.socket.as_os_str().len() > 4096
        {
            return Err(TransportEnd::Permission);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LocalWorkspace {
    pub profile: LocalProfile,
    pub workspace: String,
    pub project: Option<String>,
    pub daemon_pid: u32,
    pub daemon_started_at_ms: i64,
}

/// Read only this user's protected registry. Do not prune entries, join teams,
/// follow links, scan ports or substitute another socket for a saved profile.
pub fn discover_profiles() -> Result<Vec<LocalWorkspace>, TransportEnd> {
    refuse_in_agent_context().map_err(|_| TransportEnd::AgentContext)?;
    let registry = paths::registry_dir();
    let directory = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&registry)
    {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(TransportEnd::Permission),
    };
    let metadata = directory.metadata().map_err(|_| TransportEnd::Permission)?;
    if metadata.uid() != paths::uid() || metadata.mode() & 0o077 != 0 {
        return Err(TransportEnd::Permission);
    }
    let duplicated = directory
        .try_clone()
        .map_err(|_| TransportEnd::Permission)?
        .into_raw_fd();
    // SAFETY: fdopendir takes ownership of this duplicated directory fd.
    let pointer = unsafe { libc::fdopendir(duplicated) };
    if pointer.is_null() {
        // SAFETY: fdopendir failed and did not take ownership.
        unsafe {
            libc::close(duplicated);
        }
        return Err(TransportEnd::Permission);
    }
    struct Entries(*mut libc::DIR);
    impl Drop for Entries {
        fn drop(&mut self) {
            // SAFETY: this object alone owns the successfully opened DIR.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let entries = Entries(pointer);
    let mut result = Vec::new();
    let mut seen = 0;
    loop {
        // SAFETY: entries owns this DIR; copy the name before another readdir.
        let entry = unsafe { libc::readdir(entries.0) };
        if entry.is_null() {
            break;
        }
        seen += 1;
        if seen > 4096 {
            return Err(TransportEnd::Overflow);
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name.len() != 21
            || !name.ends_with(b".json")
            || !name[..16]
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            continue;
        }
        let name = std::str::from_utf8(name).map_err(|_| TransportEnd::Malformed)?;
        let file = match paths::open_beneath(&directory, std::path::Path::new(name)) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.uid() != paths::uid()
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() > 16 * 1024
        {
            continue;
        }
        let mut bytes = Vec::new();
        if file.take(16 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 16 * 1024 {
            continue;
        }
        let info: paths::DaemonInfo = match serde_json::from_slice(&bytes) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let state_dir = std::path::Path::new(&info.state_dir);
        let id = paths::session_id(state_dir);
        if !state_dir.is_absolute()
            || name[..16] != id
            || paths::socket_path(state_dir) != std::path::Path::new(&info.socket)
            || !paths::pid_alive(info.pid)
            || info.pid_start.is_some_and(|started| {
                paths::process_start(info.pid).is_some_and(|actual| actual != started)
            })
            || info
                .boot_id
                .as_ref()
                .is_some_and(|boot| paths::boot_id().is_some_and(|actual| &actual != boot))
        {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&info.socket) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != paths::uid()
            || metadata.mode() & 0o077 != 0
        {
            continue;
        }
        let profile = LocalProfile {
            id: format!("local:{id}"),
            endpoint_id: format!("local:{id}"),
            workspace_key: id,
            socket: info.socket.into(),
        };
        if profile.validate().is_err() {
            continue;
        }
        if result.len() >= 128 {
            return Err(TransportEnd::Overflow);
        }
        result.push(LocalWorkspace {
            profile,
            workspace: info.workspace,
            project: info.project,
            daemon_pid: info.pid,
            daemon_started_at_ms: info.started_at,
        });
    }
    result.sort_by_key(|workspace| workspace.daemon_started_at_ms);
    Ok(result)
}

struct WriteFrame {
    request_id: u64,
    text: String,
    _permit: OwnedSemaphorePermit,
}

pub struct LocalChannel {
    channel_id: ChannelId,
    sender: mpsc::Sender<WriteFrame>,
    bytes: Arc<Semaphore>,
    stop: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
    sink: ChannelSink,
}

impl LocalChannel {
    pub fn send(&self, request_id: u64, text: String) -> Result<(), TransportEnd> {
        validate_record(&text).map_err(frame_end)?;
        if *self.stop.borrow() {
            return Err(TransportEnd::Closed);
        }
        let length = u32::try_from(text.len() + 1).map_err(|_| TransportEnd::Oversized)?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(length)
            .map_err(|_| TransportEnd::Overflow)?;
        self.sender
            .try_send(WriteFrame {
                request_id,
                text,
                _permit: permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransportEnd::Overflow,
                mpsc::error::TrySendError::Closed(_) => TransportEnd::Closed,
            })
    }

    pub fn close(&self) {
        let _ = self.stop.send(true);
    }

    /// Wait for both reader and writer teardown, without stopping the daemon.
    pub async fn wait_closed(&mut self) {
        while !*self.finished.borrow_and_update() {
            if self.finished.changed().await.is_err() {
                break;
            }
        }
    }

    pub fn fail(&self, reason: TransportEnd) {
        self.sink.end(reason);
        self.close();
    }

    pub fn channel_id(&self) -> ChannelId {
        self.channel_id
    }
}

impl Drop for LocalChannel {
    fn drop(&mut self) {
        self.close();
    }
}

pub struct LocalTransport;

impl LocalTransport {
    /// Starts worker-owned authentication. `Opened` is emitted only after the
    /// protected token and exact protocol handshake have both succeeded.
    pub fn open(
        profile: LocalProfile,
        effect: &HostEffect,
        queue: &EventQueue,
    ) -> Result<LocalChannel, TransportEnd> {
        profile.validate()?;
        let HostEffect::Open {
            channel_id,
            endpoint_id,
            attempt,
            purpose,
            expected_scope,
            profile_id,
        } = effect
        else {
            return Err(TransportEnd::Permission);
        };
        if *purpose == ChannelPurpose::Index
            || profile.id != *profile_id
            || profile.endpoint_id != *endpoint_id
        {
            return Err(TransportEnd::Permission);
        }
        let sink = queue.register(*channel_id)?;
        let (sender, receiver) = mpsc::channel(QUEUE_FRAMES);
        let (stop, cancel) = watch::channel(false);
        let (completed, finished) = watch::channel(false);
        let worker_sink = sink.clone();
        let channel_id = *channel_id;
        let attempt = *attempt;
        let expected = expected_scope.clone();
        tokio::spawn(async move {
            let reason = channel_worker(
                profile,
                channel_id,
                attempt,
                expected,
                worker_sink.clone(),
                receiver,
                cancel,
            )
            .await;
            worker_sink.end(reason);
            let _ = completed.send(true);
        });
        Ok(LocalChannel {
            channel_id,
            sender,
            bytes: Arc::new(Semaphore::new(QUEUE_BYTES)),
            stop,
            finished,
            sink,
        })
    }
}

async fn channel_worker(
    profile: LocalProfile,
    channel_id: ChannelId,
    attempt: u64,
    expected: Option<ClientScope>,
    sink: ChannelSink,
    receiver: mpsc::Receiver<WriteFrame>,
    mut cancel: watch::Receiver<bool>,
) -> TransportEnd {
    let opened = tokio::select! {
        _ = cancel.changed() => return TransportEnd::Stopped,
        value = tokio::time::timeout(HELLO_TIMEOUT, authenticate(&profile)) => value,
    };
    let (stream, info, buffered) = match opened {
        Ok(Ok(value)) => value,
        Ok(Err(reason)) => return reason,
        Err(_) => return TransportEnd::Timeout,
    };
    let scope = ClientScope {
        endpoint_id: profile.endpoint_id,
        workspace_id: expected
            .as_ref()
            .map_or(profile.workspace_key, |scope| scope.workspace_id.clone()),
        generation: None,
        daemon_pid: Some(info.pid),
        daemon_started_at_ms: None,
        attempt,
    };
    let context = ConnectionContext {
        mode: ConnectionMode::LocalOperator,
        grant: Grant {
            grant_id: format!("local:{}", profile.id),
            tiers: vec![GrantTier::Read, GrantTier::Control, GrantTier::Terminal],
            workspace_ids: vec![scope.workspace_id.clone()],
            expires_at_ms: None,
        },
        snapshot_mode: SnapshotMode::OrderedConnection,
        scope,
    };
    let peer = PeerInfo {
        version: info.version,
        protocol: info.protocol,
        pid: info.pid,
        project: info.project,
    };
    if let Err(reason) = sink.try_send(HostEvent::WorkspaceOpened {
        channel_id,
        context,
        peer,
    }) {
        return reason;
    }
    let (reader, writer) = stream.into_split();
    let mut reads = tokio::spawn(read_frames(channel_id, reader, buffered, sink.clone()));
    let mut writes = tokio::spawn(write_frames(
        channel_id,
        writer,
        receiver,
        sink,
        cancel.clone(),
    ));
    let reason = tokio::select! {
        _ = cancel.changed() => TransportEnd::Stopped,
        result = &mut reads => result.unwrap_or(TransportEnd::Closed),
        result = &mut writes => result.unwrap_or(TransportEnd::Closed),
    };
    // Drop cannot detach a live writer: cancel, then await each still-live task.
    if !reads.is_finished() {
        reads.abort();
        let _ = reads.await;
    }
    if !writes.is_finished() {
        writes.abort();
        let _ = writes.await;
    }
    reason
}

async fn authenticate(
    profile: &LocalProfile,
) -> Result<(UnixStream, HelloInfo, VecDeque<String>), TransportEnd> {
    refuse_in_agent_context().map_err(|_| TransportEnd::AgentContext)?;
    let socket = profile.socket.clone();
    let token = tokio::task::spawn_blocking(move || {
        let metadata = std::fs::symlink_metadata(&socket).map_err(|_| TransportEnd::Unreachable)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != paths::uid()
            || metadata.mode() & 0o077 != 0
        {
            return Err(TransportEnd::Permission);
        }
        paths::read_operator_token(&socket).map_err(|_| TransportEnd::Permission)
    })
    .await
    .map_err(|_| TransportEnd::Closed)??;
    let missing_token = token.is_none();
    let mut stream = UnixStream::connect(&profile.socket)
        .await
        .map_err(|_| TransportEnd::Unreachable)?;
    let peer = stream.peer_cred().map_err(|_| TransportEnd::Permission)?;
    if peer.uid() != paths::uid() {
        return Err(TransportEnd::Permission);
    }
    let hello = Hello {
        role: Role::Operator,
        agent_id: None,
        token,
        client: "native".into(),
        protocol: PROTOCOL_VERSION,
        web: false,
        workspace: None,
    };
    let request = serde_json::json!({"id": 0, "method": "hello", "params": hello});
    let mut text = serde_json::to_vec(&request).map_err(|_| TransportEnd::Malformed)?;
    text.push(b'\n');
    stream
        .write_all(&text)
        .await
        .map_err(|_| TransportEnd::Closed)?;
    // Credential-bearing request is never included in diagnostics or events.
    drop(text);
    let mut line = LineBuffer::default();
    let mut buffer = [0; 8192];
    loop {
        let length = stream
            .read(&mut buffer)
            .await
            .map_err(|_| TransportEnd::Closed)?;
        if length == 0 {
            return Err(if line.finish().is_ok() {
                TransportEnd::Closed
            } else {
                TransportEnd::Malformed
            });
        }
        let records = line.feed(&buffer[..length]).map_err(frame_end)?;
        let mut records = VecDeque::from(records);
        let Some(text) = records.pop_front() else {
            continue;
        };
        let frame: ServerFrame =
            serde_json::from_str(&text).map_err(|_| TransportEnd::Malformed)?;
        if frame.id != Some(0)
            || frame.push.is_some()
            || (frame.result.is_some() == frame.error.is_some())
        {
            return Err(TransportEnd::Malformed);
        }
        if let Some(error) = frame.error {
            return Err(match error.code.as_str() {
                "protocol" => TransportEnd::ProtocolMismatch,
                "invalid" if error.message.starts_with("client speaks protocol ") => {
                    TransportEnd::ProtocolMismatch
                }
                "unauthorized" if missing_token => TransportEnd::MissingCredential,
                "unauthorized" => TransportEnd::Authentication,
                _ => TransportEnd::Permission,
            });
        }
        let info: HelloInfo = serde_json::from_value(frame.result.ok_or(TransportEnd::Malformed)?)
            .map_err(|_| TransportEnd::Malformed)?;
        if info.protocol != PROTOCOL_VERSION {
            return Err(TransportEnd::ProtocolMismatch);
        }
        if missing_token {
            return Err(TransportEnd::MissingCredential);
        }
        if info.role != Role::Operator
            || info.agent_id.is_some()
            || info.web_proof.is_some()
            || info.pid == 0
            || peer
                .pid()
                .is_some_and(|pid| u32::try_from(pid).ok() != Some(info.pid))
        {
            return Err(TransportEnd::Authentication);
        }
        // No pushes precede a local subscription. An unsolicited/partial tail
        // at hello is uncertain framing, so close rather than losing it.
        if line.buffered_bytes() != 0 || !records.is_empty() {
            return Err(TransportEnd::Malformed);
        }
        return Ok((stream, info, records));
    }
}

async fn read_frames(
    channel_id: ChannelId,
    mut reader: tokio::net::unix::OwnedReadHalf,
    mut buffered: VecDeque<String>,
    sink: ChannelSink,
) -> TransportEnd {
    let mut line = LineBuffer::default();
    let mut buffer = [0; 8192];
    loop {
        while let Some(text) = buffered.pop_front() {
            if let Err(reason) = sink.try_send(HostEvent::Frame { channel_id, text }) {
                return reason;
            }
        }
        let length = match reader.read(&mut buffer).await {
            Ok(value) => value,
            Err(_) => return TransportEnd::Closed,
        };
        if length == 0 {
            return if line.finish().is_ok() {
                TransportEnd::Closed
            } else {
                TransportEnd::Malformed
            };
        }
        match line.feed(&buffer[..length]) {
            Ok(records) => buffered.extend(records),
            Err(error) => return frame_end(error),
        }
    }
}

async fn write_frames(
    channel_id: ChannelId,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut receiver: mpsc::Receiver<WriteFrame>,
    sink: ChannelSink,
    mut cancel: watch::Receiver<bool>,
) -> TransportEnd {
    loop {
        let frame = tokio::select! {
            biased;
            _ = cancel.changed() => return TransportEnd::Stopped,
            frame = receiver.recv() => match frame { Some(frame) => frame, None => return TransportEnd::Closed },
        };
        if *cancel.borrow() {
            return TransportEnd::Stopped;
        }
        if let Err(reason) = sink.try_send(HostEvent::SendProgress {
            channel_id,
            request_id: frame.request_id,
            disposition: SendDisposition::WriteStarted,
        }) {
            return reason;
        }
        let mut bytes = frame.text.into_bytes();
        bytes.reserve_exact(1);
        bytes.push(b'\n');
        let written = tokio::select! {
            biased;
            _ = cancel.changed() => return TransportEnd::Stopped,
            result = writer.write_all(&bytes) => result,
        };
        if written.is_err() {
            return TransportEnd::Closed;
        }
        if let Err(reason) = sink.try_send(HostEvent::SendProgress {
            channel_id,
            request_id: frame.request_id,
            disposition: SendDisposition::FullyWritten,
        }) {
            return reason;
        }
    }
}

fn frame_end(error: FrameError) -> TransportEnd {
    match error {
        FrameError::Oversized => TransportEnd::Oversized,
        _ => TransportEnd::Malformed,
    }
}
