//! A bounded FIFO with one reserved end notification per registered channel.
//! Overflow cannot lose the event that makes the affected channel unusable.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::{ChannelId, HostEvent, TransportEnd};

pub const QUEUE_FRAMES: usize = 256;
pub const QUEUE_BYTES: usize = 4 * 1024 * 1024;

struct Queued {
    sequence: u64,
    bytes: usize,
    event: HostEvent,
}

struct State {
    next_sequence: u64,
    queue: VecDeque<Queued>,
    bytes: usize,
    channels: BTreeMap<ChannelId, Option<Queued>>,
    closed: bool,
}

struct Inner {
    state: Mutex<State>,
    notify: Notify,
    frame_limit: usize,
    byte_limit: usize,
    channel_limit: usize,
}

#[derive(Clone)]
pub struct EventQueue(Arc<Inner>);

#[derive(Clone)]
pub struct ChannelSink {
    queue: EventQueue,
    channel_id: ChannelId,
    ended: Arc<AtomicBool>,
}

impl EventQueue {
    pub fn new(max_channels: usize) -> Result<Self, TransportEnd> {
        Self::with_limits(max_channels, QUEUE_FRAMES, QUEUE_BYTES)
    }

    pub fn with_limits(
        max_channels: usize,
        frames: usize,
        bytes: usize,
    ) -> Result<Self, TransportEnd> {
        if !(1..=64).contains(&max_channels)
            || !(1..=QUEUE_FRAMES).contains(&frames)
            || !(1..=QUEUE_BYTES).contains(&bytes)
        {
            return Err(TransportEnd::Overflow);
        }
        Ok(Self(Arc::new(Inner {
            state: Mutex::new(State {
                next_sequence: 1,
                queue: VecDeque::new(),
                bytes: 0,
                channels: BTreeMap::new(),
                closed: false,
            }),
            notify: Notify::new(),
            frame_limit: frames,
            byte_limit: bytes,
            channel_limit: max_channels,
        })))
    }

