use super::*;
use crate::daemon::server::Session;
use crate::message_bus::NewMessage;
use crew_core::rpc::PROTOCOL_VERSION;
use crew_core::{Agent, MessageType, Task};
use serde_json::{json, Value};
use tokio::sync::mpsc;

async fn daemon() -> (tempfile::TempDir, crate::daemon::Running) {
    let temp = tempfile::tempdir().unwrap();
    let running = crate::daemon::start(crate::daemon::DaemonOptions {
        workspace: temp.path().to_path_buf(),
        crew_bin: std::env::current_exe().unwrap(),
    })
    .await
    .unwrap();
    (temp, running)
}

fn agent(id: &str, status: &str) -> Agent {
    serde_json::from_value(json!({
        "id": id, "name": id, "role": "work", "status": status, "runtime": "shell",
        "workdir": "/tmp", "backend": "pty", "last_activity_at": 0
    }))
    .unwrap()
}

fn task(id: &str, status: &str) -> Task {
    serde_json::from_value(json!({
        "id": id, "title": id, "description": "", "status": status, "created_at": 0
    }))
    .unwrap()
}

/// A connection on the socket and its frames, after a `hello`.
struct Peer {
    session: Session,
    frames: mpsc::Receiver<ServerFrame>,
    next: u64,
    /// Pushes read while waiting for replies.
    pushes: Vec<Push>,
}

impl Peer {
    async fn hello(daemon: &Daemon, params: Value) -> (Self, ServerFrame) {
        let (session, frames) = Session::new(daemon);
        let mut peer = Self {
            session,
            frames,
            next: 1,
            pushes: Vec::new(),
        };
        let reply = peer.call("hello", params).await;
        (peer, reply)
    }

    async fn operator(daemon: &Daemon, web: bool) -> Self {
        let (peer, reply) = Self::hello(
            daemon,
            json!({"role": "operator", "token": daemon.operator_token, "client": "test",
                "protocol": PROTOCOL_VERSION, "web": web}),
        )
        .await;
        assert!(reply.result.is_some(), "{reply:?}");
        peer
    }

    async fn call(&mut self, method: &str, params: Value) -> ServerFrame {
        let id = self.next;
        self.next += 1;
        let text = json!({"id": id, "method": method, "params": params}).to_string();
        assert!(self.session.request(&text).await);
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
                .await
                .expect("a reply within 5 s")
                .expect("the connection is open");
            if frame.id == Some(id) && frame.push.is_none() {
                return frame;
            }
            if let Some(push) = frame.push {
                self.pushes.push(push);
            }
        }
    }

    /// The error code of a call.
    async fn refused(&mut self, method: &str, params: Value) -> String {
        let frame = self.call(method, params).await;
        frame.error.map(|e| e.code).unwrap_or_default()
    }

    /// The `summary` pushes that arrive within `wait`.
    async fn summaries(&mut self, wait: Duration) -> Vec<WorkspaceSummary> {
        let deadline = tokio::time::Instant::now() + wait;
        let mut out = Vec::new();
        while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, self.frames.recv()).await {
            if let Some(Push::Summary(summary)) = frame.push {
                out.push(summary);
            }
        }
        out
    }
}

impl Peer {
    /// The `web` of each `session` push, until `n` came or a second passed.
    async fn session_webs(&mut self, n: usize) -> Vec<String> {
        let mut webs: Vec<String> = self
            .pushes
            .drain(..)
            .filter_map(|p| match p {
                Push::Session(s) => Some(s.web),
                _ => None,
            })
            .collect();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while webs.len() < n {
            match tokio::time::timeout_at(deadline, self.frames.recv()).await {
                Ok(Some(frame)) => {
                    if let Some(Push::Session(s)) = frame.push {
                        webs.push(s.web);
                    }
                }
                _ => break,
            }
        }
        webs
    }

