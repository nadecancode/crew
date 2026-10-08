//! SQLite implementation of the stores.
//!
//! Agents and tasks are stored as JSON documents (their shape evolves with the
//! protocol); messages, deliveries and events get real columns because they
//! are queried by sender, recipient and time.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use crew_core::util::now_ms;
use crew_core::{Agent, Event, EventLevel, InboxEntry, Message, MessageType, Task};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use super::{
    AgentStore, EventStore, MessageCounts, MessageStore, MetaStore, NewEvent, SessionStore,
    TaskStore,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agents (
    id       TEXT PRIMARY KEY,
    position INTEGER NOT NULL,
    data     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tokens (
    agent_id TEXT PRIMARY KEY,
    token    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
    id       TEXT PRIMARY KEY,
    position INTEGER NOT NULL,
    data     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    id             TEXT NOT NULL UNIQUE,
    sender         TEXT NOT NULL,
    recipient      TEXT NOT NULL,
    kind           TEXT NOT NULL,
    body           TEXT NOT NULL,
    timestamp      INTEGER NOT NULL,
    correlation_id TEXT,
    reply_to       TEXT,
    metadata       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_sender ON messages (sender, seq);
CREATE TABLE IF NOT EXISTS deliveries (
    message_id   TEXT NOT NULL,
    recipient    TEXT NOT NULL,
    delivered_at INTEGER,
    acked_at     INTEGER,
    PRIMARY KEY (message_id, recipient)
);
CREATE INDEX IF NOT EXISTS deliveries_recipient ON deliveries (recipient, acked_at);
CREATE TABLE IF NOT EXISTS events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp  INTEGER NOT NULL,
    source     TEXT NOT NULL,
    event_type TEXT NOT NULL,
    agent_id   TEXT,
    task_id    TEXT,
    message    TEXT NOT NULL,
    level      TEXT NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_agent ON events (agent_id, seq);
"#;

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL keeps readers (e.g. `sqlite3` inspection) from blocking the daemon.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.execute_batch(SCHEMA).context("creating schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn upsert_doc(conn: &Connection, table: &str, id: &str, data: &str) -> Result<()> {
    // `table` is one of two compile-time constants, never user input.
    let sql = format!(
        "INSERT INTO {table} (id, position, data)
         VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM {table}), ?2)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data"
    );
    conn.execute(&sql, params![id, data])?;
    Ok(())
}

fn load_docs<T: serde::de::DeserializeOwned>(conn: &Connection, table: &str) -> Result<Vec<T>> {
    let sql = format!("SELECT data FROM {table} ORDER BY position");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let data = row?;
        out.push(serde_json::from_str(&data).with_context(|| format!("decoding {table} row"))?);
    }
    Ok(out)
}

impl AgentStore for SqliteStore {
    fn save_agent(&self, agent: &Agent) -> Result<()> {
        upsert_doc(
            &self.conn(),
            "agents",
            &agent.id,
            &serde_json::to_string(agent)?,
        )
    }

    fn load_agents(&self) -> Result<Vec<Agent>> {
        load_docs(&self.conn(), "agents")
    }

    fn delete_agent(&self, id: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM agents WHERE id = ?1", params![id])?;
        conn.execute("DELETE FROM tokens WHERE agent_id = ?1", params![id])?;
        Ok(())
    }

    fn save_token(&self, agent_id: &str, token: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO tokens (agent_id, token) VALUES (?1, ?2)
             ON CONFLICT(agent_id) DO UPDATE SET token = excluded.token",
            params![agent_id, token],
        )?;
        Ok(())
    }

    fn load_tokens(&self) -> Result<HashMap<String, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT agent_id, token FROM tokens")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, token): (String, String) = row?;
            out.insert(id, token);
        }
        Ok(out)
    }
}

impl TaskStore for SqliteStore {
    fn save_task(&self, task: &Task) -> Result<()> {
        upsert_doc(
            &self.conn(),
            "tasks",
            &task.id,
            &serde_json::to_string(task)?,
        )
    }

    fn load_tasks(&self) -> Result<Vec<Task>> {
        load_docs(&self.conn(), "tasks")
    }
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let level: String = row.get(7)?;
    let data: String = row.get(8)?;
    Ok(Event {
        seq: row.get(0)?,
        timestamp: row.get(1)?,
        source: row.get(2)?,
        event_type: row.get(3)?,
        agent_id: row.get(4)?,
        task_id: row.get(5)?,
        message: row.get(6)?,
        level: level.parse().unwrap_or(EventLevel::Info),
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
    })
}

