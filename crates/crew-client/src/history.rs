//! Bounded retained history and scoped, arrival-identity cursors.

use std::collections::{HashSet, VecDeque};

use crew_protocol::vcs::VcsEvent;
use crew_protocol::{Event, Message};

use crate::api::{
    ClientError, ClientScope, EventFilter, EventPage, HistoryAnchor, HistoryCursor,
    MessageEndpoints, MessageFilter, MessagePage,
};

#[derive(Clone, Debug)]
pub(crate) struct HistoryEntry<T> {
    pub value: T,
    pub ordinal: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct History<T> {
    entries: VecDeque<HistoryEntry<T>>,
    capacity: usize,
    next_ordinal: u64,
    generation_revision: u64,
    pub(crate) may_have_older_remote: bool,
}

pub(crate) type MessageHistory = History<Message>;
pub(crate) type EventHistory = History<Event>;

impl<T> History<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
            next_ordinal: 1,
            generation_revision: 0,
            may_have_older_remote: true,
        }
    }

    pub(crate) fn entries(&self) -> impl DoubleEndedIterator<Item = &HistoryEntry<T>> {
        self.entries.iter()
    }

    pub(crate) fn items(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.entries().map(|entry| &entry.value)
    }

    fn reserve_ordinals(&mut self, count: usize) -> Result<u64, ClientError> {
        let first = self.next_ordinal;
        self.next_ordinal =
            first
                .checked_add(count as u64)
                .ok_or_else(|| ClientError::InternalFailure {
                    code: "history_ordinal_overflow".into(),
                })?;
        Ok(first)
    }

    pub(crate) fn reset(&mut self, items: Vec<T>, revision: u64) -> Result<(), ClientError> {
        let skip = items.len().saturating_sub(self.capacity);
        let kept = items.len() - skip;
        let first = self.reserve_ordinals(kept)?;
        self.entries = items
            .into_iter()
            .skip(skip)
            .enumerate()
            .map(|(index, value)| HistoryEntry {
                value,
                ordinal: first + index as u64,
            })
            .collect();
        self.generation_revision = revision;
        // A bounded snapshot never establishes server exhaustion.
        self.may_have_older_remote = true;
        Ok(())
    }

    fn append_value(&mut self, value: T) -> Result<(), ClientError> {
        let ordinal = self.reserve_ordinals(1)?;
        self.entries.push_back(HistoryEntry { value, ordinal });
        if self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
        Ok(())
    }

    fn prepend_values(&mut self, values: Vec<T>) -> Result<bool, ClientError> {
        let count = values
            .len()
            .min(self.capacity.saturating_sub(self.entries.len()));
        if count == 0 {
            return Ok(false);
        }
        let first = self.reserve_ordinals(count)?;
        // Keep the newest part of the returned older page, preserving wire order.
        let skip = values.len() - count;
        let mut older: VecDeque<_> = values
            .into_iter()
            .skip(skip)
            .enumerate()
            .map(|(index, value)| HistoryEntry {
                value,
                ordinal: first + index as u64,
            })
            .collect();
        older.append(&mut self.entries);
        self.entries = older;
        Ok(true)
    }

    fn check_cursor(
        &self,
        scope: &ClientScope,
        revision: u64,
        cursor: &HistoryCursor,
    ) -> Result<(), ClientError> {
        if &cursor.scope != scope
            || cursor.revision < self.generation_revision
            || cursor.revision > revision
        {
            return Err(ClientError::CursorExpired);
        }
        Ok(())
    }

    pub(crate) fn refine_remote_older(&mut self, returned: &[T], limit: u32)
    where
        T: PartialEq,
    {
        // A short unfiltered read proves exhaustion only when every returned
        // record is retained. Local capacity may have dropped older records.
        self.may_have_older_remote = returned.len() >= limit as usize
            || returned
                .iter()
                .any(|item| !self.items().any(|old| old == item));
    }
}

fn check_limit(limit: u32, maximum: u32) -> Result<usize, ClientError> {
    if limit == 0 || limit > maximum {
        return Err(ClientError::InvalidInput {
            field: "limit".into(),
            reason: format!("must be between 1 and {maximum}"),
        });
    }
    Ok(limit as usize)
}

impl History<Message> {
    pub(crate) fn append(&mut self, message: Message) -> Result<bool, ClientError> {
        if self.items().rev().take(64).any(|m| m.id == message.id) {
            return Ok(false);
        }
        self.append_value(message)?;
        Ok(true)
    }