    /// The next `summary` push, within `wait`.
    async fn next_summary(&mut self, wait: Duration) -> Option<WorkspaceSummary> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, self.frames.recv()).await {
                Ok(Some(frame)) => {
                    if let Some(Push::Summary(summary)) = frame.push {
                        return Some(summary);
                    }
                }
                _ => return None,
            }
        }
    }
}

fn events(daemon: &Daemon, kind: &str) -> Vec<crew_core::Event> {
    daemon
        .store
        .recent_events(None, 500)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == kind)
        .collect()
}

#[tokio::test]
async fn a_web_connection_is_held_to_w7_and_the_flag_grants_nothing() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let mut web = Peer::operator(&daemon, true).await;
    let team = json!({"config": {"project": "p", "objective": "", "agents": []}});
    assert_eq!(web.refused("load_team", team).await, "forbidden");
    assert_eq!(
        web.refused("web_attach", json!({"url": "http://x.localhost:1/"}))
            .await,
        "forbidden"
    );
    assert_eq!(
        web.refused("web_event", json!({"kind": "paired", "count": 1}))
            .await,
        "forbidden"
    );
    // The rest of what the operator does works.
    for (method, params) in [
        ("snapshot", json!({})),
        ("summary", json!({})),
        ("web_prefs", json!({})),
        ("tasks_list", json!(null)),
    ] {
        let frame = web.call(method, params).await;
        assert!(frame.result.is_some(), "{method}: {frame:?}");
    }
    assert!(events(&daemon, "web.joined").is_empty());

    // An agent's hello with the flag is an agent's hello: no more.
    daemon.model().agents.push(agent("a", "idle"));
    daemon
        .model()
        .tokens
        .insert("a".into(), "agent-token".into());
    let (mut as_agent, reply) = Peer::hello(
        &daemon,
        json!({"role": "agent", "agent_id": "a", "token": "agent-token", "client": "test",
            "protocol": PROTOCOL_VERSION, "web": true}),
    )
    .await;
    assert_eq!(
        reply.result.unwrap()["agent_id"],
        "a",
        "an agent, as it said"
    );
    assert_eq!(as_agent.refused("snapshot", json!({})).await, "forbidden");
    assert_eq!(as_agent.refused("summary", json!({})).await, "forbidden");
    // And with the operator's role but not its token: nothing.
    let (_, reply) = Peer::hello(
        &daemon,
        json!({"role": "operator", "token": "nope", "client": "test",
            "protocol": PROTOCOL_VERSION, "web": true}),
    )
    .await;
    assert_eq!(reply.error.unwrap().code, "unauthorized");
}

#[tokio::test]
async fn the_watch_shows_the_session_until_it_ends() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let mut listener = Peer::operator(&daemon, false).await;
    assert!(listener.call("subscribe", json!({})).await.result.is_some());
    let url = "http://c7f3.localhost:23417/";
    let mut watch = Peer::operator(&daemon, false).await;
    // Only the watch reports events.
    assert_eq!(
        watch
            .refused("web_event", json!({"kind": "paired", "count": 1}))
            .await,
        "invalid"
    );
    assert_eq!(
        watch
            .refused("web_attach", json!({"url": "file:///etc/passwd"}))
            .await,
        "invalid"
    );
    let reply = watch.call("web_attach", json!({"url": url})).await;
    assert_eq!(reply.result.unwrap()["attached"], true);
    assert_eq!(daemon.session_info().web, url);
    assert_eq!(events(&daemon, "web.joined").len(), 1);
    assert!(events(&daemon, "web.joined")[0].message.contains(url));
    for (kind, count, event, says) in [
        (
            "paired",
            3,
            "web.paired",
            "a browser was paired with the dashboard (3 sessions)",
        ),
        (
            "auth_failed",
            7,
            "web.auth_failed",
            "7 refused web logins in the last minute",
        ),
    ] {
        let reply = watch
            .call("web_event", json!({"kind": kind, "count": count}))
            .await;
        assert!(reply.result.is_some(), "{reply:?}");
        let logged = events(&daemon, event);
        assert_eq!(logged.len(), 1, "{event}");
        assert_eq!(logged[0].message, says);
    }
    assert_eq!(
        events(&daemon, "web.auth_failed")[0].level,
        crew_core::types::EventLevel::Warn
    );
    drop(watch);
    assert_eq!(daemon.session_info().web, "");
    assert_eq!(events(&daemon, "web.left").len(), 1);
    // The subscriber was told both times.
    assert_eq!(
        listener.session_webs(2).await,
        vec![url.to_string(), String::new()]
    );

    // A newer watch replaces an older one, whose end then changes nothing.
    let mut old = Peer::operator(&daemon, false).await;
    old.call("web_attach", json!({"url": "http://old.localhost:1/"}))
        .await;
    let mut new = Peer::operator(&daemon, false).await;
    new.call("web_attach", json!({"url": url})).await;
    drop(old);
    assert_eq!(daemon.session_info().web, url);
    assert_eq!(events(&daemon, "web.left").len(), 1);
}

