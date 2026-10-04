//! Dashboard state and its reducers.
//!
//! Everything here is pure: state changes come from daemon snapshots/pushes,
//! replies to requests, and key presses, each stamped with a caller-supplied
//! clock so behaviour is deterministic under test.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use crew_core::rpc::{
    GitOutcome, ModelChoice, OutputResult, Push, SessionInfo, Snapshot, TranscriptResult,
};
use crew_core::types::{
    Agent, AgentStatus, Event, EventLevel, InboxEntry, Message, MessageType, RuntimeKind, Task,
    TaskStatus, OPERATOR_ID, ORCHESTRATOR_ID,
};
use ratatui::layout::Rect;

use crate::anim::Anims;
use crate::theme::{spinner_index, SPINNER_MS};
use crate::transcript::{self, TLine};
use crew_core::vcs::VcsEvent;

pub const MAX_MESSAGES: usize = 500;
pub const MAX_EVENTS: usize = 2000;
/// How often the agent detail view refreshes its terminal capture.
pub const OUTPUT_REFRESH_MS: i64 = 2000;
const OUTPUT_TIMEOUT_MS: i64 = 6000;
/// A transcript read this recently is shown again as it is when the pane is
/// scrolled up once more.
const TRANSCRIPT_FRESH_MS: i64 = 5000;
/// A pushed VCS event older than this starts no animation.
const VCS_FRESH_MS: i64 = 10_000;
/// How often the terminal is read while it is being typed into, and for
/// how long after a key it is read back-to-back instead (the backend serves
/// each read on its own clock; the gaps between reads are what crew can
/// take out of the echo's delay).
pub const TYPING_REFRESH_MS: i64 = 200;
pub const KEY_BURST_MS: i64 = 1500;

/// What a member is on, as the task list says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkOn<'a> {
    pub title: &'a str,
    /// The member's last task, finished; otherwise one under way.
    pub done: bool,
    /// How long it has been under way, or since it was finished.
    pub elapsed: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Detail,
    Tasks,
    Logs,
}

/// A request for the daemon, produced by key handling and timers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Quit,
    FetchOutput {
        id: String,
    },
    /// The agent's session transcript, for the inspector scrolled up.
    FetchTranscript {
        id: String,
    },
    FetchInbox {
        id: String,
    },
    FetchEvents {
        id: String,
    },
    FetchDiff {
        id: String,
    },
    Kill(String),
    Restart(String),
    RestartFresh(String),
    Pause(String),
    Resume(String),
    /// Continue every agent paused at a usage limit; `restart` restarts
    /// their sessions so a new login takes effect.
    ResumeLimited {
        restart: bool,
    },
    Focus(String),
    Send {
        to: String,
        body: String,
    },
    /// Stop the team: every agent, then the daemon.
    StopTeam,
    /// The operator's answer to the question with this message id.
    Answer {
        id: String,
        body: String,
    },
    /// Queue a compaction of an agent's context.
    Compact(String),
    /// Type into an agent's terminal: text as one prompt, or an Enter.
    Type {
        id: String,
        text: String,
    },
    /// Keys for an agent's terminal, as the terminal would send them.
    Keys {
        id: String,
        text: String,
    },
    /// Turn mouse reporting on or off (off leaves the terminal's own text
    /// selection alone).
    SetMouse(bool),
    /// The models agents can be put on, for the picker.
    FetchModels,
    /// Put an agent on a model (`model` empty: the CLI's own default),
    /// on the CLI `runtime`; `label` is what the dashboard says it did.
    SetModel {
        id: String,
        model: String,
        runtime: RuntimeKind,
        label: String,
    },
}

/// The daemon's answer to a `Command`.
#[derive(Clone, Debug)]
pub enum Reply {
    Output {
        agent: String,
        result: Result<OutputResult, String>,
    },
    Transcript {
        agent: String,
        result: Result<TranscriptResult, String>,
    },
    Inbox {
        agent: String,
        result: Result<Vec<InboxEntry>, String>,
    },
    Events {
        agent: String,
        result: Result<Vec<Event>, String>,
    },
    Diff {
        agent: String,
        result: Result<GitOutcome, String>,
    },
    Action {
        label: String,
        result: Result<(), String>,
    },
    /// The answer to `Command::StopTeam`.
    Stopped(Result<(), String>),
    /// Whether keys for a terminal got there; only a failure is worth a word.
    Keys(Result<(), String>),
    /// Whether the answer to question `id` got through.
    Answered {
        id: String,
        result: Result<(), String>,
    },
    /// The answer to `Command::FetchModels`.
    Models(Result<Vec<ModelChoice>, String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compose {
    pub to: String,
    pub text: String,
    /// Cursor position as a character index into `text`.
    pub cursor: usize,
    /// Why the last Enter ran nothing (a slash command that does not exist).
    pub notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    Compose(Compose),
    /// A line typed into an agent's terminal (`to` is the agent).
    Type(Compose),
    ConfirmKill(String),
    /// Start one agent without resuming its old conversation.
    ConfirmRestartFresh(String),
    /// Restart the agents paused at a usage limit (how many).
    ConfirmRestartPaused(usize),
    /// Stop the whole team.
    ConfirmStop,
    /// A question an agent put to the operator, and the answer being given.
    Question(QuestionBox),
    /// One bus message, in full.
    Message(MessageView),
    /// The model and CLI an agent runs on, being chosen.
    Model(ModelPicker),
}

/// The CLIs the picker offers, in its order.
pub const HARNESSES: [RuntimeKind; 3] = [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi];

/// The box an agent's model is chosen in: a tab per CLI, its models below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPicker {
    pub agent: String,
    /// The CLI whose models are shown.
    pub runtime: RuntimeKind,
    /// The highlighted row of that CLI's models.
    pub row: usize,
    /// The operator moved the highlight: a listing that comes in later
    /// leaves it where it is.
    pub moved: bool,
    /// Why the last Enter did nothing.
    pub notice: Option<String>,
}

/// A bus message opened to be read in full.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageView {
    /// Id of the message shown.
    pub id: String,
    /// Lines scrolled past.
    pub scroll: usize,
}

/// The answer box of one open question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionBox {
    /// Message id of the question shown.
    pub id: String,
    /// The selected row: one of the suggested answers, or (one past them)
    /// the operator's own words.
    pub choice: usize,
    /// The operator's own words; `to` is unused.
    pub input: Compose,
    /// Lines of the question scrolled past.
    pub scroll: usize,
    /// When the box opened by itself (the question just came in); 0 when
    /// the operator opened it. An Enter this soon after was meant for
    /// whatever was there before, and does not send an answer.
    pub popped_at: i64,
}

/// How long after a question box opens by itself an Enter is not taken
/// for an answer.
pub const POPUP_GRACE_MS: i64 = 800;

#[derive(Clone, Debug, Default)]
pub struct DetailState {
    pub agent: String,
    pub output: String,
    pub output_error: Option<String>,
    pub alive: bool,
    pub output_at: Option<i64>,
    pub pending: bool,
    pub requested_at: i64,
    pub show_diff: bool,
    pub diff: Option<String>,
    /// Delivery state of the agent's inbox, by message id: (delivered, acked).
    pub delivery: HashMap<String, (bool, bool)>,
    /// Events fetched for this agent (reaches further back than the live buffer).
    pub events: Option<Vec<Event>>,
    /// Lines scrolled up from the bottom of the output pane. At 0 the pane
    /// is the live terminal; scrolled up it is the session transcript.
    pub scroll: usize,
    /// What the agent was told, wrote and did, from its session file.
    pub transcript: Option<TranscriptResult>,
    pub transcript_at: Option<i64>,
    pub transcript_pending: bool,
    pub transcript_requested_at: i64,
    pub transcript_error: Option<String>,
    /// When the pane was scrolled away from the live terminal.
    pub scrolled_at: Option<i64>,
    /// Columns the pane's text had when last drawn; the transcript wraps
    /// to them.
    pub cols: Cell<usize>,
    /// Where the terminal pane was last drawn, for clicks into it.
    pub terminal_rect: Cell<Rect>,
    /// Keys go straight to the agent's terminal (a click into the pane
    /// starts this; Ctrl+] ends it).
    pub typing: bool,
    /// The terminal pane fills the inspector.
    pub full: bool,
    /// When a key was last typed into the terminal.
    pub key_at: i64,
    /// The transcript laid out for a width, kept until either changes.
    wrapped: RefCell<Option<(usize, Rc<Vec<TLine>>)>>,
}

impl DetailState {
    /// The transcript in lines `cols` wide; `None` when there is none.
    pub fn transcript_lines(&self, cols: usize) -> Option<Rc<Vec<TLine>>> {
        let transcript = self.transcript.as_ref().filter(|t| !t.entries.is_empty())?;
        let mut wrapped = self.wrapped.borrow_mut();
        if let Some((_, lines)) = wrapped.as_ref().filter(|(at, _)| *at == cols) {
            return Some(lines.clone());
        }
        let lines = Rc::new(transcript::layout(
            &transcript.entries,
            transcript.truncated,
            cols,
        ));
        *wrapped = Some((cols, lines.clone()));
        Some(lines)
    }