    pub(crate) fn prepend(&mut self, older: Vec<Message>) -> Result<bool, ClientError> {
        let first_at = self.entries.front().map(|entry| entry.value.timestamp);
        let mut seen: HashSet<String> = self.items().map(|m| m.id.clone()).collect();
        let keep = older
            .into_iter()
            .filter(|m| first_at.is_none_or(|at| m.timestamp <= at) && seen.insert(m.id.clone()))
            .collect();
        self.prepend_values(keep)
    }

    pub(crate) fn page(
        &self,
        scope: &ClientScope,
        revision: u64,
        before: Option<&HistoryCursor>,
        limit: u32,
        maximum: u32,
        filter: &MessageFilter,
    ) -> Result<MessagePage, ClientError> {
        let limit = check_limit(limit, maximum)?;
        let end = if let Some(cursor) = before {
            self.check_cursor(scope, revision, cursor)?;
            let HistoryAnchor::Message {
                id,
                timestamp_ms,
                ordinal,
            } = &cursor.anchor
            else {
                return Err(ClientError::CursorExpired);
            };
            self.entries
                .iter()
                .position(|entry| {
                    entry.ordinal == *ordinal
                        && entry.value.id == *id
                        && entry.value.timestamp == *timestamp_ms
                })
                .ok_or(ClientError::CursorExpired)?
        } else {
            self.entries.len()
        };
        let text = filter.text.to_lowercase();
        let matching: Vec<_> = self
            .entries
            .iter()
            .take(end)
            .filter(|entry| {
                let m = &entry.value;
                let endpoints = match &filter.endpoints {
                    MessageEndpoints::All => true,
                    MessageEndpoints::Agent { id } => {
                        m.from == *id || m.to == *id || m.is_broadcast()
                    }
                    MessageEndpoints::Pair { first, second } => {
                        (m.from == *first && m.to == *second)
                            || (m.from == *second && m.to == *first)
                    }
                };
                endpoints
                    && filter.kind.is_none_or(|kind| m.kind == kind)
                    && (text.is_empty() || m.body.to_lowercase().contains(&text))
            })
            .collect();
        let start = matching.len().saturating_sub(limit);
        let selected = &matching[start..];
        Ok(MessagePage {
            items: selected.iter().map(|entry| entry.value.clone()).collect(),
            before: selected.first().map(|entry| HistoryCursor {
                scope: scope.clone(),
                revision,
                anchor: HistoryAnchor::Message {
                    id: entry.value.id.clone(),
                    timestamp_ms: entry.value.timestamp,
                    ordinal: entry.ordinal,
                },
            }),
            has_older_local: start != 0,
            may_have_older_remote: self.may_have_older_remote,
        })
    }
}

impl History<Event> {
    pub(crate) fn append(&mut self, event: Event) -> Result<bool, ClientError> {
        if event.seq < 0 {
            return Err(ClientError::InvalidInput {
                field: "event.seq".into(),
                reason: "must be nonnegative".into(),
            });
        }
        if self
            .entries
            .back()
            .is_some_and(|last| event.seq != 0 && event.seq <= last.value.seq)
        {
            return Ok(false);
        }
        self.append_value(event)?;
        Ok(true)
    }

    pub(crate) fn prepend(&mut self, older: Vec<Event>) -> Result<bool, ClientError> {
        if older.iter().any(|event| event.seq < 0) {
            return Err(ClientError::InvalidInput {
                field: "event.seq".into(),
                reason: "must be nonnegative".into(),
            });
        }
        let first = self.entries.front().map(|entry| entry.value.seq);
        let mut seen: HashSet<i64> = self.items().filter(|e| e.seq != 0).map(|e| e.seq).collect();
        let keep = older
            .into_iter()
            .filter(|e| e.seq == 0 || (first.is_none_or(|seq| e.seq < seq) && seen.insert(e.seq)))
            .collect();
        self.prepend_values(keep)
    }

