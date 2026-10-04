//! One connection to the dashboard's port: its HTTP request, answered or
//! upgraded to a WebSocket (`upgrade`), and what a WebSocket of the
//! dashboard is held to. What it then carries is `page`'s.
//!
//! Before `hello` succeeded a connection has 5 s, messages of at most
//! 4 KiB, and only `ping`, `web_pair` and `hello` do anything. The server
//! pings every 20 s and closes a connection silent for 40 s.

use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::WebSocketStream;

use super::http::{self, Answer};

/// A WebSocket must authenticate within this.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest message before `hello` succeeded.
pub(crate) const MAX_UNAUTHENTICATED_MESSAGE: usize = 4 * 1024;
/// How often the server pings.
pub(crate) const PING_EVERY: Duration = Duration::from_secs(20);
/// A connection that sent nothing for this long is closed.
pub(crate) const SILENT_FOR: Duration = Duration::from_secs(40);
/// How long a closed connection waits for the browser to close too.
pub(crate) const LINGER: Duration = Duration::from_secs(1);

/// The HTTP request of an accepted connection: a file, a refusal, or the
/// upgrade to a WebSocket (`host`: the only name answered). `None` when it
/// was not an upgrade, or the server closed it meanwhile.
pub(crate) async fn upgrade(
    mut stream: TcpStream,
    host: &str,
    close: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<WebSocketStream<TcpStream>> {
    let head = tokio::select! {
        head = http::read_head(&mut stream) => head,
        _ = close.changed() => return None,
    };
    let (head, rest) = match head {
        Ok(head) => head,
        Err(http::HeadError::Closed) => return None,
        Err(http::HeadError::Bad) => {
            http::respond(&mut stream, &Answer::Status(400), host).await;
            return None;
        }
    };
    let key = match http::route(&head, host) {
        Answer::Upgrade(key) => key,
        answer => {
            http::respond(&mut stream, &answer, host).await;
            return None;
        }
    };
    stream
        .write_all(&http::switching_protocols(&key))
        .await
        .ok()?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(crew_core::MAX_FRAME_BYTES))
        .max_frame_size(Some(crew_core::MAX_FRAME_BYTES));
    Some(WebSocketStream::from_partially_read(stream, rest, Role::Server, Some(config)).await)
}

/// Aborts its task when dropped: a connection's writer does not outlive the
/// connection's task, however that ends.
pub(crate) struct AbortOnDrop<T>(pub(crate) JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