#[tokio::test]
async fn the_summary_counts_agents_attention_questions_and_tasks() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    {
        let mut model = daemon.model();
        for (id, status) in [
            ("a", "working"),
            ("b", "working"),
            ("c", "blocked"),
            ("d", "failed"),
            ("e", "idle"),
        ] {
            model.agents.push(agent(id, status));
        }
        for (id, status) in [("t1", "done"), ("t2", "working"), ("t3", "done")] {
            model.tasks.push(task(id, status));
        }
    }
    daemon
        .bus
        .send(
            NewMessage::new("a", OPERATOR_ID, MessageType::Question, "Which port?"),
            vec![OPERATOR_ID.into()],
        )
        .unwrap();
    let mut peer = Peer::operator(&daemon, false).await;
    let reply = peer.call("summary", json!({})).await;
    let summary: WorkspaceSummary = serde_json::from_value(reply.result.unwrap()).unwrap();
    assert_eq!(
        (
            summary.agents,
            summary.working,
            summary.attention,
            summary.questions,
            summary.tasks,
            summary.tasks_done
        ),
        (5, 2, 2, 1, 3, 2),
        "{summary:?}"
    );
    assert!(summary.rev > 0);
}

#[tokio::test]
async fn a_watch_gets_each_change_at_most_every_250_ms_and_nothing_when_idle() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    daemon.model().agents.push(agent("a", "idle"));
    let mut peer = Peer::operator(&daemon, false).await;
    let first: WorkspaceSummary = serde_json::from_value(
        peer.call("summary", json!({"watch": true}))
            .await
            .result
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first.working, 0);
    // Idle: nothing.
    assert!(peer.summaries(Duration::from_millis(2000)).await.is_empty());
    // One change: pushed at once.
    daemon.update_agent("a", |a| a.status = crew_core::AgentStatus::Working);
    let pushed = peer
        .next_summary(Duration::from_millis(100))
        .await
        .expect("the first change at once");
    assert_eq!(pushed.working, 1);
    assert!(pushed.rev > first.rev);
    assert!(peer.summaries(Duration::from_millis(300)).await.is_empty());
    // A burst of 41 changes over about 450 ms: at most one push per 250 ms,
    // ending at the last state.
    for i in 0..40 {
        let status = if i % 2 == 0 { "idle" } else { "working" };
        daemon.update_agent("a", |a| a.status = status.parse().unwrap());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    daemon.update_agent("a", |a| a.status = crew_core::AgentStatus::Blocked);
    let burst = peer.summaries(Duration::from_millis(1200)).await;
    assert!(
        (1..=8).contains(&burst.len()),
        "{} pushes for 41 changes in 1.7 s",
        burst.len()
    );
    assert_eq!(burst.last().unwrap().attention, 1);
    assert!(burst.windows(2).all(|w| w[0].rev < w[1].rev));
    // Activity alone is not a change of the summary.
    daemon.update_agent("a", |a| a.activity = "reading".into());
    assert!(peer.summaries(Duration::from_millis(400)).await.is_empty());
}