    pub(crate) fn page(
        &self,
        scope: &ClientScope,
        revision: u64,
        before: Option<&HistoryCursor>,
        limit: u32,
        maximum: u32,
        filter: &EventFilter,
    ) -> Result<EventPage, ClientError> {
        let limit = check_limit(limit, maximum)?;
        let end = if let Some(cursor) = before {
            self.check_cursor(scope, revision, cursor)?;
            let HistoryAnchor::Event { seq, ordinal } = cursor.anchor else {
                return Err(ClientError::CursorExpired);
            };
            self.entries
                .iter()
                .position(|entry| {
                    entry.ordinal == ordinal && u64::try_from(entry.value.seq).ok() == Some(seq)
                })
                .ok_or(ClientError::CursorExpired)?
        } else {
            self.entries.len()
        };
        let text = filter.text.to_lowercase();
        let matching: Vec<_> = self
            .entries
            .iter()
            .take(end)
            .filter(|entry| {
                let e = &entry.value;
                filter
                    .agent_id
                    .as_ref()
                    .is_none_or(|id| e.agent_id.as_ref() == Some(id))
                    && filter.level.is_none_or(|level| e.level == level)
                    && (!filter.vcs_only || VcsEvent::from_event(e).is_some())
                    && (text.is_empty()
                        || e.message.to_lowercase().contains(&text)
                        || e.event_type.to_lowercase().contains(&text)
                        || e.agent_id
                            .as_ref()
                            .is_some_and(|id| id.to_lowercase().contains(&text)))
            })
            .collect();
        let start = matching.len().saturating_sub(limit);
        let selected = &matching[start..];
        Ok(EventPage {
            items: selected.iter().map(|entry| entry.value.clone()).collect(),
            before: selected
                .first()
                .map(|entry| {
                    let seq =
                        u64::try_from(entry.value.seq).map_err(|_| ClientError::InvalidInput {
                            field: "event.seq".into(),
                            reason: "must be nonnegative".into(),
                        })?;
                    Ok(HistoryCursor {
                        scope: scope.clone(),
                        revision,
                        anchor: HistoryAnchor::Event {
                            seq,
                            ordinal: entry.ordinal,
                        },
                    })
                })
                .transpose()?,
            has_older_local: start != 0,
            may_have_older_remote: self.may_have_older_remote,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::{EventLevel, MessageType};

    fn scope() -> ClientScope {
        ClientScope {
            endpoint_id: "e".into(),
            workspace_id: "w".into(),
            generation: None,
            daemon_pid: Some(1),
            daemon_started_at_ms: None,
            attempt: 1,
        }
    }
    fn message(id: &str, at: i64) -> Message {
        Message {
            id: id.into(),
            from: "a".into(),
            to: "b".into(),
            kind: MessageType::Message,
            body: format!("{id} Hello"),
            timestamp: at,
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
        }
    }
    fn filter() -> MessageFilter {
        MessageFilter {
            endpoints: MessageEndpoints::All,
            kind: None,
            text: String::new(),
        }
    }
    fn event(seq: i64) -> Event {
        Event {
            seq,
            timestamp: 0,
            source: "daemon".into(),
            event_type: "agent.updated".into(),
            agent_id: Some("a".into()),
            task_id: None,
            message: format!("event {seq}"),
            level: EventLevel::Info,
            data: serde_json::Value::Null,
        }
    }
    fn event_filter() -> EventFilter {
        EventFilter {
            agent_id: None,
            level: None,
            text: String::new(),
            vcs_only: false,
        }
    }

    #[test]
    fn cursor_survives_unrelated_revisions_but_not_scope_reset_or_eviction() {
        let mut history = MessageHistory::new(3);
        history
            .reset(vec![message("1", 1), message("2", 2), message("3", 3)], 1)
            .unwrap();
        let page = history.page(&scope(), 2, None, 2, 200, &filter()).unwrap();
        assert_eq!(
            page.items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["2", "3"]
        );
        let cursor = page.before.unwrap();
        assert_eq!(
            history
                .page(&scope(), 5, Some(&cursor), 2, 200, &filter())
                .unwrap()
                .items[0]
                .id,
            "1"
        );
        let mut other = scope();
        other.attempt += 1;
        assert_eq!(
            history.page(&other, 5, Some(&cursor), 2, 200, &filter()),
            Err(ClientError::CursorExpired)
        );
        history.append(message("4", 4)).unwrap();
        history.append(message("5", 5)).unwrap();
        assert_eq!(
            history.page(&scope(), 5, Some(&cursor), 2, 200, &filter()),
            Err(ClientError::CursorExpired)
        );
        history
            .reset(vec![message("2", 2), message("3", 3)], 6)
            .unwrap();
        assert_eq!(
            history.page(&scope(), 6, Some(&cursor), 2, 200, &filter()),
            Err(ClientError::CursorExpired)
        );
    }

    #[test]
    fn dedup_is_bounded_to_the_newest_sixty_four_ids() {
        let mut history = MessageHistory::new(100);
        for n in 0..65 {
            assert!(history.append(message(&n.to_string(), n)).unwrap());
        }
        assert!(!history.append(message("1", 1)).unwrap());
        assert!(history.append(message("0", 0)).unwrap());
        assert_eq!(history.items().count(), 66);
        assert_eq!(history.items().filter(|m| m.id == "0").count(), 2);
    }

    #[test]
    fn zero_sequence_events_have_distinct_cursor_identity_and_nonzero_dedup() {
        let mut history = EventHistory::new(10);
        for seq in [1, 2, 0, 0] {
            assert!(history.append(event(seq)).unwrap());
        }
        let page = history
            .page(&scope(), 1, None, 1, 200, &event_filter())
            .unwrap();
        let older = history
            .page(&scope(), 2, page.before.as_ref(), 1, 200, &event_filter())
            .unwrap();
        assert_eq!(older.items[0].seq, 0);
        assert_ne!(page.before.unwrap().anchor, older.before.unwrap().anchor);
        // As in the existing clients, the last accepted event is the watermark.
        assert!(history.append(event(1)).unwrap());
        assert!(!history.append(event(1)).unwrap());
    }

    #[test]
    fn negative_sequences_never_wrap_into_unsigned_cursor_identity() {
        let mut history = EventHistory::new(2);
        assert!(history.append(event(-1)).is_err());
        assert!(history.prepend(vec![event(-1)]).is_err());
        assert_eq!(history.items().count(), 0);
        // Generic reset is internal; pagination still protects its public
        // unsigned cursor boundary if a malformed value bypasses reduction.
        history.reset(vec![event(-1)], 1).unwrap();
        assert!(history
            .page(&scope(), 1, None, 1, 200, &event_filter())
            .is_err());
        let cursor = HistoryCursor {
            scope: scope(),
            revision: 1,
            anchor: HistoryAnchor::Event {
                seq: u64::MAX,
                ordinal: 1,
            },
        };
        assert_eq!(
            history.page(&scope(), 1, Some(&cursor), 1, 200, &event_filter()),
            Err(ClientError::CursorExpired)
        );
    }

    #[test]
    fn filters_are_literal_case_insensitive_and_pair_is_unordered() {
        let mut history = MessageHistory::new(10);
        history.append(message("one", 1)).unwrap();
        let query = MessageFilter {
            endpoints: MessageEndpoints::Pair {
                first: "b".into(),
                second: "a".into(),
            },
            kind: Some(MessageType::Message),
            text: "HELLO".into(),
        };
        assert_eq!(
            history
                .page(&scope(), 1, None, 10, 200, &query)
                .unwrap()
                .items
                .len(),
            1
        );
        let mut regex = query;
        regex.text = "H.*o".into();
        assert!(history
            .page(&scope(), 1, None, 10, 200, &regex)
            .unwrap()
            .items
            .is_empty());
    }

    #[test]
    fn backfill_preserves_bounds_and_limits_fail_before_copying() {
        let mut history = MessageHistory::new(3);
        history.reset(vec![message("3", 3)], 1).unwrap();
        history
            .prepend(vec![message("1", 1), message("2", 2), message("3", 3)])
            .unwrap();
        assert_eq!(
            history.items().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["1", "2", "3"]
        );
        assert!(!history.prepend(vec![message("0", 0)]).unwrap());
        assert!(history.page(&scope(), 1, None, 0, 200, &filter()).is_err());
        assert!(history
            .page(&scope(), 1, None, 201, 200, &filter())
            .is_err());
    }

    #[test]
    fn remote_exhaustion_requires_a_short_read_with_every_record_retained() {
        let mut history = MessageHistory::new(2);
        let returned = vec![message("1", 1), message("2", 2)];
        history.prepend(returned.clone()).unwrap();
        history.refine_remote_older(&returned, 2);
        assert!(
            history.may_have_older_remote,
            "a full read cannot prove exhaustion"
        );
        history.refine_remote_older(&returned, 3);
        assert!(!history.may_have_older_remote);
        let lost = vec![message("0", 0), message("1", 1), message("2", 2)];
        history.prepend(lost.clone()).unwrap();
        history.refine_remote_older(&lost, 4);
        assert!(
            history.may_have_older_remote,
            "an older returned record did not fit locally"
        );
        history.reset(vec![], 2).unwrap();
        assert!(
            history.may_have_older_remote,
            "snapshot replacement restores uncertainty"
        );
    }
}
