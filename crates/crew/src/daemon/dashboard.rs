//! What a daemon does for the dashboard server (notes/web-multi.md
//! §multi-transport, §multi-server): the summary a workspace switcher shows,
//! the server's watch (`web_attach`, `web_event`), and the call that tells a
//! running server this daemon has started.
//!
//! The summary is a few counts kept in a `watch` channel. Whatever changes
//! what it counts marks it dirty; a task recomputes it while some
//! connection watches. `summary {watch: true}` registers the connection and
//! then computes, so a change is either in its reply or followed by a push,
//! never neither.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crew_core::rpc::{Call, Push, RpcError, ServerFrame, WorkspaceSummary};
use crew_core::types::{AgentStatus, TaskStatus, WebEventKind, OPERATOR_ID};
use tokio::sync::{watch, Notify};

use super::server::Conn;
use super::Daemon;
use crate::persistence::NewEvent;

/// A watching connection gets at most one `summary` push per this long.
pub(crate) const SUMMARY_EVERY: Duration = Duration::from_millis(250);
/// When telling the dashboard server that this daemon started fails although
/// its socket is there: the waits before each try again.
pub(crate) const RESCAN_RETRIES: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// The summary and what keeps it current.
pub(crate) struct Summaries {
    tx: watch::Sender<WorkspaceSummary>,
    /// Something the summary counts changed since it was last computed.
    dirty: AtomicBool,
    /// So did the operator's open questions (read from the database, so
    /// only when a message to or from the operator went by).
    questions_dirty: AtomicBool,
    questions: Mutex<u32>,
    /// One refresh at a time, from its capture to its publication: a
    /// capture is never published after a later one.
    refresh: Mutex<()>,
    /// Wakes the task; shared with it, which holds the daemon only weakly.
    wake: Arc<Notify>,
    /// The task that recomputes it, started by the first watcher.
    task: OnceLock<()>,
    /// Test hook: capture the summary, wait this long, then register the
    /// watcher: the gap that registering first closes.
    #[cfg(test)]
    pub(crate) capture_first: Mutex<Option<Duration>>,
    /// Hold one captured summary before publication to exercise concurrent
    /// refreshes, without changing the state either refresh captured.
    #[cfg(test)]
    publish_hold: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

impl Default for Summaries {
    fn default() -> Self {
        Self {
            tx: watch::channel(WorkspaceSummary::default()).0,
            dirty: AtomicBool::new(true),
            questions_dirty: AtomicBool::new(true),
            questions: Mutex::new(0),
            refresh: Mutex::new(()),
            wake: Arc::new(Notify::new()),
            task: OnceLock::new(),
            #[cfg(test)]
            capture_first: Mutex::new(None),
            #[cfg(test)]
            publish_hold: Mutex::new(None),
        }
    }
}

impl Drop for Summaries {
    /// The daemon is gone: its task wakes and ends.
    fn drop(&mut self) {
        self.wake.notify_one();
    }
}

/// The dashboard server's watch on this daemon: which connection, and the
/// address it serves the dashboard at.
#[derive(Default)]
pub(crate) struct Watched {
    current: Mutex<Option<(u64, String)>>,
}

impl Watched {
    pub(crate) fn url(&self) -> Option<String> {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, url)| url.clone())
    }
}

impl Daemon {
    /// A push went out: the summary may have to be computed again.
    pub(crate) fn summary_touched(&self, push: &Push) {
        let summaries = &self.summaries;
        match push {
            Push::Agent(_) | Push::AgentRemoved { .. } | Push::Task(_) | Push::Session(_) => {}
            Push::Message(message) if message.to == OPERATOR_ID || message.from == OPERATOR_ID => {
                summaries.questions_dirty.store(true, Ordering::SeqCst);
            }
            _ => return,
        }
        summaries.dirty.store(true, Ordering::SeqCst);
        summaries.wake.notify_one();
    }