/// `summary {watch: true}` racing changes of the model: whatever the order,
/// the watcher ends at the model's summary, its revisions increase, and no
/// change is lost between the reply and the pushes.
async fn race(gap: Option<Duration>, rounds: usize) -> usize {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    daemon.model().agents.push(agent("a", "idle"));
    *daemon.summaries.capture_first.lock().unwrap() = gap;
    let mut lost = 0;
    for round in 0..rounds {
        let mut peer = Peer::operator(&daemon, false).await;
        // The change lands anywhere from before the capture to well after
        // the reply.
        let delay = Duration::from_micros((round % 10) as u64 * 500);
        let changer = {
            let daemon = daemon.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let status = if round % 2 == 0 { "working" } else { "idle" };
                daemon.update_agent("a", |a| a.status = status.parse().unwrap());
            })
        };
        let reply = peer.call("summary", json!({"watch": true})).await;
        let mut last: WorkspaceSummary = serde_json::from_value(reply.result.unwrap()).unwrap();
        changer.await.unwrap();
        let want = if round % 2 == 0 { 1 } else { 0 };
        while last.working != want {
            let Some(pushed) = peer.next_summary(Duration::from_millis(300)).await else {
                lost += 1;
                break;
            };
            assert!(pushed.rev > last.rev);
            last = pushed;
        }
    }
    lost
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_registers_and_captures_in_one_step() {
    assert_eq!(race(None, 1000).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn capturing_before_registering_loses_changes() {
    // The hook opens the gap: a change made in it is in neither the reply
    // nor a push.
    assert!(race(Some(Duration::from_millis(5)), 10).await > 0);
}

/// A stand-in dashboard server on `dir/web.sock`: answers `hello` and
/// `web_rescan` (`answer`), or drops every connection. Counts connections.
fn stand_in(dir: &Path, answer: bool) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    use std::io::{BufRead, Write};
    let socket = dir.join("web.sock");
    let token = crew_core::paths::operator_token_path(&socket);
    std::fs::write(&token, "web-token").unwrap();
    std::fs::set_permissions(&token, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            if !answer {
                continue;
            }
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let request: Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str() {
                    Some("hello") => json!({"role": "operator", "version": "0", "protocol":
                        PROTOCOL_VERSION, "pid": 1}),
                    _ => json!({"rescanning": true}),
                };
                writeln!(writer, "{}", json!({"id": request["id"], "result": result})).unwrap();
                line.clear();
            }
        }
    });
    calls
}

#[tokio::test(start_paused = true)]
async fn a_daemon_tells_a_running_dashboard_server_once_and_again_while_that_fails() {
    // No server: no call.
    let none = tempfile::tempdir().unwrap();
    assert_eq!(
        tell_the_dashboard(none.path().join("web.sock"), &RESCAN_RETRIES).await,
        0
    );
    // One that answers: one call.
    let answering = tempfile::tempdir().unwrap();
    let calls = stand_in(answering.path(), true);
    assert_eq!(
        tell_the_dashboard(answering.path().join("web.sock"), &RESCAN_RETRIES).await,
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // One that fails every call: again after 1, 2, 4, 8 and 16 s, then no
    // more.
    let failing = tempfile::tempdir().unwrap();
    let calls = stand_in(failing.path(), false);
    let started = tokio::time::Instant::now();
    assert_eq!(
        tell_the_dashboard(failing.path().join("web.sock"), &RESCAN_RETRIES).await,
        6
    );
    assert_eq!(started.elapsed(), Duration::from_secs(1 + 2 + 4 + 8 + 16));
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}
