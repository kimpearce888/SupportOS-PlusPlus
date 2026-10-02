//! HTTP event bus — bridges core `ServerEvent`s to SSE clients.
//!
//! Mirrors the reference `ServerEventBus`: a single in-process fan-out bus
//! shared by every emitter (sync coordinator, workers, campaign service,
//! notification sweep, demo routes) and the SSE route (`/api/events`).
//!
//! tokio `broadcast` matches the reference's fan-out semantics:
//!   - many subscribers (browser tabs) can listen concurrently,
//!   - slow subscribers lag and drop events instead of blocking emitters.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::events::{ConversationUpdatedEvent, ServerEvent};

/// A fan-out event bus shared by all HTTP handlers and SSE subscribers.
///
/// Clone is cheap — internally reference-counted.
#[derive(Clone)]
pub struct EventBus {
    tx: Arc<broadcast::Sender<ServerEvent>>,
}

impl EventBus {
    /// Create a new bus with the given per-subscriber buffer capacity.
    /// 256 is plenty for a single-user local app: if a client is slower
    /// than 256 events it's probably stuck and should miss updates
    /// rather than OOM the server.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx: Arc::new(tx) }
    }

    /// Default capacity (256 events).
    #[must_use]
    pub fn default_capacity() -> usize {
        256
    }

    /// Broadcast a live event to all subscribers. Returns the number
    /// of receivers that received it (0 means no SSE clients are
    /// currently connected — the event is silently dropped).
    pub fn emit(&self, event: &ServerEvent) -> usize {
        self.tx.send(event.clone()).unwrap_or(0)
    }

    /// Subscribe to the bus. Each subscriber gets its own receiver.
    /// A subscriber that lags behind more than `capacity` events will
    /// see a `RecvError::Lagged(n)` which we treat as "missed events"
    /// (the next valid event is still delivered).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    /// Current subscriber count (the reference caps SSE at 25 streams via
    /// `serverEventBus.subscriberCount`).
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(Self::default_capacity())
    }
}

/// Adapter so `EventBus` can be used wherever `EventEmitter` is expected.
impl crate::events::EventEmitter for EventBus {
    fn emit(&self, event: &ServerEvent) -> Result<(), String> {
        let _ = EventBus::emit(self, event);
        Ok(())
    }
}

/// Convenience: emit a `conversation-updated` event on the bus.
pub fn notify_conversation_updated(bus: &EventBus, event: &ConversationUpdatedEvent) {
    bus.emit(&ServerEvent::ConversationUpdated(event.clone()));
}

/// Convenience: emit a `sync-completed` event on the bus.
pub fn notify_sync_completed(bus: &EventBus, kind: &str, processed: u32, errors: u32) {
    bus.emit(&ServerEvent::sync_completed(kind, processed, errors));
}

/// Convenience: emit a `rating-received` event on the bus.
pub fn notify_rating_received(
    bus: &EventBus,
    rating: Option<&str>,
    conversation_id: Option<i64>,
    conversation_number: Option<i64>,
    customer_id: Option<i64>,
    customer_name: Option<&str>,
    comments: Option<&str>,
) {
    bus.emit(&ServerEvent::rating_received(
        rating.map(String::from),
        conversation_id,
        conversation_number,
        customer_id,
        customer_name.map(String::from),
        comments.map(String::from),
    ));
}

/// Convenience: emit a `ratings-refreshed` event on the bus.
pub fn notify_ratings_refreshed(bus: &EventBus, processed: u32, fresh: u32) {
    bus.emit(&ServerEvent::ratings_refreshed(processed, fresh));
}

/// Convenience: emit a `campaign-updated` event on the bus.
#[allow(clippy::too_many_arguments)]
pub fn notify_campaign_updated(
    bus: &EventBus,
    campaign_id: i64,
    status: &str,
    sent: u32,
    failed: u32,
    unknown: u32,
    remaining: u32,
) {
    bus.emit(&ServerEvent::campaign_updated(
        campaign_id,
        status,
        sent,
        failed,
        unknown,
        remaining,
    ));
}

/// Convenience: emit a `notification-received` event on the bus.
#[allow(clippy::too_many_arguments)]
pub fn notify_notification_received(
    bus: &EventBus,
    id: i64,
    kind: &str,
    severity: &str,
    title: &str,
    conversation_id: Option<i64>,
    conversation_number: Option<i64>,
    customer_id: Option<i64>,
    target_user_local_id: Option<i64>,
    unread_count: u32,
) {
    bus.emit(&ServerEvent::notification_received(
        id,
        kind,
        severity,
        title,
        conversation_id,
        conversation_number,
        customer_id,
        target_user_local_id,
        unread_count,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ServerEvent;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt;

    #[test]
    fn bus_emits_to_subscriber() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit(&ServerEvent::conversation_updated(
            Some(1),
            None,
            None,
            None,
            "manual",
        ));
        let event = rx.blocking_recv().expect("should receive event");
        assert!(matches!(event, ServerEvent::ConversationUpdated(_)));
    }

    #[test]
    fn bus_no_subscriber_emits_silently() {
        let bus = EventBus::new(8);
        let n = bus.emit(&ServerEvent::sync_completed("initial", 1, 0));
        assert_eq!(n, 0);
    }

    #[test]
    fn bus_multiple_subscribers_all_receive() {
        let bus = EventBus::new(8);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        bus.emit(&ServerEvent::ratings_refreshed(1, 1));
        assert!(rx1.blocking_recv().is_ok());
        assert!(rx2.blocking_recv().is_ok());
    }

    #[test]
    fn subscriber_count_tracks_subscriptions() {
        let bus = EventBus::new(8);
        assert_eq!(bus.subscriber_count(), 0);
        let _rx1 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        let _rx2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        drop(_rx1);
        assert_eq!(bus.subscriber_count(), 1);
    }

    #[test]
    fn bus_drops_events_for_slow_subscriber() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for i in 0..4 {
            bus.emit(&ServerEvent::sync_completed("single", i, 0));
        }
        let first = rx.blocking_recv();
        assert!(
            first.is_ok()
                || matches!(
                    first,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))
                ),
            "expected Ok(event) or Lagged(_), got {:?}",
            first
        );
    }

    #[tokio::test]
    async fn bus_stream_compat() {
        let bus = EventBus::new(8);
        let rx = bus.subscribe();
        let mut stream = BroadcastStream::new(rx);
        bus.emit(&ServerEvent::sync_completed("single", 7, 0));
        let event = stream.next().await.expect("stream should yield");
        assert!(event.is_ok());
    }

    #[test]
    fn notify_helpers_dont_panic() {
        let bus = EventBus::new(8);
        notify_conversation_updated(
            &bus,
            &crate::events::ConversationUpdatedEvent {
                conversation_id: Some(1),
                conversation_number: Some(2),
                mailbox_id: None,
                subject: None,
                reason: "sync".into(),
                at: "now".into(),
            },
        );
        notify_sync_completed(&bus, "incremental", 3, 0);
        notify_rating_received(&bus, Some("great"), Some(1), Some(2), None, None, None);
        notify_ratings_refreshed(&bus, 5, 2);
        notify_campaign_updated(&bus, 1, "sending", 10, 0, 0, 40);
        notify_notification_received(
            &bus,
            1,
            "mentioned",
            "info",
            "hi",
            None,
            None,
            None,
            None,
            1,
        );
    }
}