impl EventStore for SqliteStore {
    fn append_event(&self, event: NewEvent) -> Result<Event> {
        let timestamp = now_ms();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (timestamp, source, event_type, agent_id, task_id, message, level, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                timestamp,
                event.source,
                event.event_type,
                event.agent_id,
                event.task_id,
                event.message,
                event.level.as_str(),
                serde_json::to_string(&event.data)?,
            ],
        )?;
        Ok(Event {
            seq: conn.last_insert_rowid(),
            timestamp,
            source: event.source,
            event_type: event.event_type,
            agent_id: event.agent_id,
            task_id: event.task_id,
            message: event.message,
            level: event.level,
            data: event.data,
        })
    }

    fn recent_events(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Event>> {
        let conn = self.conn();
        let limit = limit as i64;
        let mut events: Vec<Event> = match agent {
            Some(agent) => {
                let mut stmt = conn.prepare(
                    "SELECT seq, timestamp, source, event_type, agent_id, task_id, message, level, data
                     FROM events WHERE agent_id = ?1 ORDER BY seq DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![agent, limit], event_from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT seq, timestamp, source, event_type, agent_id, task_id, message, level, data
                     FROM events ORDER BY seq DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], event_from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            }
        };
        events.reverse();
        Ok(events)
    }
}

const MESSAGE_COLUMNS: &str =
    "m.id, m.sender, m.recipient, m.kind, m.body, m.timestamp, m.correlation_id, m.reply_to, m.metadata";

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let kind: String = row.get(3)?;
    let metadata: String = row.get(8)?;
    Ok(Message {
        id: row.get(0)?,
        from: row.get(1)?,
        to: row.get(2)?,
        kind: kind.parse().unwrap_or(MessageType::Message),
        body: row.get(4)?,
        timestamp: row.get(5)?,
        correlation_id: row.get(6)?,
        reply_to: row.get(7)?,
        metadata: serde_json::from_str(&metadata).unwrap_or(Value::Null),
    })
}

/// Insert `message` and its delivery rows within `tx`; `false` (nothing
/// inserted) when its id is already taken.
fn insert_message_in(
    tx: &rusqlite::Transaction<'_>,
    message: &Message,
    recipients: &[String],
) -> Result<bool> {
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO messages
             (id, sender, recipient, kind, body, timestamp, correlation_id, reply_to, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            message.id,
            message.from,
            message.to,
            message.kind.as_str(),
            message.body,
            message.timestamp,
            message.correlation_id,
            message.reply_to,
            serde_json::to_string(&message.metadata)?,
        ],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    for recipient in recipients {
        tx.execute(
            "INSERT OR IGNORE INTO deliveries (message_id, recipient) VALUES (?1, ?2)",
            params![message.id, recipient],
        )?;
    }
    Ok(true)
}

