//! The daemon's servers: the Unix socket, and the protocol both transports
//! speak.
//!
//! Access control on the socket has three layers: the socket lives in a
//! `0700` directory and is `0600`; the peer's uid is checked on accept; and a
//! connection that claims to be an agent must present that agent's token.
//! The daemon stamps the sender on everything, so an agent cannot speak as
//! another agent. The web dashboard's WebSocket (`crate::web`) carries the
//! same frames through the same [`Session`], with its own checks in front.

use crew_core::paths::uid;
use crew_core::rpc::features::{Feature, Id};
use crew_core::rpc::{Call, Push, Request, Role, RpcError, ServerFrame, WorkspaceSummary};
use crew_core::util::ct_eq;
use crew_core::MAX_FRAME_BYTES;
use serde_json::Value;
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::Daemon;

/// Who a connection has authenticated as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Who {
    Operator,
    Agent(String),
}

impl Who {
    /// The sender id stamped on messages from this connection.
    pub fn sender(&self) -> &str {
        match self {
            Who::Operator => crew_core::OPERATOR_ID,
            Who::Agent(id) => id,
        }
    }

    /// Who this is, for messages: "the operator" or "agent '<id>'".
    pub fn label(&self) -> String {
        match self {
            Who::Operator => "the operator".to_string(),
            Who::Agent(id) => format!("agent '{id}'"),
        }
    }
}

pub(crate) struct Conn {
    pub id: u64,
    pub who: Option<Who>,
    pub feature_context: Option<super::features::ConnCtx>,
    pub negotiated: Option<super::features::Negotiated>,
    pub representation_started: bool,
    pub feature_after_reply: Vec<super::features::PendingWatch>,
    pub feature_tasks: HashMap<Id, (Feature, JoinHandle<()>)>,
    /// Its `hello` said `web`: the dashboard server opened it for a browser,
    /// which may not do what W7 keeps from a web session.
    pub web: bool,
    /// This connection is the dashboard server's watch (`web_attach`).
    pub web_attached: bool,
    pub tx: mpsc::Sender<ServerFrame>,
    /// A subscription to start once the pending response has been queued:
    /// from now (`subscribe`), or from a receiver taken before a snapshot was
    /// captured (the barrier).
    pub subscribe_after_reply: Option<broadcast::Receiver<Push>>,
    /// The forwarder in `stream_task` streams the daemon's pushes.
    pub subscribed: bool,
    /// This connection is an agent's delivery channel.
    pub listening: Option<String>,
    /// Forwarder for pushes or terminal output on this connection.
    pub stream_task: Option<JoinHandle<()>>,
    /// A summary watch to start once the pending response has been queued.
    pub summary_after_reply: Option<tokio::sync::watch::Receiver<WorkspaceSummary>>,
    /// Forwarder of the summary's changes (`summary {watch: true}`).
    pub summary_task: Option<JoinHandle<()>>,
    /// Agent whose terminal this connection is attached to.
    pub attached: Option<String>,
    /// Test hook: how long to wait between capturing a snapshot and queueing
    /// its reply, the window the barrier closes.
    #[cfg(test)]
    pub after_capture: Option<std::time::Duration>,
}

/// One connection's protocol state, whatever carries its frames.
pub(crate) struct Session {
    daemon: Daemon,
    conn: Conn,
}

impl Session {
    /// A new connection, and the frames for it (responses and pushes) in the
    /// order they must be written.
    pub fn new(daemon: &Daemon) -> (Self, mpsc::Receiver<ServerFrame>) {
        let (tx, rx) = mpsc::channel::<ServerFrame>(1024);
        let conn = Conn {
            id: daemon
                .next_conn
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            who: None,
            feature_context: None,
            negotiated: None,
            representation_started: false,
            feature_after_reply: Vec::new(),
            feature_tasks: HashMap::new(),
            web: false,
            web_attached: false,
            tx,
            subscribe_after_reply: None,
            subscribed: false,
            listening: None,
            stream_task: None,
            summary_after_reply: None,
            summary_task: None,
            attached: None,
            #[cfg(test)]
            after_capture: None,
        };
        let session = Self {
            daemon: daemon.clone(),
            conn,
        };
        (session, rx)
    }