    pub fn set_transcript(&mut self, transcript: TranscriptResult, now: i64) {
        self.transcript = Some(transcript);
        self.transcript_at = Some(now);
        self.transcript_error = None;
        *self.wrapped.borrow_mut() = None;
    }
}

#[derive(Clone, Debug)]
pub struct LogsState {
    /// Rows scrolled up from the newest event; 0 follows the stream.
    pub scroll: usize,
    pub filter_agent: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StatusLine {
    pub text: String,
    pub is_error: bool,
    pub expires_at: i64,
}

#[derive(Clone, Debug)]
pub struct App {
    /// Last drawn screen size, used to clamp keyboard scrolling to wrapped text.
    pub(crate) screen: Cell<Rect>,
    pub session: SessionInfo,
    /// Workers in natural id order, then the orchestrator.
    pub agents: Vec<Agent>,
    pub tasks: Vec<Task>,
    pub messages: VecDeque<Message>,
    pub events: VecDeque<Event>,
    pub selected: Option<String>,
    pub view: View,
    pub help: bool,
    pub modal: Option<Modal>,
    pub anims: Anims,
    pub connected: bool,
    /// True once the first snapshot arrived.
    pub loaded: bool,
    pub status: Option<StatusLine>,
    pub detail: DetailState,
    pub logs: LogsState,
    pub tasks_scroll: usize,
    /// Server clock minus local clock, for elapsed-time displays.
    pub clock_offset: i64,
    pub should_quit: bool,
    /// Proper names of known models by every name they go by, lowercase
    /// (`opus` is `Opus 5.5`): for agents the daemon sent without one.
    pub model_names: HashMap<String, String>,
    /// The dashboard takes mouse clicks and the wheel.
    pub mouse: bool,
    /// Questions agents put to the operator, oldest first, until answered.
    pub questions: Vec<Message>,
    /// Questions whose answer is on its way to the daemon.
    pub answering: HashSet<String>,
    /// Questions whose answer box has been on screen (opened by itself or
    /// by the operator): one put off with esc does not open again by itself.
    pub shown_questions: HashSet<String>,
    /// Ring the terminal's bell when a question opens by itself.
    pub bell: bool,
    /// A bell is due.
    ring: bool,
    /// A stop of the team was asked for and is under way.
    pub stopping: bool,
    /// The team was stopped from this dashboard.
    pub stopped: bool,
    pub(crate) daemon_stopped: bool,
    /// The models agents can be put on, as the daemon last listed them.
    pub models: Vec<ModelChoice>,
    /// A listing was asked for and has not come back.
    pub models_pending: bool,
    /// Why the last listing failed.
    pub models_error: Option<String>,
    dirty: bool,
    last_spin: usize,
    /// Something was moving at the last tick: one more redraw shows it still.
    was_moving: bool,
    last_second: i64,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Order ids so that `claude-2` sorts before `claude-10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (&str, Option<u64>) {
        let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
        let (head, tail) = s.split_at(s.len() - digits);
        (head, tail.parse().ok())
    }
    let (ah, an) = split(a);
    let (bh, bn) = split(b);
    ah.cmp(bh).then(an.cmp(&bn)).then(a.cmp(b))
}

fn is_orchestrator(agent: &Agent) -> bool {
    agent.orchestrator || agent.id == ORCHESTRATOR_ID
}

fn satellite_parent<'a>(agents: &'a [Agent], agent: &Agent) -> Option<&'a Agent> {
    if is_orchestrator(agent) {
        return None;
    }
    let parent_id = agent.parent.as_deref()?;
    let parent = agents.iter().find(|a| a.id == parent_id)?;
    (parent.id != agent.id && !is_orchestrator(parent) && parent.parent.is_none()).then_some(parent)
}

impl App {
    pub fn new() -> Self {
        Self {
            screen: Cell::new(Rect::new(0, 0, 80, 24)),
            session: SessionInfo::default(),
            agents: Vec::new(),
            tasks: Vec::new(),
            messages: VecDeque::new(),
            events: VecDeque::new(),
            selected: None,
            view: View::Dashboard,
            help: false,
            modal: None,
            anims: Anims::default(),
            connected: false,
            loaded: false,
            status: None,
            detail: DetailState::default(),
            logs: LogsState {
                scroll: 0,
                filter_agent: None,
            },
            tasks_scroll: 0,
            clock_offset: 0,
            should_quit: false,
            model_names: HashMap::new(),
            mouse: true,
            questions: Vec::new(),
            answering: HashSet::new(),
            shown_questions: HashSet::new(),
            bell: true,
            ring: false,
            stopping: false,
            stopped: false,
            daemon_stopped: false,
            models: Vec::new(),
            models_pending: false,
            models_error: None,
            dirty: true,
            last_spin: 0,
            was_moving: false,
            last_second: 0,
        }
    }

    // ---- queries ---------------------------------------------------------

    /// Every agent except the orchestrator, subagents included.
    pub fn workers(&self) -> impl Iterator<Item = &Agent> {
        self.agents.iter().filter(|a| !is_orchestrator(a))
    }

    /// The agent a subagent hangs off in the topology: its parent, when that
    /// is a known top-level worker. Subagents of the orchestrator (or of an
    /// agent that is gone) have nothing to orbit and are shown as ordinary
    /// members of the ring.
    pub fn satellite_parent(&self, agent: &Agent) -> Option<&Agent> {
        satellite_parent(&self.agents, agent)
    }

    /// Workers that sit on the ring: everyone who is not a satellite.
    pub fn top_level(&self) -> impl Iterator<Item = &Agent> {
        self.workers()
            .filter(|a| self.satellite_parent(a).is_none())
    }

    /// The subagents drawn as satellites of `id`, in display order.
    pub fn subagents_of<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Agent> {
        self.workers()
            .filter(move |a| self.satellite_parent(a).is_some_and(|p| p.id == id))
    }

    pub fn orchestrator(&self) -> Option<&Agent> {
        self.agents.iter().find(|a| is_orchestrator(a))
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.selected.as_deref().and_then(|id| self.agent(id))
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.as_deref() == Some(id)
    }

    /// (done, total) over all tasks.
    pub fn task_counts(&self) -> (usize, usize) {
        let done = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        (done, self.tasks.len())
    }

    /// The task `id` is on, else the one it finished last.
    pub fn work_of(&self, id: &str, now: i64) -> Option<WorkOn<'_>> {
        let now = self.server_now(now);
        let mine = || {
            self.tasks
                .iter()
                .filter(move |t| t.assigned_agent.as_deref() == Some(id))
        };
        let current = mine()
            .filter(|t| !t.status.is_terminal() && t.started_at.is_some())
            .max_by_key(|t| t.started_at);
        if let Some(task) = current {
            return Some(WorkOn {
                title: &task.title,
                done: false,
                elapsed: task.started_at.map(|at| (now - at).max(0)),
            });
        }
        mine()
            .filter(|t| t.status == TaskStatus::Done)
            .max_by_key(|t| t.completed_at)
            .map(|task| WorkOn {
                title: &task.title,
                done: true,
                elapsed: task.completed_at.map(|at| (now - at).max(0)),
            })
    }

    /// The daemon's idea of "now", for elapsed-time displays.
    pub fn server_now(&self, now: i64) -> i64 {
        now + self.clock_offset
    }

    /// How long the team has been running (frozen once it completed).
    pub fn elapsed_ms(&self, now: i64) -> Option<i64> {
        let start = self.session.team_started_at?;
        let end = self.session.completed_at.unwrap_or(self.server_now(now));
        Some((end - start).max(0))
    }

    /// Messages to or from an agent (broadcasts included), oldest first.
    pub fn messages_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Message> {
        self.messages
            .iter()
            .filter(move |m| m.from == id || m.to == id || m.is_broadcast())
    }

    pub fn tasks_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Task> {
        self.tasks.iter().filter(move |t| {
            t.assigned_agent.as_deref() == Some(id) || t.reviewer.as_deref() == Some(id)
        })
    }

    /// Warn/error events about an agent, oldest first: the fetched history when
    /// available, the live buffer otherwise.
    pub fn problems_for(&self, id: &str) -> Vec<&Event> {
        let relevant = |e: &&Event| {
            e.agent_id.as_deref() == Some(id)
                && matches!(e.level, EventLevel::Warn | EventLevel::Error)
        };
        let live: Vec<&Event> = self.events.iter().filter(relevant).collect();
        match &self.detail.events {
            Some(fetched) if self.detail.agent == id => {
                let last_fetched = fetched.iter().map(|e| e.seq).max().unwrap_or(0);
                let mut out: Vec<&Event> = fetched.iter().filter(relevant).collect();
                out.extend(live.into_iter().filter(|e| e.seq > last_fetched));
                out
            }
            _ => live,
        }
    }

    fn has_spinner(&self) -> bool {
        self.agents
            .iter()
            .any(|a| a.status == AgentStatus::Working && !a.paused)
            || self.anims.running()
            || self.tasks.iter().any(|t| t.status == TaskStatus::Working)
    }

    // ---- dirty tracking and timers --------------------------------------

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Advance time-driven state; marks the app dirty when a redraw is due.
    pub fn tick(&mut self, now: i64) {
        self.pop_question(now);
        let moving = self.anims.animating(now);
        if self.anims.gc(now) || moving || self.was_moving {
            self.dirty = true;
        }
        self.was_moving = moving;
        let spin = spinner_index(now);
        if spin != self.last_spin {
            self.last_spin = spin;
            if self.has_spinner() {
                self.dirty = true;
            }
        }
        let second = now / 1000;
        if second != self.last_second {
            self.last_second = second;
            self.dirty = true;
        }
        if self.status.as_ref().is_some_and(|s| now >= s.expires_at) {
            self.status = None;
            self.dirty = true;
        }
        if self.detail.pending && now - self.detail.requested_at > OUTPUT_TIMEOUT_MS {
            self.detail.pending = false;
        }
        if self.detail.transcript_pending
            && now - self.detail.transcript_requested_at > OUTPUT_TIMEOUT_MS
        {
            self.detail.transcript_pending = false;
            self.detail.transcript_error = Some("the daemon did not answer".to_string());
        }
    }

    /// How long the event loop may sleep before the next time-driven redraw:
    /// frame rate while something moves (a pulse, a sweep), spinner rate while agents work, and
    /// once a second otherwise (for the clock).
    pub fn next_wake(&self, now: i64) -> Duration {
        let ms = if self.anims.animating(now) {
            33
        } else {
            let mut ms = 1000 - now.rem_euclid(1000);
            if self.has_spinner() {
                ms = ms.min(SPINNER_MS - now.rem_euclid(SPINNER_MS));
            }
            if let Some(expiry) = self.anims.next_expiry(now) {
                ms = ms.min(expiry);
            }
            ms.max(5)
        };
        Duration::from_millis(ms as u64)
    }

    /// Requests that are due on a timer: the detail view's terminal capture,
    /// and the transcript once the pane is scrolled up.
    pub fn due_commands(&mut self, now: i64) -> Vec<Command> {
        if self.view != View::Detail || self.detail.agent.is_empty() {
            return Vec::new();
        }
        let mut cmds = Vec::new();
        let detail = &mut self.detail;
        // Scrolled up, the pane shows the session transcript. It is read
        // when the scrolling starts and then left alone, so the text does
        // not move under the reader (`r` reads it again).
        let scrolled = detail.scroll > 0 && !detail.show_diff;
        match (scrolled, detail.scrolled_at) {
            (true, None) => detail.scrolled_at = Some(now),
            (false, Some(_)) => {
                detail.scrolled_at = None;
                detail.transcript_error = None;
            }
            _ => {}
        }
        if let Some(since) = detail.scrolled_at {
            let fresh = detail
                .transcript_at
                .is_some_and(|at| at + TRANSCRIPT_FRESH_MS >= since);
            if !fresh && !detail.transcript_pending && detail.transcript_error.is_none() {
                detail.transcript_pending = true;
                detail.transcript_requested_at = now;
                cmds.push(Command::FetchTranscript {
                    id: detail.agent.clone(),
                });
            }
        }
        // Typing into the terminal wants to see the keys land: right after
        // a key the screen is read as often as the daemon answers.
        let every = if detail.typing && now - detail.key_at < KEY_BURST_MS {
            0
        } else if detail.typing {
            TYPING_REFRESH_MS
        } else {
            OUTPUT_REFRESH_MS
        };
        let due = detail.output_at.is_none_or(|at| now - at >= every);
        if !detail.pending && due && now - detail.requested_at >= every {
            detail.pending = true;
            detail.requested_at = now;
            cmds.push(Command::FetchOutput {
                id: detail.agent.clone(),
            });
        }
        cmds
    }

    /// A question for the operator that has not been on screen yet opens its
    /// answer box by itself, over whatever view is open — once nothing else
    /// holds the keyboard (a box being typed in, the inspector's terminal,
    /// the key reference), so no keystroke lands in it by accident.
    fn pop_question(&mut self, now: i64) {
        if let Some(Modal::Question(open)) = &self.modal {
            self.shown_questions.insert(open.id.clone());
        }
        if !self.loaded
            || self.modal.is_some()
            || self.help
            || (self.view == View::Detail && self.detail.typing)
        {
            return;
        }
        let Some(id) = self
            .open_questions()
            .iter()
            .find(|q| !self.shown_questions.contains(&q.id))
            .map(|q| q.id.clone())
        else {
            return;
        };
        self.shown_questions.insert(id.clone());
        self.modal = Some(Modal::Question(QuestionBox {
            id,
            popped_at: now,
            ..QuestionBox::default()
        }));
        self.ring = self.bell;
        self.dirty = true;
    }

    /// Whether the terminal's bell is due (once per question that opened
    /// by itself).
    pub fn take_ring(&mut self) -> bool {
        std::mem::take(&mut self.ring)
    }

    /// The questions still waiting for the operator, oldest first.
    pub fn open_questions(&self) -> Vec<&Message> {
        self.questions
            .iter()
            .filter(|q| !self.answering.contains(&q.id))
            .collect()
    }

    /// The daemon connection was lost.
    pub fn disconnected(&mut self) {
        self.connected = false;
        if self.stopping {
            // It went away because this dashboard stopped it.
            self.team_stopped();
        }
        self.dirty = true;
    }

    fn team_stopped(&mut self) {
        self.stopping = false;
        self.stopped = true;
        self.should_quit = true;
    }

    /// A warning that stays in the footer long enough to be read and acted
    /// on (any other status replaces it).
    pub fn set_notice(&mut self, text: impl Into<String>, now: i64) {
        self.status = Some(StatusLine {
            text: text.into(),
            is_error: true,
            expires_at: now + 30_000,
        });
        self.dirty = true;
    }

    pub fn set_status(&mut self, text: impl Into<String>, is_error: bool, now: i64) {
        self.status = Some(StatusLine {
            text: text.into(),
            is_error,
            expires_at: now + if is_error { 6000 } else { 3000 },
        });
        self.dirty = true;
    }

    // ---- reducers --------------------------------------------------------

    pub fn apply_snapshot(&mut self, snap: Snapshot, now: i64) {
        self.clock_offset = snap.server_time - now;
        self.session = snap.session;
        self.agents = snap.agents;
        for agent in &mut self.agents {
            name_model(&self.model_names, agent);
        }
        self.sort_agents();
        self.tasks = snap.tasks;
        self.messages = snap.messages.into();
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
        self.events = snap.events.into();
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        self.resume_vcs(now);
        self.questions = snap.questions;
        let questions = &self.questions;
        self.answering
            .retain(|id| questions.iter().any(|q| &q.id == id));
        self.loaded = true;
        self.connected = true;
        self.fix_selection(None);
        self.dirty = true;
    }

    pub fn apply_push(&mut self, push: Push, now: i64) {
        match push {
            Push::Agent(mut agent) => {
                name_model(&self.model_names, &mut agent);
                self.upsert_agent(*agent);
            }
            Push::AgentRemoved { id } => self.remove_agent(&id),
            Push::Task(task) => self.upsert_task(*task),
            Push::Message(msg) => self.add_message(msg, now),
            Push::Event(event) => {
                let fresh = self
                    .events
                    .back()
                    .is_none_or(|last| event.seq == 0 || event.seq > last.seq);
                if fresh {
                    self.start_vcs(&event, now);
                }
                if fresh
                    && matches!(
                        event.event_type.as_str(),
                        crew_core::AGENT_ERROR_EVENT | crew_core::AGENT_REFUSED_EVENT
                    )
                {
                    self.set_notice(event.message.clone(), now);
                }
                self.add_event(event);
            }
            Push::Session(session) => self.session = *session,
            Push::Shutdown => {
                self.daemon_stopped = true;
                self.stopped |= self.stopping;
                self.should_quit = true;
                self.stopping = false;
            }
            // Handled by the connection layer (refetch / reconnect / attach only).
            Push::Unknown
            | Push::Resync
            | Push::Pty { .. }
            | Push::PtyClosed
            | Push::Compact { .. }
            | Push::Deliver { .. } => return,
        }
        self.dirty = true;
    }

    /// Workers in natural id order, each followed by its subagents, then the
    /// orchestrator. This is also the order `tab` walks.
    fn sort_agents(&mut self) {
        let roots: HashMap<String, String> = self
            .agents
            .iter()
            .filter_map(|a| satellite_parent(&self.agents, a).map(|p| (a.id.clone(), p.id.clone())))
            .collect();
        self.agents.sort_by(|a, b| {
            let root_a = roots.get(&a.id).map_or(a.id.as_str(), String::as_str);
            let root_b = roots.get(&b.id).map_or(b.id.as_str(), String::as_str);
            is_orchestrator(a)
                .cmp(&is_orchestrator(b))
                .then_with(|| natural_cmp(root_a, root_b))
                .then_with(|| roots.contains_key(&a.id).cmp(&roots.contains_key(&b.id)))
                .then_with(|| natural_cmp(&a.id, &b.id))
        });
    }

    fn upsert_agent(&mut self, agent: Agent) {
        match self.agents.iter_mut().find(|a| a.id == agent.id) {
            Some(slot) => {
                let reparented = slot.parent != agent.parent;
                *slot = agent;
                if reparented {
                    self.sort_agents();
                }
            }
            None => {
                self.agents.push(agent);
                self.sort_agents();
            }
        }
        self.fix_selection(None);
    }

    fn remove_agent(&mut self, id: &str) {
        let Some(index) = self.agents.iter().position(|a| a.id == id) else {
            return;
        };
        self.agents.remove(index);
        // Subagents of a removed parent rejoin the ring until they go too.
        self.sort_agents();
        self.fix_selection(Some(index));
        if self.view == View::Detail && self.detail.agent == id {
            self.view = View::Dashboard;
        }
        if self.logs.filter_agent.as_deref() == Some(id) {
            self.logs.filter_agent = None;
        }
    }

    /// Keep the selection pointing at a live agent. `hint` is the index the
    /// previously selected agent occupied, so its neighbour takes over.
    fn fix_selection(&mut self, hint: Option<usize>) {
        if self
            .selected
            .as_deref()
            .is_some_and(|id| self.agents.iter().any(|a| a.id == id))
        {
            return;
        }
        if self.agents.is_empty() {
            self.selected = None;
            return;
        }
        let index = hint.unwrap_or(0).min(self.agents.len() - 1);
        self.selected = Some(self.agents[index].id.clone());
    }

    fn upsert_task(&mut self, task: Task) {
        match self.tasks.iter_mut().find(|t| t.id == task.id) {
            Some(slot) => *slot = task,
            None => self.tasks.push(task),
        }
    }

    fn add_message(&mut self, msg: Message, now: i64) {
        // A snapshot taken after subscribing can overlap the push stream.
        if self.messages.iter().rev().take(64).any(|m| m.id == msg.id) {
            return;
        }
        if msg.is_operator_question() {
            if !self.questions.iter().any(|q| q.id == msg.id) {
                self.set_status(
                    format!("? {} has a question for you", msg.asker()),
                    false,
                    now,
                );
                self.questions.push(msg.clone());
            }
        } else if msg.from == OPERATOR_ID && msg.kind == MessageType::Answer {
            // Answered, here or from another terminal.
            if let Some(id) = &msg.reply_to {
                self.questions.retain(|q| &q.id != id);
                self.answering.remove(id);
            }
        }
        let workers: Vec<String> = self.workers().map(|a| a.id.clone()).collect();
        self.anims.on_message(&msg, &workers, now);
        self.messages.push_back(msg);
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }

    /// A VCS event that just arrived starts its animation, unless it is
    /// already old (a reconnect replays what the daemon kept).
    fn start_vcs(&mut self, event: &Event, now: i64) {
        let server_now = self.server_now(now);
        if server_now - event.timestamp > VCS_FRESH_MS {
            return;
        }
        if let Some(vcs) = VcsEvent::from_event(event) {
            self.anims.on_vcs(&vcs, now);
        }
    }

    /// After a snapshot: what still runs shows as running, nothing else.
    fn resume_vcs(&mut self, now: i64) {
        let server_now = self.server_now(now);
        let parsed: Vec<(VcsEvent, i64)> = self
            .events
            .iter()
            .filter_map(|e| Some((VcsEvent::from_event(e)?, (server_now - e.timestamp).max(0))))
            .collect();
        self.anims
            .resume(parsed.iter().map(|(e, age)| (e, *age)), now);
    }

    fn add_event(&mut self, event: Event) {
        if let Some(last) = self.events.back() {
            if event.seq != 0 && event.seq <= last.seq {
                return;
            }
        }
        self.events.push_back(event);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        // Keep a scrolled-back log view anchored on what the user is reading.
        if self.logs.scroll > 0 {
            self.logs.scroll += 1;
        }
    }

    pub fn apply_reply(&mut self, reply: Reply, now: i64) {
        match reply {
            Reply::Output { agent, result } => {
                if agent != self.detail.agent {
                    return;
                }
                self.detail.pending = false;
                self.detail.output_at = Some(now);
                match result {
                    Ok(out) => {
                        self.detail.output = out.text;
                        self.detail.alive = out.alive;
                        self.detail.output_error = None;
                    }
                    Err(e) => self.detail.output_error = Some(e),
                }
            }
            Reply::Transcript { agent, result } => {
                if agent != self.detail.agent {
                    return;
                }
                self.detail.transcript_pending = false;
                match result {
                    Ok(transcript) => self.detail.set_transcript(transcript, now),
                    Err(e) => self.detail.transcript_error = Some(e),
                }
            }
            Reply::Inbox { agent, result } => {
                if agent != self.detail.agent {
                    return;
                }
                if let Ok(entries) = result {
                    self.detail.delivery = entries
                        .into_iter()
                        .map(|e| {
                            (
                                e.message.id,
                                (e.delivered_at.is_some(), e.acked_at.is_some()),
                            )
                        })
                        .collect();
                }
            }
            Reply::Events { agent, result } => {
                if agent != self.detail.agent {
                    return;
                }
                if let Ok(events) = result {
                    self.detail.events = Some(events);
                }
            }
            Reply::Diff { agent, result } => {
                if agent != self.detail.agent {
                    return;
                }
                self.detail.diff = Some(match result {
                    Ok(outcome) => {
                        if let Some(conflict) = outcome.conflict {
                            format!(
                                "{} conflict in: {}\n{}",
                                conflict.operation,
                                conflict.files.join(", "),
                                conflict.detail
                            )
                        } else if outcome.text.trim().is_empty() {
                            "(no uncommitted changes)".to_string()
                        } else {
                            outcome.text
                        }
                    }
                    Err(e) => format!("diff unavailable: {e}"),
                });
            }
            Reply::Action { label, result } => match result {
                Ok(()) => self.set_status(format!("✓ {label}"), false, now),
                Err(e) => self.set_status(format!("{label}: {e}"), true, now),
            },
            Reply::Keys(result) => {
                if let Err(e) = result {
                    self.set_status(format!("keys not delivered: {e}"), true, now);
                }
            }
            Reply::Answered { id, result } => match result {
                Ok(()) => {
                    self.questions.retain(|q| q.id != id);
                    self.answering.remove(&id);
                    self.set_status("✓ answer sent", false, now);
                }
                Err(e) => {
                    // The question is still open: show it again.
                    self.answering.remove(&id);
                    self.set_status(format!("the answer was not sent: {e}"), true, now);
                }
            },
            Reply::Stopped(Ok(())) => self.team_stopped(),
            // The daemon may be gone before its answer got through.
            Reply::Stopped(Err(_)) if !self.connected => self.team_stopped(),
            Reply::Stopped(Err(e)) => {
                self.stopping = false;
                self.set_status(format!("stopping the team: {e}"), true, now);
            }
            Reply::Models(result) => {
                self.models_pending = false;
                match result {
                    Ok(models) => {
                        self.models = models;
                        self.models_error = None;
                    }
                    Err(e) => self.models_error = Some(e),
                }
                // The highlight starts on what the agent runs on, once
                // that is known.
                if let Some(Modal::Model(picker)) = &self.modal {
                    let mut picker = picker.clone();
                    if !picker.moved {
                        picker.row = self.start_row(&picker.agent, picker.runtime);
                    }
                    picker.row = picker
                        .row
                        .min(self.model_rows(picker.runtime).len().saturating_sub(1));
                    self.modal = Some(Modal::Model(picker));
                }
            }
        }
        self.dirty = true;
    }

    // ---- models ----------------------------------------------------------

    /// Open the model picker for agent `id`; returns the listing it needs.
    pub fn open_model_picker(&mut self, id: &str, now: i64) -> Vec<Command> {
        let Some(agent) = self.agent(id) else {
            return Vec::new();
        };
        if agent.runtime == RuntimeKind::Shell {
            self.set_status(
                format!("{id} is a shell agent; it runs no model"),
                true,
                now,
            );
            return Vec::new();
        }
        let runtime = agent.runtime;
        self.modal = Some(Modal::Model(ModelPicker {
            agent: id.to_string(),
            runtime,
            row: self.start_row(id, runtime),
            moved: false,
            notice: None,
        }));
        self.models_pending = true;
        self.dirty = true;
        vec![Command::FetchModels]
    }

    /// The models of one CLI, in the daemon's order.
    pub fn model_rows(&self, runtime: RuntimeKind) -> Vec<&ModelChoice> {
        self.models
            .iter()
            .filter(|m| m.runtime == runtime)
            .collect()
    }

    /// The row of `runtime`'s models that agent `id` runs on now.
    pub fn current_row(&self, id: &str, runtime: RuntimeKind) -> Option<usize> {
        let agent = self.agent(id).filter(|a| a.runtime == runtime)?;
        let model = agent.model.as_deref().unwrap_or("");
        self.model_rows(runtime)
            .iter()
            .position(|m| m.model.eq_ignore_ascii_case(model))
    }

    /// Where the highlight starts in `runtime`'s tab: on the model the
    /// agent runs on, else on the first that can be launched.
    pub fn start_row(&self, id: &str, runtime: RuntimeKind) -> usize {
        self.current_row(id, runtime)
            .or_else(|| {
                self.model_rows(runtime)
                    .iter()
                    .position(|m| m.unavailable.is_none())
            })
            .unwrap_or(0)
    }

    // ---- navigation ------------------------------------------------------

    /// Move the selection through workers then the orchestrator, wrapping.
    pub fn select_step(&mut self, delta: i32) {
        if self.agents.is_empty() {
            self.selected = None;
            return;
        }
        let len = self.agents.len() as i32;
        let current = self
            .selected
            .as_deref()
            .and_then(|id| self.agents.iter().position(|a| a.id == id))
            .map(|i| i as i32);
        let next = match current {
            Some(i) => (i + delta).rem_euclid(len),
            None if delta < 0 => len - 1,
            None => 0,
        };
        self.selected = Some(self.agents[next as usize].id.clone());
        self.dirty = true;
    }

    pub fn select_orchestrator(&mut self) -> bool {
        match self.orchestrator().map(|a| a.id.clone()) {
            Some(id) => {
                self.selected = Some(id);
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// Open the detail view for an agent; returns the fetches it needs.
    pub fn open_detail(&mut self, id: &str, now: i64) -> Vec<Command> {
        self.view = View::Detail;
        self.selected = Some(id.to_string());
        self.detail = DetailState {
            agent: id.to_string(),
            pending: true,
            requested_at: now,
            ..DetailState::default()
        };
        self.dirty = true;
        vec![
            Command::FetchOutput { id: id.to_string() },
            Command::FetchInbox { id: id.to_string() },
            Command::FetchEvents { id: id.to_string() },
        ]
    }
}

/// Give an agent that came without one its model's proper name, when the
/// model is one that is known by name.
fn name_model(names: &HashMap<String, String>, agent: &mut Agent) {
    if agent.model_name.is_some() {
        return;
    }
    let known = [agent.running_model.as_deref(), agent.model.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|model| names.get(&model.to_lowercase()));
    if let Some(name) = known {
        agent.model_name = Some(name.clone());
    }
}

/// Group tasks into dependency layers: layer 0 has no dependencies, layer n
/// depends on something in layer n-1. Returns indices into `tasks`. Unknown
/// dependencies are ignored and cycles are cut rather than looping.
pub fn task_layers(tasks: &[Task]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy)]
    enum Depth {
        Unknown,
        Visiting,
        Known(usize),
    }

    fn depth(i: usize, tasks: &[Task], index: &HashMap<&str, usize>, memo: &mut [Depth]) -> usize {
        match memo[i] {
            Depth::Known(d) => return d,
            Depth::Visiting => return 0,
            Depth::Unknown => {}
        }
        memo[i] = Depth::Visiting;
        let mut d = 0;
        for dep in &tasks[i].dependencies {
            if let Some(&j) = index.get(dep.as_str()) {
                d = d.max(depth(j, tasks, index, memo) + 1);
            }
        }
        memo[i] = Depth::Known(d);
        d
    }

    let index: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.as_str(), i))
        .collect();
    let mut memo = vec![Depth::Unknown; tasks.len()];
    let mut layers: Vec<Vec<usize>> = Vec::new();
    for i in 0..tasks.len() {
        let d = depth(i, tasks, &index, &mut memo);
        if layers.len() <= d {
            layers.resize_with(d + 1, Vec::new);
        }
        layers[d].push(i);
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_shutdown_push_finishes_the_dashboard() {
        let mut app = App::new();
        app.apply_push(Push::Shutdown, 1);
        assert!(app.daemon_stopped && app.should_quit && !app.stopped);
        let mut app = App::new();
        app.stopping = true;
        app.apply_push(Push::Shutdown, 1);
        assert!(app.daemon_stopped && app.should_quit && app.stopped);
    }
    use crate::fixture;
    use crew_core::types::MessageType;

    fn loaded() -> App {
        let mut app = App::new();
        app.apply_snapshot(fixture::snapshot(1_000_000), 1_000_000);
        app
    }

    #[test]
    fn replayed_provider_events_do_not_raise_a_new_error_notice() {
        for kind in ["agent.error", "agent.refused"] {
            for (seq, raises_notice) in [(49, false), (50, false), (51, true), (0, true)] {
                let mut app = loaded();
                app.events.clear();
                app.add_event(fixture::event(
                    50,
                    crew_core::EventLevel::Info,
                    "daemon",
                    "agent.started",
                    Some("claude-1"),
                    "started",
                    1_000_000,
                ));
                app.status = None;
                app.apply_push(
                    Push::Event(fixture::event(
                        seq,
                        crew_core::EventLevel::Error,
                        "daemon",
                        kind,
                        Some("claude-1"),
                        "provider denied this turn",
                        1_000_001,
                    )),
                    1_000_001,
                );
                assert_eq!(app.status.is_some(), raises_notice, "{kind} seq={seq}");
                if let Some(status) = app.status {
                    assert!(status.is_error && status.text.contains("provider denied"));
                }
            }
        }
    }

    #[test]
    fn provider_error_events_are_visible_errors_without_an_unresponsive_diagnosis() {
        let mut app = loaded();
        for kind in ["agent.error", "agent.refused"] {
            let event = fixture::event(
                10_000,
                crew_core::EventLevel::Error,
                "daemon",
                kind,
                Some("claude-1"),
                "claude-1: provider refused this turn",
                1_000_000,
            );
            app.apply_push(Push::Event(event), 1_000_000);
            let status = app
                .status
                .as_ref()
                .expect("provider error must be visible on the dashboard");
            assert!(status.is_error);
            assert!(status.text.contains("provider refused"));
            assert!(!status.text.contains("unresponsive"));
            app.events.clear();
        }
    }

    #[test]
    fn questions_for_the_operator_stay_open_until_answered() {
        let mut app = loaded();
        assert!(app.open_questions().is_empty());
        let question = fixture::question(1_000_000);
        app.apply_push(Push::Message(question.clone()), 5);
        app.apply_push(Push::Message(question.clone()), 6);
        assert_eq!(
            app.open_questions().len(),
            1,
            "a repeated push is one question"
        );
        assert!(app
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("orchestrator, for claude-2 has a question"));

        // While the answer is on its way the question is out of view; a
        // failed send brings it back.
        app.answering.insert(question.id.clone());
        assert!(app.open_questions().is_empty());
        app.apply_reply(
            Reply::Answered {
                id: question.id.clone(),
                result: Err("daemon unreachable".into()),
            },
            7,
        );
        assert_eq!(app.open_questions().len(), 1);
        assert!(app.status.as_ref().unwrap().is_error);

        // The answer on the bus closes it, whoever gave it.
        let mut answer = question.clone();
        answer.id = "m-answer".into();
        answer.from = OPERATOR_ID.into();
        answer.to = "orchestrator".into();
        answer.kind = MessageType::Answer;
        answer.reply_to = Some(question.id.clone());
        app.apply_push(Push::Message(answer), 8);
        assert!(app.questions.is_empty());

        // A snapshot brings the open questions with it.
        let mut snap = fixture::snapshot(1_000_000);
        snap.questions = vec![question.clone()];
        app.answering.insert("m-gone".into());
        app.apply_snapshot(snap, 1_000_000);
        assert_eq!(app.open_questions().len(), 1);
        assert!(app.answering.is_empty());
        app.apply_reply(
            Reply::Answered {
                id: question.id,
                result: Ok(()),
            },
            9,
        );
        assert!(app.questions.is_empty());
    }

    #[test]
    fn agents_without_a_model_name_get_one_from_the_known_models() {
        let mut app = App::new();
        app.model_names = HashMap::from([
            ("opus".to_string(), "Opus 5.5".to_string()),
            ("claude-fable-5-1".to_string(), "Fable 5.1".to_string()),
        ]);
        let mut snap = fixture::snapshot(1_000_000);
        for agent in &mut snap.agents {
            agent.model_name = None;
            agent.running_model = None;
        }
        snap.agents[0].model = Some("Opus".into());
        snap.agents[1].model = Some("house-model".into());
        snap.agents[2].model_name = Some("Named By The Daemon".into());
        let (first, second, third) = (
            snap.agents[0].id.clone(),
            snap.agents[1].id.clone(),
            snap.agents[2].id.clone(),
        );
        app.apply_snapshot(snap, 1_000_000);
        let label = |app: &App, id: &str| app.agent(id).unwrap().model_label();
        assert_eq!(label(&app, &first).as_deref(), Some("Opus 5.5"));
        // A model nobody listed is named after its id; the daemon's word stands.
        assert_eq!(label(&app, &second).as_deref(), Some("House Model"));
        assert_eq!(label(&app, &third).as_deref(), Some("Named By The Daemon"));
        // The same on an update, by what the session reports.
        let mut agent = app.agent(&second).unwrap().clone();
        agent.running_model = Some("claude-fable-5-1".into());
        app.apply_push(Push::Agent(Box::new(agent)), 5);
        assert_eq!(label(&app, &second).as_deref(), Some("Fable 5.1"));
    }

    #[test]
    fn stopping_the_team_closes_the_dashboard() {
        // The daemon confirms the stop.
        let mut app = loaded();
        app.stopping = true;
        app.apply_reply(Reply::Stopped(Ok(())), 0);
        assert!(app.stopped && app.should_quit && !app.stopping);

        // The daemon is gone before (or instead of) its answer.
        let mut app = loaded();
        app.stopping = true;
        app.disconnected();
        assert!(app.stopped && app.should_quit);
        let mut app = loaded();
        app.stopping = true;
        app.connected = false;
        app.apply_reply(Reply::Stopped(Err("connection closed".into())), 0);
        assert!(app.stopped && app.should_quit);

        // A refused stop leaves the dashboard open and says why.
        let mut app = loaded();
        app.stopping = true;
        app.apply_reply(Reply::Stopped(Err("operator only".into())), 0);
        assert!(!app.stopped && !app.should_quit && !app.stopping);
        let status = app.status.as_ref().unwrap();
        assert!(status.is_error && status.text.contains("operator only"));

        // A disconnect nobody asked for is not a stop.
        let mut app = loaded();
        app.disconnected();
        assert!(!app.connected && !app.stopped && !app.should_quit);
    }

    #[test]
    fn natural_ordering() {
        let mut ids = vec!["claude-10", "claude-2", "claude-1", "db", "architect"];
        ids.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            ids,
            ["architect", "claude-1", "claude-2", "claude-10", "db"]
        );
    }

    #[test]
    fn snapshot_orders_agents_and_selects_the_first_worker() {
        let app = loaded();
        let ids: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-1",
                "claude-2",
                "claude-3",
                "claude-4",
                "claude-5",
                "claude-6",
                "orchestrator"
            ]
        );
        assert_eq!(app.workers().count(), 6);
        assert_eq!(app.orchestrator().unwrap().id, "orchestrator");
        assert_eq!(app.selected.as_deref(), Some("claude-1"));
        assert!(app.loaded && app.connected);
    }

    #[test]
    fn agent_pushes_upsert_in_order() {
        let mut app = loaded();
        let mut changed = app.agent("claude-3").unwrap().clone();
        changed.status = AgentStatus::Failed;
        app.apply_push(Push::Agent(Box::new(changed)), 0);
        assert_eq!(app.agent("claude-3").unwrap().status, AgentStatus::Failed);
        assert_eq!(app.agents.len(), 7);

        let mut added = app.agent("claude-1").unwrap().clone();
        added.id = "claude-10".into();
        app.apply_push(Push::Agent(Box::new(added)), 0);
        let ids: Vec<&str> = app.workers().map(|a| a.id.as_str()).collect();
        assert_eq!(ids.last(), Some(&"claude-10"));
        assert_eq!(ids[1], "claude-2");
        assert_eq!(app.agents.last().unwrap().id, "orchestrator");
    }

    #[test]
    fn selection_survives_agents_coming_and_going() {
        let mut app = loaded();
        app.selected = Some("claude-3".into());
        // Unrelated removal keeps the selection.
        app.apply_push(
            Push::AgentRemoved {
                id: "claude-5".into(),
            },
            0,
        );
        assert_eq!(app.selected.as_deref(), Some("claude-3"));
        // Removing the selected agent moves to its neighbour.
        app.apply_push(
            Push::AgentRemoved {
                id: "claude-3".into(),
            },
            0,
        );
        assert_eq!(app.selected.as_deref(), Some("claude-4"));
        // Removing the last entry falls back to the new last.
        app.selected = Some("orchestrator".into());
        app.apply_push(
            Push::AgentRemoved {
                id: "orchestrator".into(),
            },
            0,
        );
        assert_eq!(app.selected.as_deref(), Some("claude-6"));
        for id in ["claude-1", "claude-2", "claude-4", "claude-6"] {
            app.apply_push(Push::AgentRemoved { id: id.into() }, 0);
        }
        assert_eq!(app.selected, None);
        // Unknown ids are ignored.
        app.apply_push(Push::AgentRemoved { id: "ghost".into() }, 0);
        assert!(app.agents.is_empty());
    }

    #[test]
    fn subagents_attach_to_their_parent() {
        let mut app = loaded();
        let sub = fixture::subagent(
            "claude-3-s1",
            "claude-3",
            "tests",
            AgentStatus::Working,
            Some(40),
            0,
        );
        app.apply_push(Push::Agent(Box::new(sub)), 0);
        // Listed right after its parent, which is also the order tab walks.
        let ids: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "claude-1",
                "claude-2",
                "claude-3",
                "claude-3-s1",
                "claude-4",
                "claude-5",
                "claude-6",
                "orchestrator"
            ]
        );
        assert_eq!(app.top_level().count(), 6, "the ring is unchanged");
        assert_eq!(app.workers().count(), 7);
        let children: Vec<&str> = app
            .subagents_of("claude-3")
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(children, ["claude-3-s1"]);
        assert_eq!(
            app.satellite_parent(app.agent("claude-3-s1").unwrap())
                .map(|a| a.id.as_str()),
            Some("claude-3")
        );
        app.selected = Some("claude-3".into());
        app.select_step(1);
        assert_eq!(app.selected.as_deref(), Some("claude-3-s1"));
        app.select_step(1);
        assert_eq!(app.selected.as_deref(), Some("claude-4"));

        // A second subagent sorts after the first; one of the orchestrator's
        // has no box to orbit and joins the ring.
        let second = fixture::subagent(
            "claude-3-s2",
            "claude-3",
            "docs",
            AgentStatus::Spawning,
            None,
            0,
        );
        app.apply_push(Push::Agent(Box::new(second)), 0);
        let hub_child = fixture::subagent(
            "scout",
            "orchestrator",
            "research",
            AgentStatus::Working,
            None,
            0,
        );
        app.apply_push(Push::Agent(Box::new(hub_child)), 0);
        assert_eq!(app.subagents_of("claude-3").count(), 2);
        assert_eq!(app.top_level().count(), 7);
        assert!(app.satellite_parent(app.agent("scout").unwrap()).is_none());
    }

    #[test]
    fn removing_a_parent_or_a_child_keeps_the_state_consistent() {
        let mut app = App::new();
        app.apply_snapshot(fixture::with_subagents(1_000_000), 1_000_000);
        assert_eq!(app.subagents_of("claude-3").count(), 1);

        // The child goes: its parent is untouched and selection moves on.
        app.selected = Some("claude-3-s1".into());
        app.apply_push(
            Push::AgentRemoved {
                id: "claude-3-s1".into(),
            },
            0,
        );
        assert_eq!(app.subagents_of("claude-3").count(), 0);
        assert_eq!(app.selected.as_deref(), Some("claude-4"));
        assert_eq!(app.top_level().count(), 6);

        // The parent goes while a child is still around: the orphan rejoins
        // the ring instead of pointing at a box that no longer exists.
        let sub = fixture::subagent(
            "claude-3-s1",
            "claude-3",
            "tests",
            AgentStatus::Working,
            None,
            0,
        );
        app.apply_push(Push::Agent(Box::new(sub)), 0);
        app.selected = Some("claude-3-s1".into());
        app.apply_push(
            Push::AgentRemoved {
                id: "claude-3".into(),
            },
            0,
        );
        assert!(app.agent("claude-3").is_none());
        assert_eq!(app.selected.as_deref(), Some("claude-3-s1"));
        let orphan = app.agent("claude-3-s1").unwrap();
        assert!(app.satellite_parent(orphan).is_none());
        assert_eq!(app.top_level().count(), 6);
        assert_eq!(app.subagents_of("claude-3").count(), 0);
        // And then the orphan itself.
        app.apply_push(
            Push::AgentRemoved {
                id: "claude-3-s1".into(),
            },
            0,
        );
        assert_eq!(app.top_level().count(), 5);
        assert!(app.selected.is_some());
    }

    #[test]
    fn a_team_can_start_empty_and_grow_one_member_at_a_time() {
        let mut app = App::new();
        let mut snap = fixture::team(0, 1_000_000);
        snap.tasks.clear();
        app.apply_snapshot(snap, 1_000_000);
        assert_eq!(app.top_level().count(), 0);
        assert_eq!(app.selected.as_deref(), Some("orchestrator"));
        for id in ["backend", "db", "tests"] {
            let member = fixture::agent(id, id, AgentStatus::Spawning, None, 0);
            app.apply_push(Push::Agent(Box::new(member)), 0);
            assert_eq!(
                app.selected.as_deref(),
                Some("orchestrator"),
                "selection is stable"
            );
        }
        assert_eq!(app.top_level().count(), 3);
        app.apply_push(Push::AgentRemoved { id: "db".into() }, 0);
        assert_eq!(app.top_level().count(), 2);
    }

    #[test]
    fn selection_cycles_and_wraps() {
        let mut app = loaded();
        app.select_step(1);
        assert_eq!(app.selected.as_deref(), Some("claude-2"));
        app.select_step(-2);
        assert_eq!(app.selected.as_deref(), Some("orchestrator"));
        app.select_step(1);
        assert_eq!(app.selected.as_deref(), Some("claude-1"));
        assert!(app.select_orchestrator());
        assert_eq!(app.selected.as_deref(), Some("orchestrator"));
    }

    #[test]
    fn message_log_is_capped_and_deduplicated() {
        let mut app = loaded();
        let template = app.messages.back().unwrap().clone();
        let before = app.messages.len();
        app.apply_push(Push::Message(template.clone()), 5);
        assert_eq!(app.messages.len(), before, "duplicate id must be ignored");
        for i in 0..(MAX_MESSAGES + 50) {
            let mut m = template.clone();
            m.id = format!("m-new-{i}");
            m.body = format!("body {i}");
            app.apply_push(Push::Message(m), 5);
        }
        assert_eq!(app.messages.len(), MAX_MESSAGES);
        assert_eq!(
            app.messages.back().unwrap().body,
            format!("body {}", MAX_MESSAGES + 49)
        );
    }

    #[test]
    fn p2p_message_starts_a_direct_animation() {
        let mut app = loaded();
        app.anims = Anims::default();
        let mut m = app.messages.back().unwrap().clone();
        m.id = "m-p2p".into();
        m.from = "claude-3".into();
        m.to = "claude-5".into();
        m.kind = MessageType::Question;
        app.apply_push(Push::Message(m), 10_000);
        assert!(app.anims.p2p_live(10_000));
        assert_eq!(app.next_wake(10_000), Duration::from_millis(33));
        app.tick(10_000 + crate::anim::LINK_MS);
        assert!(!app.anims.p2p_live(10_000 + crate::anim::LINK_MS));
    }

    #[test]
    fn scrolling_the_inspector_up_reads_the_transcript_once() {
        use crew_core::types::{TranscriptEntry, TranscriptKind};

        let mut app = loaded();
        app.open_detail("claude-3", 0);
        app.detail.pending = false;
        app.detail.output_at = Some(0);
        // At the live terminal nothing but the screen is asked for.
        assert_eq!(
            app.due_commands(2000),
            vec![Command::FetchOutput {
                id: "claude-3".into()
            }]
        );
        app.detail.pending = false;
        app.detail.output_at = Some(2000);

        // Scrolled up, the transcript is: once, however long it takes.
        app.detail.scroll = 3;
        let fetch = vec![Command::FetchTranscript {
            id: "claude-3".into(),
        }];
        assert_eq!(app.due_commands(2100), fetch);
        assert!(app.detail.transcript_pending);
        assert!(app.due_commands(2200).is_empty());
        let transcript = TranscriptResult {
            entries: vec![TranscriptEntry {
                kind: TranscriptKind::Reply,
                at: None,
                text: "one two three four five six".into(),
            }],
            available: true,
            truncated: false,
        };
        let reply = |agent: &str| Reply::Transcript {
            agent: agent.into(),
            result: Ok(transcript.clone()),
        };
        // An answer about another agent is not this pane's.
        app.apply_reply(reply("claude-1"), 2300);
        assert!(app.detail.transcript.is_none());
        app.apply_reply(reply("claude-3"), 2300);
        assert!(!app.detail.transcript_pending);
        // It is laid out for the pane's width, and again when that changes.
        assert_eq!(app.detail.transcript_lines(40).unwrap().len(), 1);
        assert_eq!(app.detail.transcript_lines(12).unwrap().len(), 3);
        // It stays as read while the reader is in it.
        assert!(app.due_commands(2400).is_empty());
        assert!(app
            .due_commands(60_000)
            .iter()
            .all(|c| !matches!(c, Command::FetchTranscript { .. })));

        // Back at the terminal and soon up again: what was read is shown.
        app.detail.scroll = 0;
        app.detail.pending = true;
        assert!(app.due_commands(61_000).is_empty());
        app.detail.scroll = 1;
        app.detail.transcript_at = Some(60_500);
        assert!(app.due_commands(61_500).is_empty());
        // Later, it is read afresh.
        app.detail.scroll = 0;
        assert!(app.due_commands(62_000).is_empty());
        app.detail.scroll = 1;
        assert_eq!(app.due_commands(90_000), fetch);

        // A failure is shown, not retried in a loop, until the pane is left.
        app.apply_reply(
            Reply::Transcript {
                agent: "claude-3".into(),
                result: Err("no".into()),
            },
            90_100,
        );
        assert_eq!(app.detail.transcript_error.as_deref(), Some("no"));
        assert!(app.due_commands(99_000).is_empty());
        app.detail.scroll = 0;
        assert!(app.due_commands(99_100).is_empty());
        assert!(app.detail.transcript_error.is_none());
        // An unanswered request is given up on.
        app.detail.scroll = 2;
        app.detail.transcript_at = None;
        assert_eq!(app.due_commands(100_000), fetch);
        app.tick(100_000 + OUTPUT_TIMEOUT_MS + 1);
        assert!(!app.detail.transcript_pending && app.detail.transcript_error.is_some());
    }

    #[test]
    fn events_are_ordered_capped_and_keep_scroll_anchor() {
        let mut app = loaded();
        let last = app.events.back().unwrap().clone();
        let before = app.events.len();
        app.apply_push(Push::Event(last.clone()), 0);
        assert_eq!(app.events.len(), before, "stale seq must be ignored");
        app.logs.scroll = 3;
        let mut next = last.clone();
        next.seq += 1;
        app.apply_push(Push::Event(next), 0);
        assert_eq!(app.events.len(), before + 1);
        assert_eq!(app.logs.scroll, 4);
    }

    #[test]
    fn task_pushes_upsert_and_count() {
        let mut app = loaded();
        let (done, total) = app.task_counts();
        let mut t = app
            .tasks
            .iter()
            .find(|t| t.status != TaskStatus::Done)
            .unwrap()
            .clone();
        t.status = TaskStatus::Done;
        app.apply_push(Push::Task(Box::new(t.clone())), 0);
        assert_eq!(app.task_counts(), (done + 1, total));
        t.id = "brand-new".into();
        app.apply_push(Push::Task(Box::new(t)), 0);
        assert_eq!(app.task_counts(), (done + 2, total + 1));
    }

    #[test]
    fn replies_update_only_the_open_detail_view() {
        let mut app = loaded();
        let cmds = app.open_detail("claude-3", 100);
        assert_eq!(cmds.len(), 3);
        assert!(app.detail.pending);
        app.apply_reply(
            Reply::Output {
                agent: "claude-1".into(),
                result: Ok(OutputResult {
                    text: "other".into(),
                    alive: true,
                }),
            },
            200,
        );
        assert!(app.detail.output.is_empty());
        app.apply_reply(
            Reply::Output {
                agent: "claude-3".into(),
                result: Ok(OutputResult {
                    text: "hello".into(),
                    alive: true,
                }),
            },
            200,
        );
        assert_eq!(app.detail.output, "hello");
        assert!(!app.detail.pending);
        // Not due again until the refresh interval has passed.
        assert!(app.due_commands(300).is_empty());
        let due = app.due_commands(200 + OUTPUT_REFRESH_MS);
        assert_eq!(
            due,
            vec![Command::FetchOutput {
                id: "claude-3".into()
            }]
        );
        assert!(app.due_commands(200 + OUTPUT_REFRESH_MS + 1).is_empty());

        app.apply_reply(
            Reply::Action {
                label: "kill claude-3".into(),
                result: Err("not_found: nope".into()),
            },
            500,
        );
        assert!(app.status.as_ref().unwrap().is_error);
        app.tick(500 + 6000);
        assert!(app.status.is_none());
    }

    #[test]
    fn layers_follow_dependencies_and_tolerate_cycles() {
        let app = loaded();
        let layers = task_layers(&app.tasks);
        let layer_of = |id: &str| {
            layers
                .iter()
                .position(|l| l.iter().any(|&i| app.tasks[i].id == id))
                .unwrap()
        };
        for t in &app.tasks {
            for dep in &t.dependencies {
                assert!(
                    layer_of(dep) < layer_of(&t.id),
                    "{dep} must precede {}",
                    t.id
                );
            }
        }
        assert_eq!(layers.iter().map(Vec::len).sum::<usize>(), app.tasks.len());

        let mut cyclic = app.tasks.clone();
        let first = cyclic[0].id.clone();
        let second = cyclic[1].id.clone();
        cyclic[0].dependencies = vec![second];
        cyclic[1].dependencies = vec![first, "missing".into()];
        let layers = task_layers(&cyclic);
        assert_eq!(layers.iter().map(Vec::len).sum::<usize>(), cyclic.len());
    }

    #[test]
    fn idle_app_wakes_about_once_a_second() {
        let mut app = App::new();
        app.take_dirty();
        let wake = app.next_wake(10_250);
        assert_eq!(wake, Duration::from_millis(750));
        let mut busy = loaded();
        busy.anims = Anims::default();
        assert!(busy.next_wake(10_250) <= Duration::from_millis(SPINNER_MS as u64));
    }

    // ---- version control animations ---------------------------------------

    use crate::anim::{MarkKind, MARK_MS, MARK_SWEEP_MS, RUNNING_MAX_MS};
    use crew_core::vcs::{VcsAction, VcsOutcome};

    const T: i64 = 1_000_000;

    /// A team with nothing working: no spinner, nothing moving.
    fn calm() -> App {
        let mut app = loaded();
        for agent in &mut app.agents {
            agent.status = AgentStatus::Done;
        }
        for task in &mut app.tasks {
            task.status = TaskStatus::Done;
        }
        app.anims = Anims::default();
        app
    }

    fn vcs_push(app: &mut App, seq: i64, ts: i64, now: i64, event: VcsEvent) {
        app.apply_push(Push::Event(fixture::vcs_event(seq, ts, "vcs", event)), now);
    }

    fn commit_event(agent: &str, outcome: VcsOutcome, op: &str) -> VcsEvent {
        VcsEvent {
            commit: Some("1a2b3c4d5e6f".into()),
            ..fixture::vcs(VcsAction::Commit, outcome, op, agent)
        }
    }

    fn merge_event(outcome: VcsOutcome, op: &str) -> VcsEvent {
        VcsEvent {
            peer: Some("claude-5".into()),
            ..fixture::vcs(VcsAction::Merge, outcome, op, "claude-3")
        }
    }

    #[test]
    fn a_live_commit_wakes_at_frame_rate_then_settles() {
        let mut app = calm();
        app.tick(T);
        app.take_dirty();
        assert!(app.next_wake(T) > Duration::from_millis(33));
        vcs_push(
            &mut app,
            900,
            T,
            T,
            commit_event("claude-3", VcsOutcome::Ok, "v1"),
        );
        assert!(app.take_dirty());
        assert_eq!(app.next_wake(T + 10), Duration::from_millis(33));
        app.tick(T + MARK_SWEEP_MS - 10);
        assert!(app.take_dirty());
        // The sweep is over: one more redraw clears its last frame ...
        app.tick(T + MARK_SWEEP_MS + 5);
        assert!(app.take_dirty());
        // ... then nothing until the label expires.
        app.tick(T + MARK_SWEEP_MS + 60);
        assert!(!app.take_dirty());
        let wake = app.next_wake(T + MARK_SWEEP_MS + 60);
        assert!(wake > Duration::from_millis(33), "{wake:?}");
        app.tick(T + MARK_MS);
        assert!(app.take_dirty());
        assert!(app.anims.marks.is_empty());
        // Idle again: about once a second.
        let wake = app.next_wake(T + MARK_MS + 250);
        assert!(wake > Duration::from_millis(33) && wake <= Duration::from_millis(1000));
    }

    #[test]
    fn a_running_merge_wakes_at_the_spinner_rate_until_it_ends() {
        let mut app = calm();
        app.take_dirty();
        vcs_push(&mut app, 900, T, T, merge_event(VcsOutcome::Started, "v1"));
        assert!(app.take_dirty());
        assert!(app.anims.running());
        // 50 ms in is a spinner step: the next one is a full step away, not a frame.
        assert_eq!(
            app.next_wake(T + 50),
            Duration::from_millis(SPINNER_MS as u64)
        );
        // The spinner turns: the app redraws each step.
        app.tick(T + 20);
        app.take_dirty();
        app.tick(T + SPINNER_MS + 20);
        assert!(app.take_dirty());
        // Long after, still running (until the cap), still not frame rate.
        assert_eq!(
            app.next_wake(T + 300_050),
            Duration::from_millis(SPINNER_MS as u64)
        );
        vcs_push(
            &mut app,
            901,
            T + 400,
            T + 400,
            merge_event(VcsOutcome::Ok, "v1"),
        );
        assert!(!app.anims.running());
        assert_eq!(app.next_wake(T + 410), Duration::from_millis(33));
        // A start that never ends is dropped at the cap.
        vcs_push(
            &mut app,
            902,
            T + 5000,
            T + 5000,
            merge_event(VcsOutcome::Started, "v2"),
        );
        app.tick(T + 5000 + RUNNING_MAX_MS);
        assert!(!app.anims.running());
    }

    #[test]
    fn a_snapshot_restores_what_still_runs_and_nothing_else() {
        let snap = |events: Vec<VcsEvent>| {
            let mut snap = fixture::snapshot(T);
            for (i, e) in events.into_iter().enumerate() {
                snap.events.push(fixture::vcs_event(
                    900 + i as i64,
                    T - 5000 + i as i64,
                    "vcs",
                    e,
                ));
            }
            snap
        };
        let mut app = calm();
        app.apply_snapshot(snap(vec![merge_event(VcsOutcome::Started, "v1")]), T);
        assert!(app.anims.running());
        assert!(matches!(app.anims.marks[0].kind, MarkKind::Running { .. }));
        assert!(app.anims.links.iter().any(|l| l.held.is_some()));
        assert!(!app.anims.animating(T));
        // Started and ended, and plain history: nothing shows.
        let mut app = calm();
        app.apply_snapshot(
            snap(vec![
                merge_event(VcsOutcome::Started, "v1"),
                merge_event(VcsOutcome::Ok, "v1"),
                commit_event("claude-1", VcsOutcome::Ok, "v2"),
                fixture::vcs(VcsAction::Status, VcsOutcome::Ok, "v3", "claude-2"),
            ]),
            T,
        );
        assert!(app.anims.marks.is_empty() && app.anims.links.is_empty());
        // A reconnect clears what a snapshot no longer says is running.
        let mut app = calm();
        app.apply_snapshot(snap(vec![merge_event(VcsOutcome::Started, "v1")]), T);
        app.apply_snapshot(snap(vec![]), T + 100);
        assert!(app.anims.marks.is_empty() && app.anims.links.is_empty());
    }

    #[test]
    fn old_and_unknown_vcs_events_start_nothing() {
        let mut app = calm();
        // 11 s old.
        vcs_push(
            &mut app,
            900,
            T - 11_000,
            T,
            commit_event("claude-1", VcsOutcome::Ok, "v1"),
        );
        assert!(app.anims.marks.is_empty());
        // Just inside the window.
        vcs_push(
            &mut app,
            901,
            T - 9_000,
            T,
            commit_event("claude-1", VcsOutcome::Ok, "v2"),
        );
        assert_eq!(app.anims.marks.len(), 1);
        // An action this build does not know: logged, nothing animates.
        let mut app = calm();
        let mut unknown = fixture::vcs_event(
            900,
            T,
            "vcs",
            commit_event("claude-1", VcsOutcome::Ok, "v3"),
        );
        unknown.event_type = "vcs.rebase".into();
        unknown.data["action"] = serde_json::json!("rebase");
        app.apply_push(Push::Event(unknown.clone()), T);
        let mut unknown_outcome = fixture::vcs_event(
            901,
            T,
            "vcs",
            commit_event("claude-1", VcsOutcome::Ok, "v4"),
        );
        unknown_outcome.data["outcome"] = serde_json::json!("deferred");
        app.apply_push(Push::Event(unknown_outcome), T);
        assert!(app.anims.marks.is_empty());
        assert!(app.events.iter().any(|e| e.event_type == "vcs.rebase"));
        assert_eq!(app.events.iter().filter(|e| e.seq >= 900).count(), 2);
        // A replayed event (stale seq) is not new either.
        vcs_push(
            &mut app,
            901,
            T,
            T,
            commit_event("claude-1", VcsOutcome::Ok, "v5"),
        );
        assert!(app.anims.marks.is_empty());
    }
}
