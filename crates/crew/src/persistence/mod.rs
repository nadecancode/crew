//! Storage interfaces. The daemon only talks to these traits; SQLite is the
//! one implementation today (`sqlite.rs`).

use std::collections::HashMap;

use anyhow::Result;
use crew_core::{Agent, Event, EventLevel, InboxEntry, Message, Task};
use serde_json::Value;

pub mod sqlite;

pub use sqlite::SqliteStore;

/// An event before the store assigns its sequence number.
#[derive(Clone, Debug)]
pub struct NewEvent {
    pub source: String,
    pub event_type: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub message: String,
    pub level: EventLevel,
    pub data: Value,
}

impl NewEvent {
    pub fn new(source: &str, event_type: &str, message: impl Into<String>) -> Self {
        Self {
            source: source.to_string(),
            event_type: event_type.to_string(),
            agent_id: None,
            task_id: None,
            message: message.into(),
            level: EventLevel::Info,
            data: Value::Null,
        }
    }

    pub fn agent(mut self, id: &str) -> Self {
        self.agent_id = Some(id.to_string());
        self
    }

    pub fn task(mut self, id: &str) -> Self {
        self.task_id = Some(id.to_string());
        self
    }

    pub fn level(mut self, level: EventLevel) -> Self {
        self.level = level;
        self
    }

    pub fn warn(self) -> Self {
        self.level(EventLevel::Warn)
    }

    pub fn error(self) -> Self {
        self.level(EventLevel::Error)
    }

    pub fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}

/// Inbox/outbox counters shown per agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageCounts {
    pub unread: u32,
    pub inbox_total: u32,
    pub outbox_total: u32,
}

pub trait AgentStore: Send + Sync {
    fn save_agent(&self, agent: &Agent) -> Result<()>;
    /// Agents in the order they were first saved.
    fn load_agents(&self) -> Result<Vec<Agent>>;
    fn delete_agent(&self, id: &str) -> Result<()>;
    fn save_token(&self, agent_id: &str, token: &str) -> Result<()>;
    fn load_tokens(&self) -> Result<HashMap<String, String>>;
}

pub trait TaskStore: Send + Sync {
    fn save_task(&self, task: &Task) -> Result<()>;
    /// Tasks in creation order.
    fn load_tasks(&self) -> Result<Vec<Task>>;
}

pub trait EventStore: Send + Sync {
    fn append_event(&self, event: NewEvent) -> Result<Event>;
    /// The most recent `limit` events, oldest first.
    fn recent_events(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Event>>;
}

pub trait MessageStore: Send + Sync {
    /// Persist a message and one delivery row per recipient. Returns `false`
    /// (and stores nothing) when a message with this id already exists.
    fn insert_message(&self, message: &Message, recipients: &[String]) -> Result<bool>;
    fn get_message(&self, id: &str) -> Result<Option<Message>>;
    fn recipients(&self, message_id: &str) -> Result<Vec<String>>;
    /// Messages addressed to `agent`, oldest first.
    fn inbox(&self, agent: &str, unread_only: bool, limit: usize) -> Result<Vec<InboxEntry>>;
    /// Messages not yet injected into the agent's session, oldest first.
    fn undelivered(&self, agent: &str) -> Result<Vec<Message>>;
    fn mark_delivered(&self, agent: &str, ids: &[String], at: i64) -> Result<()>;
    /// Acknowledge the given messages, or everything unread when `ids` is `None`.
    fn acknowledge(&self, agent: &str, ids: Option<&[String]>, at: i64) -> Result<usize>;
    /// The most recent `limit` messages (optionally only those involving
    /// `agent`), oldest first.
    fn history(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Message>>;
    fn counts(&self, agent: &str) -> Result<MessageCounts>;
}

pub trait MetaStore: Send + Sync {
    fn get_meta(&self, key: &str) -> Result<Option<String>>;
    fn set_meta(&self, key: &str, value: &str) -> Result<()>;
    fn delete_meta(&self, key: &str) -> Result<()>;
}

pub trait SessionStore: Send + Sync {
    /// Forget the previous team (agents, tasks, messages). The event log stays.
    fn clear_session(&self) -> Result<()>;
}

/// Everything the daemon persists.
pub trait Store:
    AgentStore + TaskStore + EventStore + MessageStore + MetaStore + SessionStore
{
}

impl<T: AgentStore + TaskStore + EventStore + MessageStore + MetaStore + SessionStore> Store for T {}