    /// Handle one request as received (one line, one text message): parse,
    /// dispatch, queue the response, start the subscription after the reply.
    /// A `snapshot` on a subscribed connection is a barrier in the push
    /// stream. Malformed JSON is answered with `invalid`. `false`: the writer
    /// is gone, stop reading.
    pub async fn request(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return true;
        }
        let frame = match serde_json::from_str::<Request>(text) {
            Ok(request) => {
                let id = request.id;
                // Input for an attached terminal is not acknowledged.
                let silent = matches!(
                    request.call,
                    Call::AttachInput { .. } | Call::AttachResize { .. }
                );
                #[cfg(test)]
                let snapshot = matches!(request.call, Call::Snapshot { .. });
                let result = self.daemon.dispatch(&mut self.conn, request.call).await;
                #[cfg(test)]
                if let (true, Some(wait)) = (snapshot, self.conn.after_capture) {
                    tokio::time::sleep(wait).await;
                }
                match result {
                    Ok(_) if silent => {
                        self.after_reply();
                        return true;
                    }
                    Ok(result) => ServerFrame::ok(id, result),
                    Err(error) => ServerFrame::err(id, error),
                }
            }
            Err(e) => {
                // Malformed request: answer with the id if one can be salvaged.
                let value = serde_json::from_str::<Value>(text).ok();
                let id = value
                    .as_ref()
                    .and_then(|v| v.get("id").and_then(Value::as_u64))
                    .unwrap_or(0);
                let error = value
                    .as_ref()
                    .and_then(crew_core::rpc::feature_decode_error)
                    .unwrap_or_else(|| RpcError::invalid(format!("malformed request: {e}")));
                ServerFrame::err(id, error)
            }
        };
        if self.conn.tx.send(frame).await.is_err() {
            return false;
        }
        self.after_reply();
        true
    }

    /// Start the subscription a request asked for, now that its reply is
    /// queued: the reply precedes every push of it.
    fn after_reply(&mut self) {
        for pending in std::mem::take(&mut self.conn.feature_after_reply) {
            let id = pending.id.clone();
            let feature = pending.feature;
            let task = self.conn.spawn(pending.forward(self.conn.tx.clone()));
            self.conn.feature_tasks.insert(id, (feature, task));
        }
        if let Some(rx) = self.conn.subscribe_after_reply.take() {
            self.daemon.start_subscription(&mut self.conn, rx);
        }
        if let Some(rx) = self.conn.summary_after_reply.take() {
            self.daemon.start_summary_watch(&mut self.conn, rx);
        }
    }

    /// Queue a frame that answers no request (an oversized message).
    pub async fn send(&self, frame: ServerFrame) -> bool {
        self.conn.tx.send(frame).await.is_ok()
    }

    #[cfg(test)]
    pub fn authenticated(&self) -> bool {
        self.conn.who.is_some()
    }

    #[cfg(test)]
    pub fn delay_after_capture(&mut self, wait: std::time::Duration) {
        self.conn.after_capture = Some(wait);
    }
}

impl Drop for Session {
    /// The connection ended, or the task that served it was aborted: its
    /// stream task stops, its listener slot is released.
    fn drop(&mut self) {
        for (_, (_, task)) in self.conn.feature_tasks.drain() {
            task.abort();
        }
        if let Some(context) = &self.conn.feature_context {
            context.cancellation.cancel();
            self.daemon
                .feature_services
                .connection_closed(&context.connection_epoch);
        }
        if let Some(task) = self.conn.stream_task.take() {
            task.abort();
        }
        if let Some(task) = self.conn.summary_task.take() {
            task.abort();
        }
        if let Some(agent) = self.conn.listening.take() {
            self.daemon.listener_gone(&agent, self.conn.id);
        }
        if self.conn.web_attached {
            self.daemon.web_detached(self.conn.id);
        }
    }
}