    /// Compute the summary if something it counts changed (`force`: in any
    /// case), and publish it with a new `rev` if it differs.
    fn refresh_summary(&self, force: bool) {
        let summaries = &self.summaries;
        let _one_at_a_time = summaries.refresh.lock().unwrap_or_else(|e| e.into_inner());
        if !summaries.dirty.swap(false, Ordering::SeqCst) && !force {
            return;
        }
        if summaries.questions_dirty.swap(false, Ordering::SeqCst) || force {
            match self.open_questions() {
                Ok(open) => {
                    *summaries
                        .questions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = open.len() as u32
                }
                Err(e) => {
                    // Counted again at the next message.
                    summaries.questions_dirty.store(true, Ordering::SeqCst);
                    tracing::warn!("counting the open questions: {}", e.message);
                }
            }
        }
        let questions = *summaries
            .questions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let next = {
            let model = self.model();
            let agents = &model.agents;
            let count = |f: &dyn Fn(&AgentStatus) -> bool| {
                agents.iter().filter(|a| f(&a.status)).count() as u32
            };
            WorkspaceSummary {
                rev: 0,
                project: model.session.team.as_ref().map(|t| t.project.clone()),
                agents: agents.len() as u32,
                working: count(&|s| *s == AgentStatus::Working),
                attention: count(&|s| matches!(s, AgentStatus::Blocked | AgentStatus::Failed)),
                questions,
                tasks: model.tasks.len() as u32,
                tasks_done: model
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Done)
                    .count() as u32,
            }
        };
        #[cfg(test)]
        {
            let hold = summaries.publish_hold.lock().unwrap().take();
            if let Some((captured, resume)) = hold {
                captured.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }
        summaries.tx.send_if_modified(|current| {
            let same = WorkspaceSummary {
                rev: current.rev,
                ..next.clone()
            } == *current;
            if !same {
                *current = WorkspaceSummary {
                    rev: current.rev + 1,
                    ..next
                };
            }
            !same
        });
    }

    /// The task that recomputes the summary while anyone watches it. It
    /// holds the daemon weakly: it ends with it.
    fn start_summary_task(&self) {
        self.summaries.task.get_or_init(|| {
            let weak = Arc::downgrade(&self.0);
            let wake = self.summaries.wake.clone();
            tokio::spawn(async move {
                loop {
                    wake.notified().await;
                    let Some(inner) = weak.upgrade() else { return };
                    let daemon = Daemon(inner);
                    // Nobody watches: it stays dirty, for whoever asks next.
                    if daemon.summaries.tx.receiver_count() > 0 {
                        daemon.refresh_summary(false);
                    }
                }
            });
        });
    }

    /// `summary {watch}`: the summary now; with `watch`, the connection is
    /// registered first and gets a push for every change after its reply.
    pub(crate) async fn summary(&self, conn: &mut Conn, watch: bool) -> WorkspaceSummary {
        #[cfg(test)]
        let gap = *self
            .summaries
            .capture_first
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        if let Some(gap) = gap {
            self.start_summary_task();
            self.refresh_summary(true);
            let current = self.summaries.tx.borrow().clone();
            tokio::time::sleep(gap).await;
            let mut rx = self.summaries.tx.subscribe();
            rx.mark_unchanged();
            if watch {
                conn.summary_after_reply = Some(rx);
            }
            return current;
        }
        let mut rx = self.summaries.tx.subscribe();
        self.start_summary_task();
        self.refresh_summary(true);
        let current = rx.borrow_and_update().clone();
        if watch {
            conn.summary_after_reply = Some(rx);
        }
        current
    }

