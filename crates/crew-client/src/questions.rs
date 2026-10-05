//! Operator questions are authoritative state independent of retained history.

use std::collections::BTreeSet;

use crew_protocol::{Message, MessageType, OPERATOR_ID};

#[derive(Clone, Debug, Default)]
pub(crate) struct Questions {
    pub(crate) open: Vec<Message>,
    answering: BTreeSet<String>,
}

impl Questions {
    pub(crate) fn replace(&mut self, questions: Vec<Message>) {
        self.open = questions;
        self.answering
            .retain(|id| self.open.iter().any(|q| &q.id == id));
    }

    pub(crate) fn on_message(&mut self, message: &Message) -> bool {
        if message.is_operator_question() {
            if self.open.iter().any(|q| q.id == message.id) {
                return false;
            }
            self.open.push(message.clone());
            return true;
        }
        if message.from == OPERATOR_ID && message.kind == MessageType::Answer {
            if let Some(id) = &message.reply_to {
                let before = self.open.len();
                self.open.retain(|q| &q.id != id);
                let answering = self.answering.remove(id);
                return before != self.open.len() || answering;
            }
        }
        false
    }

    pub(crate) fn visible(&self) -> impl Iterator<Item = &Message> {
        self.open.iter().filter(|q| !self.answering.contains(&q.id))
    }

    pub(crate) fn sync_answering<'a>(&mut self, ids: impl Iterator<Item = &'a str>) -> bool {
        let next: BTreeSet<String> = ids
            .filter(|id| self.open.iter().any(|question| question.id == *id))
            .map(str::to_string)
            .collect();
        if next == self.answering {
            return false;
        }
        self.answering = next;
        true
    }

    pub(crate) fn clear(&mut self) {
        self.open.clear();
        self.answering.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> Message {
        Message {
            id: "q".into(),
            from: "orchestrator".into(),
            to: OPERATOR_ID.into(),
            kind: MessageType::Question,
            body: "Choose".into(),
            timestamp: 0,
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn pending_answers_hide_without_removing_and_failure_restores() {
        let mut questions = Questions::default();
        assert!(questions.on_message(&question()));
        assert!(!questions.on_message(&question()));
        assert!(questions.sync_answering(["q"].into_iter()));
        assert_eq!(questions.open.len(), 1);
        assert_eq!(questions.visible().count(), 0);
        assert!(questions.sync_answering([].into_iter()));
        assert_eq!(questions.visible().count(), 1);
    }

    #[test]
    fn authoritative_answer_and_snapshot_retire_pending_questions() {
        let mut questions = Questions::default();
        questions.replace(vec![question()]);
        questions.sync_answering(["q"].into_iter());
        let answer = Message {
            id: "answer".into(),
            from: OPERATOR_ID.into(),
            to: "orchestrator".into(),
            kind: MessageType::Answer,
            reply_to: Some("q".into()),
            ..question()
        };
        assert!(questions.on_message(&answer));
        assert!(questions.open.is_empty());
        questions.replace(vec![question()]);
        questions.sync_answering(["q"].into_iter());
        questions.replace(vec![]);
        questions.replace(vec![question()]);
        assert_eq!(questions.visible().count(), 1);
    }
}
