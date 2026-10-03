//! The agent message bus.
//!
//! `MessageBus` is the seam for swapping transports: the daemon depends only
//! on the trait. `LocalBus` is the implementation used today — durable
//! storage through a `MessageStore`, live fan-out through an in-process
//! broadcast channel (no polling anywhere).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crew_core::util::{new_id, now_ms};
use crew_core::{InboxEntry, Message, MessageType, BROADCAST, MAX_MESSAGE_BYTES};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::persistence::{MessageCounts, MessageStore};

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("message body is empty")]
    Empty,
    #[error("message body is {size} bytes; the limit is {limit}")]
    TooLarge { size: usize, limit: usize },
    #[error("invalid message id '{0}' (1-64 chars of [A-Za-z0-9_.:-])")]
    InvalidId(String),
    #[error("message id '{0}' was already used by another sender")]
    IdConflict(String),
    #[error("a message needs at least one recipient")]
    NoRecipients,
    #[error("storage error: {0}")]
    Storage(#[from] anyhow::Error),
}

/// A message as submitted, before the bus assigns id and timestamp.
#[derive(Clone, Debug)]
pub struct NewMessage {
    /// Optional client-chosen id; resending the same id is a no-op.
    pub id: Option<String>,
    pub from: String,
    /// A single recipient id, or `*` for a broadcast.
    pub to: String,
    pub kind: MessageType,
    pub body: String,
    pub correlation_id: Option<String>,
    pub reply_to: Option<String>,
    pub metadata: Value,
}

impl NewMessage {
    pub fn new(from: &str, to: &str, kind: MessageType, body: impl Into<String>) -> Self {
        Self {
            id: None,
            from: from.to_string(),
            to: to.to_string(),
            kind,
            body: body.into(),
            correlation_id: None,
            reply_to: None,
            metadata: Value::Null,
        }
    }

    pub fn metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn correlation(mut self, id: Option<String>) -> Self {
        self.correlation_id = id;
        self
    }
}

#[derive(Clone, Debug)]
pub struct SendOutcome {
    pub message: Message,
    /// The id had been seen before; nothing was stored or delivered again.
    pub duplicate: bool,
    pub recipients: Vec<String>,
}

/// Published to subscribers for every newly accepted message.
#[derive(Clone, Debug)]
pub struct BusEvent {
    pub message: Message,
    pub recipients: Vec<String>,
}

pub trait MessageBus: Send + Sync {
    /// Accept a message for the given recipients. For a direct message
    /// `recipients` is `[to]`; for a broadcast it is everyone but the sender.
    fn send(&self, message: NewMessage, recipients: Vec<String>) -> Result<SendOutcome, BusError>;
    /// Read an agent's inbox, oldest first.
    fn receive(
        &self,
        agent: &str,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<InboxEntry>, BusError>;
    /// Messages that still have to be injected into the agent's session.
    fn undelivered(&self, agent: &str) -> Result<Vec<Message>, BusError>;
    fn mark_delivered(&self, agent: &str, ids: &[String]) -> Result<(), BusError>;
    /// Acknowledge specific messages, or everything unread when `ids` is `None`.
    fn acknowledge(&self, agent: &str, ids: Option<&[String]>) -> Result<usize, BusError>;
    /// Most recent messages (optionally only those involving `agent`), oldest first.
    fn history(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Message>, BusError>;
    /// One message by id, with whom it went to.
    fn get(&self, id: &str) -> Result<Option<(Message, Vec<String>)>, BusError>;
    fn counts(&self, agent: &str) -> Result<MessageCounts, BusError>;
    /// Live stream of accepted messages.
    fn subscribe(&self) -> broadcast::Receiver<BusEvent>;
    fn set_max_bytes(&self, limit: usize);
    /// The largest message body `send` accepts.
    fn max_bytes(&self) -> usize;
}

pub struct LocalBus {
    store: Arc<dyn MessageStore>,
    tx: broadcast::Sender<BusEvent>,
    max_bytes: AtomicUsize,
}

impl LocalBus {
    pub fn new(store: Arc<dyn MessageStore>) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            store,
            tx,
            max_bytes: AtomicUsize::new(MAX_MESSAGE_BYTES),
        }
    }
}