    /// Stream the summary's changes to a connection: the first at once,
    /// then at most one per `SUMMARY_EVERY`, each as it is when it is sent.
    pub(crate) fn start_summary_watch(
        &self,
        conn: &mut Conn,
        mut rx: watch::Receiver<WorkspaceSummary>,
    ) {
        if let Some(task) = conn.summary_task.take() {
            task.abort();
        }
        let tx = conn.tx.clone();
        conn.summary_task = Some(conn.spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    return;
                }
                let summary = rx.borrow_and_update().clone();
                if tx
                    .send(ServerFrame::push(Push::Summary(summary)))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(SUMMARY_EVERY).await;
            }
        }));
    }

    /// `web_attach {url}`: this connection is the dashboard server's watch.
    /// The session's `web` line is `url` until it ends.
    pub(crate) fn web_attach(&self, conn: &mut Conn, url: String) -> Result<(), RpcError> {
        let url = url.trim().to_string();
        if !url.starts_with("http://") || url.len() > 200 || url.chars().any(char::is_control) {
            return Err(RpcError::invalid(
                "web_attach takes the dashboard's address, http://…",
            ));
        }
        *self
            .watched
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((conn.id, url.clone()));
        conn.web_attached = true;
        self.emit(NewEvent::new(
            "web",
            "web.joined",
            format!("shown in the web dashboard at {url}"),
        ));
        self.push(Push::Session(Box::new(self.session_info())));
        Ok(())
    }

    /// The watch connection `conn` ended: the session is no longer shown,
    /// unless a newer watch took its place.
    pub(crate) fn web_detached(&self, conn: u64) {
        let left = {
            let mut current = self
                .watched
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match current.as_ref() {
                Some((id, _)) if *id == conn => current.take().is_some(),
                _ => false,
            }
        };
        if left {
            self.emit(NewEvent::new(
                "web",
                "web.left",
                "no longer shown in the web dashboard",
            ));
            self.push(Push::Session(Box::new(self.session_info())));
        }
    }

    /// `web_event {kind, count}` on the watch: the texts are the daemon's.
    pub(crate) fn web_event(
        &self,
        conn: &Conn,
        kind: WebEventKind,
        count: u32,
    ) -> Result<(), RpcError> {
        if !conn.web_attached {
            return Err(RpcError::invalid(
                "web_event is for the dashboard server's watch (web_attach first)",
            ));
        }
        let plural = |n: u32| if n == 1 { "" } else { "s" };
        let event = match kind {
            WebEventKind::Paired => NewEvent::new(
                "web",
                "web.paired",
                format!(
                    "a browser was paired with the dashboard ({count} session{})",
                    plural(count)
                ),
            ),
            WebEventKind::AuthFailed => NewEvent::new(
                "web",
                "web.auth_failed",
                format!(
                    "{count} refused web login{} in the last minute",
                    plural(count)
                ),
            )
            .warn(),
        };
        self.emit(event);
        Ok(())
    }
}

/// Tell the dashboard server at `socket`, if one runs, that this daemon
/// started: `web_rescan`, as the operator. Tried again after each of
/// `retries` while it fails and the socket is still there. Returns how many
/// calls were made.
pub(crate) async fn tell_the_dashboard(socket: PathBuf, retries: &[Duration]) -> usize {
    let mut calls = 0;
    let mut waits = retries.iter();
    loop {
        if !socket.exists() {
            return calls;
        }
        calls += 1;
        let path = socket.clone();
        let told = tokio::task::spawn_blocking(move || rescan(&path))
            .await
            .unwrap_or(false);
        if told {
            return calls;
        }
        let Some(wait) = waits.next() else {
            tracing::debug!(
                "the dashboard server at {} did not answer",
                socket.display()
            );
            return calls;
        };
        tokio::time::sleep(*wait).await;
    }
}

/// One `web_rescan` as the operator, with the server's token. Not
/// `Client::operator`: that refuses in an agent's environment, and a daemon
/// is not an agent whatever environment started it.
fn rescan(socket: &Path) -> bool {
    let Ok(Some(token)) = crew_core::paths::read_operator_token(socket) else {
        return false;
    };
    let Ok(mut client) = crew_core::client::Client::connect(socket) else {
        return false;
    };
    let _ = client.set_timeout(Some(Duration::from_secs(2)));
    let hello = client.call::<crew_core::rpc::HelloInfo>(Call::Hello(crew_core::rpc::Hello {
        role: crew_core::rpc::Role::Operator,
        agent_id: None,
        token: Some(token),
        client: "daemon".into(),
        protocol: crew_core::rpc::PROTOCOL_VERSION,
        web: false,
        workspace: None,
    }));
    hello.is_ok() && client.call_value(Call::WebRescan).is_ok()
}

#[cfg(test)]
mod tests;
