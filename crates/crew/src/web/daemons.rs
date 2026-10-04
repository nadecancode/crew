//! How the dashboard server reaches the daemons (notes/web-multi.md
//! §multi-join, §multi-security "The server to a daemon"): the registry it
//! reads, the check an entry must pass before its socket is dialled, and the
//! dial itself, `hello` as the operator with the token the daemon's token
//! file holds at that moment. Behind a trait, so that tests can put stand-in
//! daemons and a registry of their own in its place.

use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{Call, Hello, HelloInfo, Request, Role, ServerFrame, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// How long a daemon has to answer the server's `hello`.
pub(crate) const DIAL_WITHIN: Duration = Duration::from_secs(2);

pub(crate) type Lines = BufReader<Pin<Box<dyn AsyncRead + Send>>>;
pub(crate) type Writer = Pin<Box<dyn AsyncWrite + Send>>;
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A connection to a daemon after its `hello`.
pub(crate) struct Dialled {
    pub lines: Lines,
    pub writer: Writer,
    /// The daemon's answer to the `hello`.
    pub hello: HelloInfo,
}

/// Why a daemon could not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialError {
    /// It answered `protocol`: another version of crew.
    Protocol(String),
    /// It refused the operator's token, or the token file could not be read.
    Refused(String),
    /// No connection, or no answer in time: it is going, or coming, or hangs.
    NoAnswer(String),
}

impl DialError {
    pub(crate) fn detail(&self) -> &str {
        match self {
            DialError::Protocol(why) | DialError::Refused(why) | DialError::NoAnswer(why) => why,
        }
    }
}

/// What the server knows of the daemons of this user.
pub(crate) trait Daemons: Send + Sync {
    /// Every session the registry publishes as live.
    fn sessions(&self) -> BoxFuture<'_, Vec<DaemonInfo>>;
    /// Whether the server may dial what `info` names: its socket must be the
    /// one the server derives for its state directory, and this user's.
    fn check(&self, info: &DaemonInfo) -> Result<(), String>;
    /// Dial `info`'s daemon and say `hello` as the operator; `web`: for a
    /// browser, so that the daemon holds the connection to W7.
    fn dial(&self, info: &DaemonInfo, web: bool) -> BoxFuture<'static, Result<Dialled, DialError>>;
}

/// The id of a registry entry: the stem of its socket, as its state
/// directory names it.
pub(crate) fn id_of(info: &DaemonInfo) -> String {
    paths::session_id(Path::new(&info.state_dir))
}

/// The workspace a state directory belongs to.
pub(crate) fn workspace_of(state_dir: &Path) -> String {
    state_dir
        .parent()
        .unwrap_or(state_dir)
        .to_string_lossy()
        .into_owned()
}

/// The real registry, sockets and token files.
pub(crate) struct Registered;

impl Daemons for Registered {
    fn sessions(&self) -> BoxFuture<'_, Vec<DaemonInfo>> {
        Box::pin(async {
            tokio::task::spawn_blocking(paths::list_sessions)
                .await
                .unwrap_or_default()
        })
    }

    fn check(&self, info: &DaemonInfo) -> Result<(), String> {
        let socket = PathBuf::from(&info.socket);
        let derived = paths::socket_path(Path::new(&info.state_dir));
        if socket != derived {
            return Err(format!(
                "its registry entry names the socket {}, not {}: refused",
                socket.display(),
                derived.display()
            ));
        }
        let meta = std::fs::symlink_metadata(&socket)
            .map_err(|e| format!("its socket {}: {e}", socket.display()))?;
        if !meta.file_type().is_socket() || meta.uid() != paths::uid() {
            return Err(format!(
                "{} is not a socket of this user: refused",
                socket.display()
            ));
        }
        Ok(())
    }

    fn dial(&self, info: &DaemonInfo, web: bool) -> BoxFuture<'static, Result<Dialled, DialError>> {
        let socket = PathBuf::from(&info.socket);
        Box::pin(async move {
            let token = paths::read_operator_token(&socket)
                .map_err(|e| DialError::Refused(format!("its token file: {e:#}")))?
                .ok_or_else(|| DialError::Refused("its token file is missing".into()))?;
            let stream =
                tokio::time::timeout(DIAL_WITHIN, tokio::net::UnixStream::connect(&socket))
                    .await
                    .map_err(|_| DialError::NoAnswer("it did not take the connection".into()))?
                    .map_err(|e| DialError::NoAnswer(format!("connecting: {e}")))?;
            let (read, write) = stream.into_split();
            say_hello(Box::pin(read), Box::pin(write), token, web).await
        })
    }
}