impl Conn {
    pub(crate) fn feature_watch_count(&mut self, feature: Feature) -> usize {
        self.feature_tasks
            .retain(|_, (_, task)| !task.is_finished());
        self.feature_tasks
            .values()
            .filter(|(kind, _)| *kind == feature)
            .count()
            + self
                .feature_after_reply
                .iter()
                .filter(|p| p.feature == feature)
                .count()
    }

    pub(crate) fn feature_watch_exists(&self, id: &Id) -> bool {
        self.feature_tasks.contains_key(id) || self.feature_after_reply.iter().any(|p| &p.id == id)
    }

    pub(crate) fn stop_feature_watch(&mut self, feature: Feature, id: &Id) {
        if self
            .feature_tasks
            .get(id)
            .is_some_and(|(kind, _)| *kind == feature)
        {
            if let Some((_, task)) = self.feature_tasks.remove(id) {
                task.abort();
            }
        }
        self.feature_after_reply
            .retain(|pending| pending.feature != feature || &pending.id != id);
    }

    /// Spawn a task of this connection (a forwarder).
    pub fn spawn<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(task)
    }
}

pub(crate) async fn accept_loop(daemon: Daemon, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    handle(daemon, stream).await;
                });
            }
            Err(e) => {
                tracing::error!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle(daemon: Daemon, stream: UnixStream) {
    // Only the user who owns the daemon may talk to it.
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == uid() => {}
        Ok(cred) => {
            tracing::warn!("rejected connection from uid {}", cred.uid());
            return;
        }
        Err(e) => {
            tracing::warn!("rejected connection without credentials: {e}");
            return;
        }
    }
    let (read, mut write) = stream.into_split();
    let (mut session, mut rx) = Session::new(&daemon);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let Ok(mut line) = serde_json::to_vec(&frame) else {
                continue;
            };
            line.push(b'\n');
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    });

    let mut reader = BufReader::new(read);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        // Bound the line length so a client cannot make the daemon buffer
        // without limit.
        let mut limited = (&mut reader).take(MAX_FRAME_BYTES as u64 + 1);
        match limited.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.len() > MAX_FRAME_BYTES {
            let _ = session
                .send(ServerFrame::err(
                    0,
                    RpcError::too_large(format!("request exceeds {MAX_FRAME_BYTES} bytes")),
                ))
                .await;
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        if !session.request(&line).await {
            break;
        }
    }
    drop(session);
    let _ = writer.await;
}

impl Daemon {
    /// Authenticate a connection.
    pub(crate) fn authenticate(
        &self,
        role: Role,
        agent_id: Option<String>,
        token: Option<String>,
    ) -> Result<Who, RpcError> {
        match role {
            Role::Operator => match token {
                None => Err(RpcError::unauthorized("operator token required")),
                Some(token) if ct_eq(&self.operator_token, &token) => Ok(Who::Operator),
                Some(_) => Err(RpcError::unauthorized(
                    "operator token does not match this daemon (it may have restarted; reconnect)",
                )),
            },
            Role::Agent => {
                let id = agent_id.ok_or_else(|| RpcError::unauthorized("agent_id is required"))?;
                let token = token.ok_or_else(|| RpcError::unauthorized("token is required"))?;
                let model = self.model();
                let expected = model
                    .tokens
                    .get(&id)
                    .ok_or_else(|| RpcError::unauthorized("unknown agent or bad token"))?;
                if !ct_eq(expected, &token) {
                    return Err(RpcError::unauthorized("unknown agent or bad token"));
                }
                if model.agent(&id).is_none() {
                    return Err(RpcError::unauthorized("unknown agent or bad token"));
                }
                Ok(Who::Agent(id))
            }
        }
    }

    /// Stop the connection's forwarder and wait until its task has ended,
    /// so nothing of the old stream can be queued after this returns.
    pub(crate) async fn stop_stream(&self, conn: &mut Conn) {
        conn.subscribed = false;
        if let Some(task) = conn.stream_task.take() {
            task.abort();
            let _ = task.await;
        }
    }