    pub fn register(&self, channel_id: ChannelId) -> Result<ChannelSink, TransportEnd> {
        let mut state = self.0.state.lock().map_err(|_| TransportEnd::Stopped)?;
        if state.closed
            || state.channels.len() >= self.0.channel_limit
            || state.channels.contains_key(&channel_id)
        {
            return Err(TransportEnd::Overflow);
        }
        state.channels.insert(channel_id, None);
        Ok(ChannelSink {
            queue: self.clone(),
            channel_id,
            ended: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn recv(&self) -> Option<HostEvent> {
        loop {
            let notified = self.0.notify.notified();
            // Enable before checking the queue: a producer can run on another
            // worker between checking emptiness and awaiting the notification.
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(event) = self.try_recv() {
                return Some(event);
            }
            if self.0.state.lock().map_or(true, |state| state.closed) {
                return None;
            }
            notified.await;
        }
    }

    pub fn try_recv(&self) -> Option<HostEvent> {
        let mut state = self.0.state.lock().ok()?;
        let first_end = state
            .channels
            .iter()
            .filter_map(|(id, event)| event.as_ref().map(|event| (*id, event.sequence)))
            .min_by_key(|(_, sequence)| *sequence);
        let ordinary_first = match (state.queue.front(), first_end) {
            (Some(frame), Some((_, end_sequence))) => frame.sequence < end_sequence,
            (Some(_), None) => true,
            _ => false,
        };
        if ordinary_first {
            let frame = state.queue.pop_front()?;
            state.bytes -= frame.bytes;
            return Some(frame.event);
        }
        first_end.and_then(|(id, _)| {
            state
                .channels
                .remove(&id)
                .flatten()
                .map(|event| event.event)
        })
    }

    pub fn close(&self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.closed = true;
        }
        self.0.notify.notify_waiters();
    }
}

impl ChannelSink {
    pub fn try_send(&self, event: HostEvent) -> Result<(), TransportEnd> {
        if self.ended.load(Ordering::Acquire) || event.channel_id() != Some(self.channel_id) {
            return Err(TransportEnd::Closed);
        }
        if let HostEvent::Ended { reason, .. } = event {
            self.end(reason);
            return Ok(());
        }
        let bytes = event.queued_bytes();
        let mut state = self
            .queue
            .0
            .state
            .lock()
            .map_err(|_| TransportEnd::Stopped)?;
        if state.closed
            || self.ended.load(Ordering::Acquire)
            || !state.channels.contains_key(&self.channel_id)
        {
            return Err(TransportEnd::Closed);
        }
        if state.queue.len() >= self.queue.0.frame_limit
            || bytes > self.queue.0.byte_limit.saturating_sub(state.bytes)
        {
            drop(state);
            self.end(TransportEnd::Overflow);
            return Err(TransportEnd::Overflow);
        }
        let sequence = state.next_sequence;
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .ok_or(TransportEnd::Stopped)?;
        state.bytes += bytes;
        state.queue.push_back(Queued {
            sequence,
            bytes,
            event,
        });
        drop(state);
        self.queue.0.notify.notify_one();
        Ok(())
    }

    pub fn end(&self, reason: TransportEnd) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut state) = self.queue.0.state.lock() {
            let sequence = state.next_sequence;
            // Overflow is a terminal queue condition; never reuse sequence IDs.
            if let Some(next) = sequence.checked_add(1) {
                state.next_sequence = next;
                if let Some(slot) = state.channels.get_mut(&self.channel_id) {
                    *slot = Some(Queued {
                        sequence,
                        bytes: 0,
                        event: HostEvent::Ended {
                            channel_id: self.channel_id,
                            reason,
                        },
                    });
                }
            } else {
                state.closed = true;
            }
        }
        self.queue.0.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SendDisposition;

    #[tokio::test]
    async fn overflow_keeps_prior_write_progress_and_one_reserved_end_in_order() {
        let queue = EventQueue::with_limits(2, 1, 8).unwrap();
        let a = queue.register(10).unwrap();
        let b = queue.register(20).unwrap();
        a.try_send(HostEvent::SendProgress {
            channel_id: 10,
            request_id: 1,
            disposition: SendDisposition::WriteStarted,
        })
        .unwrap();
        assert_eq!(
            a.try_send(HostEvent::Frame {
                channel_id: 10,
                text: "{}".into()
            }),
            Err(TransportEnd::Overflow)
        );
        a.end(TransportEnd::Closed);
        assert!(matches!(
            queue.recv().await,
            Some(HostEvent::SendProgress { channel_id: 10, .. })
        ));
        assert!(matches!(
            queue.recv().await,
            Some(HostEvent::Ended {
                channel_id: 10,
                reason: TransportEnd::Overflow
            })
        ));
        b.try_send(HostEvent::Frame {
            channel_id: 20,
            text: "{}".into(),
        })
        .unwrap();
        assert!(matches!(
            queue.recv().await,
            Some(HostEvent::Frame { channel_id: 20, .. })
        ));
        queue.close();
        assert!(queue.recv().await.is_none());
    }

    #[tokio::test]
    async fn bytes_are_bounded_and_closed_sinks_cannot_publish_or_register_again() {
        let queue = EventQueue::with_limits(1, 2, 3).unwrap();
        let sink = queue.register(1).unwrap();
        assert!(queue.register(2).is_err());
        sink.try_send(HostEvent::Frame {
            channel_id: 1,
            text: "{}".into(),
        })
        .unwrap();
        assert_eq!(
            sink.try_send(HostEvent::Frame {
                channel_id: 1,
                text: "{}".into()
            }),
            Err(TransportEnd::Overflow)
        );
        queue.recv().await.unwrap();
        queue.recv().await.unwrap();
        assert!(sink
            .try_send(HostEvent::Frame {
                channel_id: 1,
                text: "{}".into()
            })
            .is_err());
        assert!(queue.register(2).is_ok());
    }
}