fn valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

impl MessageBus for LocalBus {
    fn send(&self, new: NewMessage, recipients: Vec<String>) -> Result<SendOutcome, BusError> {
        if new.body.trim().is_empty() {
            return Err(BusError::Empty);
        }
        let limit = self.max_bytes.load(Ordering::Relaxed);
        if new.body.len() > limit {
            return Err(BusError::TooLarge {
                size: new.body.len(),
                limit,
            });
        }
        if recipients.is_empty() && new.to != BROADCAST {
            return Err(BusError::NoRecipients);
        }
        let id = match new.id {
            Some(id) if valid_message_id(&id) => id,
            Some(id) => return Err(BusError::InvalidId(id)),
            None => new_id("m"),
        };
        let message = Message {
            id,
            from: new.from,
            to: new.to,
            kind: new.kind,
            body: new.body,
            timestamp: now_ms(),
            correlation_id: new.correlation_id,
            reply_to: new.reply_to,
            metadata: new.metadata,
        };
        if !self.store.insert_message(&message, &recipients)? {
            // Same id again: hand back the original and deliver nothing.
            let original = self
                .store
                .get_message(&message.id)?
                .ok_or_else(|| anyhow::anyhow!("duplicate message vanished"))?;
            if original.from != message.from {
                return Err(BusError::IdConflict(message.id));
            }
            let recipients = self.store.recipients(&original.id)?;
            return Ok(SendOutcome {
                message: original,
                duplicate: true,
                recipients,
            });
        }
        // No subscribers is fine; history is the source of truth.
        let _ = self.tx.send(BusEvent {
            message: message.clone(),
            recipients: recipients.clone(),
        });
        Ok(SendOutcome {
            message,
            duplicate: false,
            recipients,
        })
    }

    fn receive(
        &self,
        agent: &str,
        unread_only: bool,
        limit: usize,
    ) -> Result<Vec<InboxEntry>, BusError> {
        Ok(self.store.inbox(agent, unread_only, limit)?)
    }

    fn undelivered(&self, agent: &str) -> Result<Vec<Message>, BusError> {
        Ok(self.store.undelivered(agent)?)
    }

    fn mark_delivered(&self, agent: &str, ids: &[String]) -> Result<(), BusError> {
        Ok(self.store.mark_delivered(agent, ids, now_ms())?)
    }

    fn acknowledge(&self, agent: &str, ids: Option<&[String]>) -> Result<usize, BusError> {
        Ok(self.store.acknowledge(agent, ids, now_ms())?)
    }

    fn history(&self, agent: Option<&str>, limit: usize) -> Result<Vec<Message>, BusError> {
        Ok(self.store.history(agent, limit)?)
    }

    fn get(&self, id: &str) -> Result<Option<(Message, Vec<String>)>, BusError> {
        let Some(message) = self.store.get_message(id)? else {
            return Ok(None);
        };
        let recipients = self.store.recipients(id)?;
        Ok(Some((message, recipients)))
    }

    fn counts(&self, agent: &str) -> Result<MessageCounts, BusError> {
        Ok(self.store.counts(agent)?)
    }

    fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.tx.subscribe()
    }

    fn set_max_bytes(&self, limit: usize) {
        self.max_bytes
            .store(limit.clamp(1, MAX_MESSAGE_BYTES), Ordering::Relaxed);
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SqliteStore;

    fn bus() -> LocalBus {
        LocalBus::new(Arc::new(SqliteStore::in_memory().unwrap()))
    }

    #[test]
    fn direct_message_reaches_only_its_recipient() {
        let bus = bus();
        let mut rx = bus.subscribe();
        let out = bus
            .send(
                NewMessage::new("claude-3", "claude-5", MessageType::Question, "schema?"),
                vec!["claude-5".into()],
            )
            .unwrap();
        assert!(!out.duplicate);
        assert!(out.message.is_p2p());

        let event = rx.try_recv().unwrap();
        assert_eq!(event.message.id, out.message.id);
        assert_eq!(event.recipients, ["claude-5"]);

        let inbox = bus.receive("claude-5", true, 10).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].message.body, "schema?");
        assert!(bus.receive("claude-4", true, 10).unwrap().is_empty());
        assert!(bus.receive("orchestrator", true, 10).unwrap().is_empty());
    }

    #[test]
    fn broadcast_fans_out_to_every_recipient() {
        let bus = bus();
        let recipients: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let out = bus
            .send(
                NewMessage::new("orchestrator", BROADCAST, MessageType::Status, "freeze"),
                recipients.clone(),
            )
            .unwrap();
        assert!(out.message.is_broadcast());
        for r in &recipients {
            assert_eq!(bus.receive(r, true, 10).unwrap().len(), 1);
        }
        // One message in history, not three.
        assert_eq!(bus.history(None, 10).unwrap().len(), 1);
    }

    #[test]
    fn resending_the_same_id_is_a_noop() {
        let bus = bus();
        let mut rx = bus.subscribe();
        let mut m = NewMessage::new("a", "b", MessageType::Message, "once");
        m.id = Some("retry-1".into());
        let first = bus.send(m.clone(), vec!["b".into()]).unwrap();
        let second = bus.send(m, vec!["b".into()]).unwrap();
        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(second.message, first.message);
        assert_eq!(bus.receive("b", true, 10).unwrap().len(), 1);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "duplicate must not be re-published");

        // Another sender cannot reuse (or probe) the id.
        let mut spoof = NewMessage::new("c", "b", MessageType::Message, "mine");
        spoof.id = Some("retry-1".into());
        assert!(matches!(
            bus.send(spoof, vec!["b".into()]),
            Err(BusError::IdConflict(_))
        ));
    }

    #[test]
    fn rejects_empty_oversized_and_malformed() {
        let bus = bus();
        bus.set_max_bytes(8);
        assert!(matches!(
            bus.send(
                NewMessage::new("a", "b", MessageType::Message, "  "),
                vec!["b".into()]
            ),
            Err(BusError::Empty)
        ));
        assert!(matches!(
            bus.send(
                NewMessage::new("a", "b", MessageType::Message, "123456789"),
                vec!["b".into()]
            ),
            Err(BusError::TooLarge { size: 9, limit: 8 })
        ));
        let mut bad = NewMessage::new("a", "b", MessageType::Message, "ok");
        bad.id = Some("has space".into());
        assert!(matches!(
            bus.send(bad, vec!["b".into()]),
            Err(BusError::InvalidId(_))
        ));
        assert!(matches!(
            bus.send(
                NewMessage::new("a", "b", MessageType::Message, "ok"),
                vec![]
            ),
            Err(BusError::NoRecipients)
        ));
    }

    #[test]
    fn acknowledge_clears_unread() {
        let bus = bus();
        let a = bus
            .send(
                NewMessage::new("a", "b", MessageType::Message, "1"),
                vec!["b".into()],
            )
            .unwrap();
        bus.send(
            NewMessage::new("a", "b", MessageType::Message, "2"),
            vec!["b".into()],
        )
        .unwrap();
        assert_eq!(bus.undelivered("b").unwrap().len(), 2);
        bus.mark_delivered("b", std::slice::from_ref(&a.message.id))
            .unwrap();
        assert_eq!(bus.undelivered("b").unwrap().len(), 1);
        assert_eq!(bus.acknowledge("b", Some(&[a.message.id])).unwrap(), 1);
        assert_eq!(bus.counts("b").unwrap().unread, 1);
        assert_eq!(bus.acknowledge("b", None).unwrap(), 1);
        assert_eq!(bus.counts("b").unwrap().unread, 0);
        // Acknowledged messages are no longer pending delivery.
        assert!(bus.undelivered("b").unwrap().is_empty());
    }
}
