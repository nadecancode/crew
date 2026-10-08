//! Queue admission is not consumption. Keep receipts out of the public
//! protocol and leave their bus rows pending until the native turn is known.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub(super) const RECEIPT_MS: i64 = 120_000;
pub(super) const MAX_RECEIPTS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Offered,
    Accepted,
    Consumed,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub batch_id: String,
    pub ids: Vec<String>,
    pub session_id: String,
    pub launch: Option<String>,
    pub queued_at: i64,
    pub accepted_at: Option<i64>,
    pub observed_at: Option<i64>,
    pub observed_turn: Option<String>,
    pub consumed_at: Option<i64>,
    pub deadline: i64,
    pub phase: Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Book {
    pub(super) version: u32,
    pub(super) receipts: BTreeMap<String, Receipt>,
}

impl Default for Book {
    fn default() -> Self {
        Self {
            version: 1,
            receipts: BTreeMap::new(),
        }
    }
}

impl Book {
    pub(super) fn contains(&self, id: &str) -> bool {
        self.receipts
            .values()
            .any(|r| r.ids.iter().any(|x| x == id))
    }

    pub(super) fn offer(
        &mut self,
        ids: Vec<String>,
        session_id: String,
        launch: Option<String>,
        now: i64,
    ) -> Option<String> {
        if ids.is_empty()
            || self.receipts.len() >= MAX_RECEIPTS
            || ids.iter().any(|id| self.contains(id))
        {
            return None;
        }
        let key = crew_core::util::new_id("delivery");
        self.receipts.insert(
            key.clone(),
            Receipt {
                batch_id: key.clone(),
                ids,
                session_id,
                launch,
                queued_at: now,
                accepted_at: None,
                observed_at: None,
                observed_turn: None,
                consumed_at: None,
                deadline: now.saturating_add(RECEIPT_MS),
                phase: Phase::Offered,
            },
        );
        Some(key)
    }

    pub(super) fn matching_mut(
        &mut self,
        key: &str,
        ids: &[String],
        session: &str,
        launch: &Option<String>,
    ) -> Option<&mut Receipt> {
        self.receipts
            .get_mut(key)
            .filter(|r| r.ids == ids && r.session_id == session && &r.launch == launch)
    }

    pub(super) fn expire(&mut self, now: i64) -> Vec<Receipt> {
        let mut unknown = Vec::new();
        for r in self.receipts.values_mut() {
            let admission_timed_out =
                r.phase == Phase::Offered && now.saturating_sub(r.queued_at) >= 20_000;
            if (r.deadline <= now || admission_timed_out)
                && !matches!(r.phase, Phase::Unknown | Phase::Consumed)
            {
                r.phase = Phase::Unknown;
                unknown.push(r.clone());
            }
        }
        // Expiry never frees a native admission. Only retirement does that.
        unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_and_expired_receipts_survive_restart_without_authorizing_a_second_copy() {
        let ids = vec!["m-1".into(), "m-2".into()];
        let mut book = Book::default();
        let key = book
            .offer(
                ids.clone(),
                "native-session".into(),
                Some("launch".into()),
                10,
            )
            .unwrap();
        let r = book
            .matching_mut(&key, &ids, "native-session", &Some("launch".into()))
            .unwrap();
        r.phase = Phase::Accepted;
        r.accepted_at = Some(20);
        let mut restored: Book =
            serde_json::from_str(&serde_json::to_string(&book).unwrap()).unwrap();
        assert!(restored
            .offer(
                ids.clone(),
                "native-session".into(),
                Some("launch".into()),
                30
            )
            .is_none());
        assert_eq!(restored.expire(10 + RECEIPT_MS).len(), 1);
        assert!(restored
            .offer(
                ids.clone(),
                "native-session".into(),
                Some("launch".into()),
                10 + RECEIPT_MS
            )
            .is_none());
        assert!(restored
            .matching_mut(&key, &ids, "other-session", &Some("launch".into()))
            .is_none());
        restored.receipts.remove(&key);
        assert!(restored
            .offer(
                ids,
                "next-session".into(),
                Some("next-launch".into()),
                10 + RECEIPT_MS
            )
            .is_some());
    }
}