impl MessageStore for SqliteStore {
    fn insert_message(&self, message: &Message, recipients: &[String]) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let inserted = insert_message_in(&tx, message, recipients)?;
        if inserted {
            tx.commit()?;
        }
        Ok(inserted)
    }

    fn insert_message_with_task(
        &self,
        message: &Message,
        recipients: &[String],
        task: &Task,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !insert_message_in(&tx, message, recipients)? {
            return Ok(false);
        }
        upsert_doc(&tx, "tasks", &task.id, &serde_json::to_string(task)?)?;
        tx.commit()?;
        Ok(true)
    }

    fn get_message(&self, id: &str) -> Result<Option<Message>> {
        let conn = self.conn();
        let sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.id = ?1");
        Ok(conn
            .query_row(&sql, params![id], message_from_row)
            .optional()?)
    }

    fn recipients(&self, message_id: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT recipient FROM deliveries WHERE message_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![message_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn inbox(&self, agent: &str, unread_only: bool, limit: usize) -> Result<Vec<InboxEntry>> {
        let conn = self.conn();
        let filter = if unread_only {
            "AND d.acked_at IS NULL"
        } else {
            ""
        };
        // Newest `limit` rows, returned oldest first.
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS}, d.delivered_at, d.acked_at
             FROM deliveries d JOIN messages m ON m.id = d.message_id
             WHERE d.recipient = ?1 {filter}
             ORDER BY m.seq DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![agent, limit as i64], |row| {
            Ok(InboxEntry {
                message: message_from_row(row)?,
                delivered_at: row.get(9)?,
                acked_at: row.get(10)?,
            })
        })?;
        let mut out: Vec<InboxEntry> = rows.collect::<rusqlite::Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    fn undelivered(&self, agent: &str) -> Result<Vec<Message>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             FROM deliveries d JOIN messages m ON m.id = d.message_id
             WHERE d.recipient = ?1 AND d.delivered_at IS NULL AND d.acked_at IS NULL
             ORDER BY m.seq"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![agent], message_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn mark_delivered(&self, agent: &str, ids: &[String], at: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE deliveries SET delivered_at = ?1
                 WHERE message_id = ?2 AND recipient = ?3 AND delivered_at IS NULL",
                params![at, id, agent],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn acknowledge(&self, agent: &str, ids: Option<&[String]>, at: i64) -> Result<usize> {
        let mut conn = self.conn();
        match ids {
            None => Ok(conn.execute(
                "UPDATE deliveries SET acked_at = ?1 WHERE recipient = ?2 AND acked_at IS NULL",
                params![at, agent],
            )?),
            Some(ids) => {
                let tx = conn.transaction()?;
                let mut n = 0;
                for id in ids {
                    n += tx.execute(
                        "UPDATE deliveries SET acked_at = ?1
                         WHERE message_id = ?2 AND recipient = ?3 AND acked_at IS NULL",
                        params![at, id, agent],
                    )?;
                }
                tx.commit()?;
                Ok(n)
            }
        }
    }

    fn history(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Message>> {
        let conn = self.conn();
        let limit = limit as i64;
        let mut out: Vec<Message> = match agent {
            Some(agent) => {
                let sql = format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     WHERE m.sender = ?1 OR EXISTS (
                         SELECT 1 FROM deliveries d
                         WHERE d.message_id = m.id AND d.recipient = ?1)
                     ORDER BY m.seq DESC LIMIT ?2"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params![agent, limit], message_from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            }
            None => {
                let sql = format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m ORDER BY m.seq DESC LIMIT ?1"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params![limit], message_from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            }
        };
        out.reverse();
        Ok(out)
    }

    fn counts(&self, agent: &str) -> Result<MessageCounts> {
        let conn = self.conn();
        let (inbox_total, unread): (u32, u32) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(acked_at IS NULL), 0)
             FROM deliveries WHERE recipient = ?1",
            params![agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let outbox_total: u32 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE sender = ?1",
            params![agent],
            |row| row.get(0),
        )?;
        Ok(MessageCounts {
            unread,
            inbox_total,
            outbox_total,
        })
    }
}

impl MetaStore for SqliteStore {
    fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn delete_meta(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        Ok(())
    }
}

impl SessionStore for SqliteStore {
    fn clear_session(&self) -> Result<()> {
        self.conn().execute_batch(
            "BEGIN;
             DELETE FROM agents;
             DELETE FROM tokens;
             DELETE FROM tasks;
             DELETE FROM messages;
             DELETE FROM deliveries;
             DELETE FROM meta;
             COMMIT;",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::{AgentStatus, RuntimeKind, TaskStatus};

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: id.into(),
            role: "role".into(),
            task: None,
            status: AgentStatus::Idle,
            activity: String::new(),
            runtime: RuntimeKind::Shell,
            model: None,
            workdir: "/tmp".into(),
            branch: None,
            backend: "pty".into(),
            backend_ref: None,
            pid: None,
            session_id: None,
            orchestrator: false,
            parent: None,
            subagents: 0,
            temporary: false,
            delivery: String::new(),
            started_at: None,
            last_activity_at: 0,
            progress: None,
            paused: false,
            block_kind: None,
            limit: None,
            context: None,
            compact_pending: false,
            compactions: 0,
            running_model: None,
            model_name: None,
            effort: None,
            fallback: None,
            exit_code: None,
            restarts: 0,
            inbox_unread: 0,
            inbox_total: 0,
            outbox_total: 0,
            usage: None,
            error: None,
            toolbox: None,
        }
    }

    fn message(id: &str, from: &str, to: &str) -> Message {
        Message {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            kind: MessageType::Message,
            body: format!("body of {id}"),
            timestamp: now_ms(),
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::json!({"k": 1}),
        }
    }

    #[test]
    fn agents_keep_insertion_order_across_updates() {
        let store = SqliteStore::in_memory().unwrap();
        for id in ["b", "a", "c"] {
            store.save_agent(&agent(id)).unwrap();
        }
        let mut updated = agent("b");
        updated.status = AgentStatus::Working;
        store.save_agent(&updated).unwrap();
        let ids: Vec<String> = store
            .load_agents()
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["b", "a", "c"]);
        assert_eq!(store.load_agents().unwrap()[0].status, AgentStatus::Working);
        store.delete_agent("a").unwrap();
        assert_eq!(store.load_agents().unwrap().len(), 2);
    }

    #[test]
    fn duplicate_message_ids_are_ignored() {
        let store = SqliteStore::in_memory().unwrap();
        let m = message("m-1", "a", "b");
        assert!(store.insert_message(&m, &["b".into()]).unwrap());
        assert!(!store.insert_message(&m, &["b".into()]).unwrap());
        assert_eq!(store.history(None, 10).unwrap().len(), 1);
        assert_eq!(store.inbox("b", true, 10).unwrap().len(), 1);
        assert_eq!(store.get_message("m-1").unwrap().unwrap(), m);
    }

    #[test]
    fn delivery_and_ack_are_tracked_per_recipient() {
        let store = SqliteStore::in_memory().unwrap();
        let m = message("m-1", "a", "*");
        store.insert_message(&m, &["b".into(), "c".into()]).unwrap();
        assert_eq!(store.recipients("m-1").unwrap(), ["b", "c"]);
        assert_eq!(store.undelivered("b").unwrap().len(), 1);
        store.mark_delivered("b", &["m-1".into()], 5).unwrap();
        assert!(store.undelivered("b").unwrap().is_empty());
        assert_eq!(store.undelivered("c").unwrap().len(), 1);

        assert_eq!(store.counts("b").unwrap().unread, 1);
        assert_eq!(store.acknowledge("b", None, 9).unwrap(), 1);
        assert_eq!(store.acknowledge("b", None, 9).unwrap(), 0);
        let counts = store.counts("b").unwrap();
        assert_eq!((counts.unread, counts.inbox_total), (0, 1));
        assert_eq!(store.counts("a").unwrap().outbox_total, 1);
        assert!(store.inbox("b", true, 10).unwrap().is_empty());
        let all = store.inbox("b", false, 10).unwrap();
        assert_eq!(all[0].delivered_at, Some(5));
        assert_eq!(all[0].acked_at, Some(9));
        // c still has it unread.
        assert_eq!(store.inbox("c", true, 10).unwrap().len(), 1);
    }

    #[test]
    fn history_is_bounded_and_ordered() {
        let store = SqliteStore::in_memory().unwrap();
        for i in 0..5 {
            let (from, to) = if i % 2 == 0 { ("a", "b") } else { ("c", "d") };
            store
                .insert_message(&message(&format!("m-{i}"), from, to), &[to.into()])
                .unwrap();
        }
        let last3: Vec<String> = store
            .history(None, 3)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(last3, ["m-2", "m-3", "m-4"]);
        let for_b: Vec<String> = store
            .history(Some("b"), 10)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(for_b, ["m-0", "m-2", "m-4"]);
    }

    #[test]
    fn events_and_tasks_and_meta_persist_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crew.db");
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .append_event(NewEvent::new("test", "a.b", "first").agent("x"))
                .unwrap();
            let e = store
                .append_event(NewEvent::new("test", "a.c", "second").error())
                .unwrap();
            assert_eq!(e.seq, 2);
            store
                .save_task(&Task {
                    epic: None,
                    id: "t1".into(),
                    title: "T".into(),
                    description: String::new(),
                    assigned_agent: None,
                    status: TaskStatus::Queued,
                    dependencies: vec!["t0".into()],
                    created_at: 1,
                    started_at: None,
                    completed_at: None,
                    artifacts: vec![],
                    reviewer: None,
                    result: None,
                    progress: None,
                    note: None,
                    timeout_secs: None,
                })
                .unwrap();
            store.set_meta("k", "v1").unwrap();
            store.set_meta("k", "v2").unwrap();
            store.save_token("x", "secret").unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        let events = store.recent_events(None, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].level, EventLevel::Error);
        assert_eq!(store.recent_events(Some("x"), 10).unwrap().len(), 1);
        assert_eq!(store.load_tasks().unwrap()[0].dependencies, ["t0"]);
        assert_eq!(store.get_meta("k").unwrap().as_deref(), Some("v2"));
        assert_eq!(store.load_tokens().unwrap()["x"], "secret");
        store.delete_meta("k").unwrap();
        assert!(store.get_meta("k").unwrap().is_none());
        // A new session clears team state but keeps the event log.
        store.clear_session().unwrap();
        assert!(store.load_tasks().unwrap().is_empty());
        assert!(store.load_tokens().unwrap().is_empty());
        assert_eq!(store.recent_events(None, 10).unwrap().len(), 2);
    }
}