    /// Stream the pushes `rx` receives to a connection, in place of whatever
    /// it streamed before.
    pub(crate) fn start_subscription(&self, conn: &mut Conn, mut rx: broadcast::Receiver<Push>) {
        if let Some(task) = conn.stream_task.take() {
            task.abort();
        }
        conn.subscribed = true;
        let tx = conn.tx.clone();
        conn.stream_task = Some(conn.spawn(async move {
            loop {
                let frame = match rx.recv().await {
                    // P1 enables no features. P2 selects this projection or the
                    // negotiated stream per connection; never leak new variants.
                    Ok(push) => match push.legacy_projection() {
                        Some(push) => ServerFrame::push(push),
                        None => continue,
                    },
                    // The subscriber fell behind; tell it to refetch.
                    Err(broadcast::error::RecvError::Lagged(_)) => ServerFrame::push(Push::Resync),
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::rpc::Snapshot;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    async fn daemon() -> (tempfile::TempDir, super::super::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "working", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        running.daemon.model().agents.push(agent);
        (temp, running)
    }

    fn request(id: u64, method: &str, params: Value) -> String {
        serde_json::json!({"id": id, "method": method, "params": params}).to_string()
    }

    async fn operator(daemon: &Daemon) -> (Session, mpsc::Receiver<ServerFrame>) {
        let (mut session, mut rx) = Session::new(daemon);
        let hello = serde_json::json!({"role": "operator", "token": daemon.operator_token,
            "client": "test", "protocol": crew_core::rpc::PROTOCOL_VERSION});
        assert!(session.request(&request(1, "hello", hello)).await);
        let reply = rx.recv().await.unwrap();
        assert!(reply.result.is_some(), "{reply:?}");
        (session, rx)
    }

    #[tokio::test]
    async fn feature_contract_legacy_subscription_strips_populated_fields() {
        let (_temp, running) = daemon().await;
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../crew-core/tests/fixtures/features/legacy-v6.json"
        ))
        .unwrap();
        let mut task: crew_core::Task =
            serde_json::from_str(fixture["task"].as_str().unwrap()).unwrap();
        task.epic = Some("epic-1".into());
        running.daemon.model().tasks.push(task.clone());
        let (mut session, mut rx) = operator(&running.daemon).await;
        assert!(
            session
                .request(&request(2, "subscribe", serde_json::json!({})))
                .await
        );
        assert!(rx.recv().await.unwrap().result.is_some());
        running.daemon.push(Push::Task(Box::new(task)));
        let frame = rx.recv().await.unwrap();
        assert_eq!(
            serde_json::to_string(&frame.push.unwrap()).unwrap(),
            fixture["pushes"][0]
        );
        let info = crew_core::rpc::SessionInfo {
            workspace_run_id: Some("run-1".into()),
            ..Default::default()
        };
        running.daemon.push(Push::Session(Box::new(info)));
        let frame = rx.recv().await.unwrap();
        assert_eq!(
            serde_json::to_string(&frame.push.unwrap()).unwrap(),
            fixture["pushes"][1]
        );
        assert!(
            session
                .request(&request(
                    3,
                    "snapshot",
                    serde_json::json!({"messages":0,"events":0})
                ))
                .await
        );
        let result = rx.recv().await.unwrap().result.unwrap();
        assert!(result.get("epics").is_none());
        assert!(result["session"].get("workspace_run_id").is_none());
        assert!(result["tasks"][0].get("epic").is_none());
        assert_eq!(
            running.daemon.model().tasks[0].epic.as_deref(),
            Some("epic-1")
        );
    }

    #[tokio::test]
    async fn feature_contract_definitions_do_not_enable_backends() {
        let (_temp, running) = daemon().await;
        let (mut session, mut rx) = operator(&running.daemon).await;
        let calls: Vec<Value> = serde_json::from_str(include_str!(
            "../../../crew-core/tests/fixtures/features/requests-v1.json"
        ))
        .unwrap();
        for call in calls {
            assert!(session.request(&call.to_string()).await);
            let frame = rx.recv().await.unwrap();
            assert_eq!(frame.error.unwrap().code, "unsupported_feature");
        }
        assert!(running.daemon.model().tasks.is_empty());
        assert!(
            session
                .request(&request(
                    90,
                    "features_negotiate",
                    serde_json::json!({"versions":false})
                ))
                .await
        );
        assert_eq!(
            rx.recv().await.unwrap().error.unwrap().code,
            "invalid_feature_request"
        );
        assert!(
            session
                .request(&request(91, "not_a_method", serde_json::json!({})))
                .await
        );
        assert_eq!(rx.recv().await.unwrap().error.unwrap().code, "invalid");
    }

    /// `n` of an agent's activity `n=<n>`.
    fn n_of(activity: &str) -> u64 {
        activity
            .strip_prefix("n=")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    /// What a frame says about agent `a`: a push of it, or a snapshot reply.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Seen {
        Push(u64),
        Reply(u64),
        Resync,
        Other,
    }

    fn seen(frame: &ServerFrame) -> Seen {
        if let Some(push) = &frame.push {
            return match push {
                Push::Agent(agent) if agent.id == "a" => Seen::Push(n_of(&agent.activity)),
                Push::Resync => Seen::Resync,
                _ => Seen::Other,
            };
        }
        match frame
            .result
            .as_ref()
            .and_then(|r| serde_json::from_value::<Snapshot>(r.clone()).ok())
        {
            Some(snap) => Seen::Reply(
                snap.agents
                    .iter()
                    .find(|a| a.id == "a")
                    .map(|a| n_of(&a.activity))
                    .unwrap_or(0),
            ),
            None => Seen::Other,
        }
    }

    /// Changes agent `a` to `n=1` … `n=last`. `persist`: through
    /// `update_agent` (stored, then pushed); otherwise straight into the
    /// model and the push channel, for runs too long to store.
    fn churn(daemon: &Daemon, last: u64, persist: bool) -> tokio::task::JoinHandle<()> {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            for n in 1..=last {
                if persist {
                    daemon.update_agent("a", |a| a.activity = format!("n={n}"));
                } else {
                    let agent = {
                        let mut model = daemon.model();
                        let agent = model.agents.iter_mut().find(|a| a.id == "a").unwrap();
                        agent.activity = format!("n={n}");
                        agent.clone()
                    };
                    daemon.push(Push::Agent(Box::new(agent)));
                }
                if n % 8 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        })
    }

    /// The page's rule: discard pushes while a snapshot is in flight (one at
    /// most), replace on the reply, apply what follows; a `resync` after
    /// the reply asks again. Runs until `done` and the stream is quiet;
    /// returns the frames seen and the state the page ends in.
    async fn page(
        session: &mut Session,
        rx: &mut mpsc::UnboundedReceiver<ServerFrame>,
        done: Arc<AtomicBool>,
    ) -> (Vec<Seen>, u64) {
        let mut frames = Vec::new();
        let mut state = 0;
        let mut id = 10;
        let mut in_flight = false;
        let mut asked = 0;
        loop {
            if !in_flight && (asked < 20 || done.load(Ordering::SeqCst) && asked < 21) {
                id += 1;
                asked += 1;
                in_flight = true;
                let params = serde_json::json!({"messages": 0, "events": 0});
                assert!(session.request(&request(id, "snapshot", params)).await);
            }
            let frame = match tokio::time::timeout(Duration::from_millis(25), rx.recv()).await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(_) if done.load(Ordering::SeqCst) && !in_flight => break,
                Err(_) => continue,
            };
            let what = seen(&frame);
            frames.push(what);
            match what {
                Seen::Reply(n) => {
                    in_flight = false;
                    state = n;
                }
                Seen::Push(n) if !in_flight => state = n,
                Seen::Resync if !in_flight => {
                    // Ask again at once.
                    asked = asked.min(19);
                }
                _ => {}
            }
        }
        (frames, state)
    }

    /// The connection's frames as its writer takes them: always read, so a
    /// request never waits for room in the queue.
    /// `stall`: it reads nothing for that long first, as a browser that
    /// stopped reading would.
    fn pump(
        mut rx: mpsc::Receiver<ServerFrame>,
        stall: Option<Duration>,
    ) -> mpsc::UnboundedReceiver<ServerFrame> {
        let (tx, out) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            if let Some(stall) = stall {
                tokio::time::sleep(stall).await;
            }
            while let Some(frame) = rx.recv().await {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        out
    }

    /// Checks 1 and 2 of notes/web-design.md §tests on one run's frames.
    fn check_order(frames: &[Seen], last: u64) {
        let mut pushes: Vec<u64> = Vec::new();
        for frame in frames {
            match *frame {
                Seen::Reply(n) => {
                    // 1: nothing before a reply is newer than it.
                    assert!(
                        pushes.iter().all(|p| *p <= n),
                        "a push newer than the snapshot came before it: {pushes:?} then reply {n}"
                    );
                    pushes.clear();
                }
                Seen::Push(n) => {
                    // 2: in order between two replies.
                    assert!(
                        pushes.last().is_none_or(|p| *p < n),
                        "pushes out of order: {pushes:?} then {n}"
                    );
                    pushes.push(n);
                }
                _ => {}
            }
        }
        let final_n = frames
            .iter()
            .rev()
            .find_map(|f| match f {
                Seen::Push(n) | Seen::Reply(n) => Some(*n),
                _ => None,
            })
            .unwrap();
        assert_eq!(final_n, last, "the run did not end with the final state");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn snapshot_is_a_barrier() {
        let (_temp, running) = daemon().await;
        let daemon = running.daemon.clone();
        for run in 0..200 {
            let (mut session, mut rx) = operator(&daemon).await;
            session.delay_after_capture(Duration::from_millis(1));
            assert!(session.request(&request(2, "subscribe", Value::Null)).await);
            assert!(rx.recv().await.unwrap().result.is_some());
            let mut rx = pump(rx, None);
            let start = n_of(&daemon.agent("a").unwrap().activity);
            let last = start + 60;
            let done = Arc::new(AtomicBool::new(false));
            let churner = {
                let (daemon, done) = (daemon.clone(), done.clone());
                tokio::spawn(async move {
                    for n in start + 1..=last {
                        daemon.update_agent("a", |a| a.activity = format!("n={n}"));
                        if n % 4 == 0 {
                            tokio::task::yield_now().await;
                        }
                    }
                    done.store(true, Ordering::SeqCst);
                })
            };
            let (frames, state) = page(&mut session, &mut rx, done).await;
            churner.await.unwrap();
            check_order(&frames, last);
            // 3: the page's rule ends in the daemon's final state.
            assert_eq!(state, last, "run {run}: the page ended stale: {frames:?}");
            drop(session);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_lagging_subscriber_resyncs_and_converges() {
        let (_temp, running) = daemon().await;
        let daemon = running.daemon.clone();
        let (mut session, mut rx) = operator(&daemon).await;
        session.delay_after_capture(Duration::from_millis(1));
        assert!(session.request(&request(2, "subscribe", Value::Null)).await);
        assert!(rx.recv().await.unwrap().result.is_some());
        let mut rx = pump(rx, Some(Duration::from_millis(400)));
        // More than the push channel and the connection's queue hold, while
        // the page does not read.
        let last = 8000;
        let done = Arc::new(AtomicBool::new(false));
        let churner = {
            let (daemon, done) = (daemon.clone(), done.clone());
            tokio::spawn(async move {
                churn(&daemon, last, false).await.unwrap();
                done.store(true, Ordering::SeqCst);
            })
        };
        let (frames, state) = page(&mut session, &mut rx, done).await;
        churner.await.unwrap();
        assert!(
            frames.contains(&Seen::Resync),
            "the subscriber never lagged"
        );
        check_order(&frames, last);
        assert_eq!(state, last);
        drop(session);
    }

    #[tokio::test]
    async fn the_socket_behaviours_hold_through_the_session() {
        let (_temp, running) = daemon().await;
        let daemon = running.daemon.clone();
        let (mut session, mut rx) = Session::new(&daemon);
        // Before hello: unauthorized.
        assert!(
            session
                .request(&request(3, "snapshot", serde_json::json!({})))
                .await
        );
        let reply = rx.recv().await.unwrap();
        assert_eq!(reply.id, Some(3));
        assert_eq!(reply.error.unwrap().code, "unauthorized");
        // Malformed: answered with the id that could be salvaged.
        assert!(session.request(r#"{"id": 9, "method": "no_such"}"#).await);
        let reply = rx.recv().await.unwrap();
        assert_eq!(reply.id, Some(9));
        assert_eq!(reply.error.unwrap().code, "invalid");
        assert!(session.request("not json").await);
        assert_eq!(rx.recv().await.unwrap().id, Some(0));
        drop(session);
        // Subscribing: the reply precedes every push.
        let (mut session, mut rx) = operator(&daemon).await;
        assert!(session.authenticated());
        daemon.update_agent("a", |a| a.activity = "before".into());
        assert!(session.request(&request(2, "subscribe", Value::Null)).await);
        daemon.update_agent("a", |a| a.activity = "after".into());
        let first = rx.recv().await.unwrap();
        assert_eq!(first.id, Some(2));
        let next = rx.recv().await.unwrap();
        assert!(matches!(next.push, Some(Push::Agent(_))), "{next:?}");
        drop(session);
    }

    /// A `hello` that is refused, on a connection of its own.
    async fn refused_hello(daemon: &Daemon, hello: Value) -> RpcError {
        let (mut session, mut rx) = Session::new(daemon);
        assert!(session.request(&request(1, "hello", hello)).await);
        let reply = rx.recv().await.unwrap();
        assert!(!session.authenticated());
        reply.error.expect("the hello was refused")
    }

    #[tokio::test]
    async fn a_hello_in_another_protocol_is_told_who_the_daemon_is_and_an_agent_is_named() {
        use crew_core::rpc::PROTOCOL_VERSION;
        let (_temp, running) = daemon().await;
        let daemon = running.daemon.clone();
        daemon.model().tokens.insert("a".into(), "token-a".into());
        let other = PROTOCOL_VERSION + 1;
        let warnings = || {
            daemon
                .store
                .recent_events(None, 100)
                .unwrap()
                .into_iter()
                .filter(|e| e.event_type == "agent.protocol_mismatch")
                .collect::<Vec<_>>()
        };
        // Whoever asks is told the daemon's build; the message stays what
        // clients of every protocol so far read.
        let refusal = refused_hello(
            &daemon,
            serde_json::json!({"role": "operator", "token": daemon.operator_token,
                "client": "cli", "protocol": other}),
        )
        .await;
        assert_eq!(refusal.code, "protocol");
        assert_eq!(
            refusal.message,
            format!("client speaks protocol {other}, daemon speaks {PROTOCOL_VERSION}")
        );
        assert_eq!(
            refusal.data,
            Some(serde_json::json!({
                "version": super::super::VERSION,
                "protocol": PROTOCOL_VERSION,
                "workspace": daemon.workspace.to_string_lossy(),
            }))
        );
        // An agent is named only with its own token.
        let agent = |token: &str, client: &str| {
            serde_json::json!({"role": "agent", "agent_id": "a", "token": token,
                "client": client, "protocol": other})
        };
        refused_hello(&daemon, agent("not-a's", "mcp")).await;
        assert!(warnings().is_empty());
        refused_hello(&daemon, agent("token-a", "mcp")).await;
        let said = warnings();
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0].agent_id.as_deref(), Some("a"));
        assert_eq!(said[0].level, crew_core::EventLevel::Warn);
        assert_eq!(
            said[0].message,
            format!(
                "a's crew tools speak protocol {other}, this daemon speaks {PROTOCOL_VERSION}: \
restart the agent"
            )
        );
        // At most once a minute.
        refused_hello(&daemon, agent("token-a", "hook")).await;
        assert_eq!(warnings().len(), 1);
        daemon.model().rt("a").protocol_warned_at -= 61_000;
        refused_hello(&daemon, agent("token-a", "hook")).await;
        assert_eq!(warnings().len(), 2);
    }
}