/// `hello` as the operator on a fresh connection to a daemon, within
/// `DIAL_WITHIN`.
pub(crate) async fn say_hello(
    read: Pin<Box<dyn AsyncRead + Send>>,
    mut writer: Writer,
    token: String,
    web: bool,
) -> Result<Dialled, DialError> {
    let mut lines: Lines = BufReader::with_capacity(64 * 1024, read);
    let hello = Request {
        id: 0,
        call: Call::Hello(Hello {
            role: Role::Operator,
            agent_id: None,
            token: Some(token),
            client: "web".into(),
            protocol: PROTOCOL_VERSION,
            web,
            workspace: None,
        }),
    };
    let answered = tokio::time::timeout(DIAL_WITHIN, async {
        let mut line = serde_json::to_vec(&hello).map_err(|e| e.to_string())?;
        line.push(b'\n');
        writer.write_all(&line).await.map_err(|e| e.to_string())?;
        let mut reply = String::new();
        match lines.read_line(&mut reply).await {
            Ok(0) => Err("it closed the connection".to_string()),
            Ok(_) => Ok(reply),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|_| {
        DialError::NoAnswer(format!(
            "it did not answer within {} s",
            DIAL_WITHIN.as_secs()
        ))
    })?
    .map_err(DialError::NoAnswer)?;
    let frame: ServerFrame = serde_json::from_str(&answered)
        .map_err(|_| DialError::NoAnswer("its answer was not a frame".into()))?;
    if let Some(error) = frame.error {
        return Err(match error.code.as_str() {
            "protocol" => DialError::Protocol(error.message),
            "unauthorized" => DialError::Refused(error.message),
            _ => DialError::NoAnswer(error.message),
        });
    }
    let hello: HelloInfo = frame
        .result
        .and_then(|result| serde_json::from_value(result).ok())
        .ok_or_else(|| DialError::NoAnswer("its answer to hello was empty".into()))?;
    Ok(Dialled {
        lines,
        writer,
        hello,
    })
}

/// One line from a daemon, at most `limit` bytes before its `\n`: `None` at
/// its end (or past the limit, which no daemon sends).
///
/// Safe to cancel: what a call read before it was cancelled stays in `buf`
/// and the next call goes on from there, the limit counting all of it. The
/// caller clears `buf` once it consumed the line.
pub(crate) async fn read_line(lines: &mut Lines, buf: &mut Vec<u8>, limit: usize) -> Option<()> {
    if buf.len() > limit {
        return None;
    }
    let room = (limit + 1 - buf.len()) as u64;
    let mut limited = (&mut *lines).take(room);
    match limited.read_until(b'\n', buf).await {
        Ok(0) | Err(_) => None,
        // Past the limit, or the end in the middle of a line.
        Ok(_) if buf.last() != Some(&b'\n') => None,
        Ok(_) => {
            buf.pop();
            Some(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_line_read_across_a_cancelled_read_is_whole() {
        let (mut daemon, ours) = tokio::io::duplex(64);
        let mut lines: Lines = BufReader::new(Box::pin(ours));
        let mut buf = Vec::new();
        daemon.write_all(br#"{"id":7,"#).await.unwrap();
        // Another branch wins while half the line is in.
        let cancelled = tokio::time::timeout(
            Duration::from_millis(20),
            read_line(&mut lines, &mut buf, 1024),
        )
        .await;
        assert!(cancelled.is_err());
        daemon
            .write_all(b"\"result\":{\"pong\":true}}\n")
            .await
            .unwrap();
        assert!(read_line(&mut lines, &mut buf, 1024).await.is_some());
        assert_eq!(buf, br#"{"id":7,"result":{"pong":true}}"#);
        buf.clear();
        // The limit counts what came before the cancellation too.
        daemon.write_all(&[b'x'; 20]).await.unwrap();
        let _ = tokio::time::timeout(
            Duration::from_millis(20),
            read_line(&mut lines, &mut buf, 30),
        )
        .await;
        daemon.write_all(&[b'x'; 20]).await.unwrap();
        daemon.write_all(b"\n").await.unwrap();
        assert!(read_line(&mut lines, &mut buf, 30).await.is_none());
    }
}
#[cfg(test)]
mod reviewer_tests {
    use super::*;

    #[tokio::test]
    async fn reviewer_a_daemon_frame_survives_an_interrupted_read() {
        let (reader, mut writer) = tokio::io::duplex(128);
        let mut lines: Lines = BufReader::new(Box::pin(reader));
        let mut buf = Vec::new();
        writer.write_all(b"{\"id\":7,").await.unwrap();
        // page::Conn::committed and hub::watching cancel this future when
        // another branch of their select! is ready, then call it again.
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            read_line(&mut lines, &mut buf, 1024)
        )
        .await
        .is_err());
        assert_eq!(buf, b"{\"id\":7,");
        writer
            .write_all(b"\"result\":{\"pong\":true}}\n")
            .await
            .unwrap();
        read_line(&mut lines, &mut buf, 1024).await.unwrap();
        assert_eq!(buf, b"{\"id\":7,\"result\":{\"pong\":true}}");
    }
}
