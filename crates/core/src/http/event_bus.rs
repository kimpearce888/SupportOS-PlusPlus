//! HTTP event bus — bridges core `LiveEvent`s to SSE clients.
//!
//! The reference Fastify server has an `eventBus` that the route handlers
//! emit into (webhook received, sync updated, rating arrived). Browser
//! clients connect to `/api/events` (SSE) and receive every event in
//! real time, which they use to invalidate TanStack Query caches.
//!
//! In the port we use a tokio `broadcast::Sender<LiveEvent>` because it
//! matches the reference's fan-out semantics:
//!   - many subscribers (browser tabs) can listen concurrently,
//!   - slow subscribers lag and drop events instead of blocking emitters,
//!   - dropping the last sender does NOT close the channel until all
//!     receivers are gone.
//!
//! The bus is held in `AppState` (cloneable). Route handlers that mutate
//! state (webhook receive, conversation reply, ticket status change, etc.)
//! call `bus.emit(&event)` to push to all connected SSE clients.
//!
//! The `BusEmitter` impl of `EventEmitter` lets us reuse the existing
//! `emit_sync_updated` / `emit_webhook_received` / `emit_rating_arrived`
//! helpers without touching their call sites.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::events::{emit_rating_arrived, emit_sync_updated, emit_webhook_received, LiveEvent};

/// A fan-out event bus shared by all HTTP handlers and SSE subscribers.
///
/// Clone is cheap — internally reference-counted.
#[derive(Clone)]
pub struct EventBus {
    tx: Arc<broadcast::Sender<LiveEvent>>,
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
    pub fn emit(&self, event: &LiveEvent) -> usize {
        self.tx.send(event.clone()).unwrap_or(0)
    }

    /// Subscribe to the bus. Each subscriber gets its own receiver.
    /// A subscriber that lags behind more than `capacity` events will
    /// see a `RecvError::Lagged(n)` which we treat as "missed events"
    /// (the next valid event is still delivered).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(Self::default_capacity())
    }
}

/// Adapter so `EventBus` can be used wherever `EventEmitter` is expected.
/// This lets us reuse the `emit_sync_updated` / `emit_webhook_received`
/// / `emit_rating_arrived` helpers without modification.
impl crate::events::EventEmitter for EventBus {
    fn emit(&self, event: &LiveEvent) -> Result<(), String> {
        let _ = EventBus::emit(self, event);
        Ok(())
    }
}

/// Convenience: emit a `WebhookReceived` event on the bus.
pub fn notify_webhook(bus: &EventBus, event_id: &str) {
    emit_webhook_received(bus, event_id);
}

/// Convenience: emit a `SyncUpdated` event on the bus.
pub fn notify_sync(bus: &EventBus, resource: &str, count: u32) {
    emit_sync_updated(bus, resource, count);
}

/// Convenience: emit a `RatingArrived` event on the bus.
pub fn notify_rating(bus: &EventBus, rating_id: &str, rating: u32) {
    emit_rating_arrived(bus, rating_id, rating);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt;

    #[test]
    fn bus_emits_to_subscriber() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.emit(&LiveEvent::WebhookReceived {
            event_id: "evt_1".into(),
        });
        let event = rx.blocking_recv().expect("should receive event");
        assert!(matches!(
            event,
            LiveEvent::WebhookReceived { event_id } if event_id == "evt_1"
        ));
    }

    #[test]
    fn bus_no_subscriber_emits_silently() {
        let bus = EventBus::new(8);
        // No subscribers — emit should not panic.
        let n = bus.emit(&LiveEvent::SyncUpdated {
            resource: "x".into(),
            count: 1,
        });
        assert_eq!(n, 0);
    }

    #[test]
    fn bus_multiple_subscribers_all_receive() {
        let bus = EventBus::new(8);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        bus.emit(&LiveEvent::RatingArrived {
            rating_id: "r1".into(),
            rating: 5,
        });
        let e1 = rx1.blocking_recv().expect("rx1 should receive");
        let e2 = rx2.blocking_recv().expect("rx2 should receive");
        assert!(matches!(e1, LiveEvent::RatingArrived { rating, .. } if rating == 5));
        assert!(matches!(e2, LiveEvent::RatingArrived { rating, .. } if rating == 5));
    }

    #[test]
    fn bus_drops_events_for_slow_subscriber() {
        // Capacity 2 — subsequent emits should cause the receiver to lag.
        // The receiver's first recv should be a `Lagged` notification
        // (or, if the timing is lucky, the latest buffered event).
        // Either way, the bus must NOT block or panic on overflow.
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        bus.emit(&LiveEvent::SyncUpdated {
            resource: "a".into(),
            count: 1,
        });
        bus.emit(&LiveEvent::SyncUpdated {
            resource: "b".into(),
            count: 2,
        });
        bus.emit(&LiveEvent::SyncUpdated {
            resource: "c".into(),
            count: 3,
        });
        bus.emit(&LiveEvent::SyncUpdated {
            resource: "d".into(),
            count: 4,
        });

        // First recv may be `Err(Lagged(_))` or `Ok(_)` — both are valid
        // outcomes of a tokio broadcast channel with capacity 2.
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
        // Verify BroadcastStream interop (used by the SSE handler).
        let bus = EventBus::new(8);
        let rx = bus.subscribe();
        let mut stream = BroadcastStream::new(rx);
        bus.emit(&LiveEvent::SyncUpdated {
            resource: "conversations".into(),
            count: 7,
        });
        let event = stream.next().await.expect("stream should yield");
        assert!(event.is_ok());
    }

    #[test]
    fn bus_event_emitter_trait_compat() {
        // The EventEmitter trait impl should work with the existing helpers.
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        emit_sync_updated(&bus, "conversations", 10);
        emit_webhook_received(&bus, "evt_42");
        emit_rating_arrived(&bus, "r_99", 4);
        let _ = rx.blocking_recv().expect("first event");
        let _ = rx.blocking_recv().expect("second event");
        let _ = rx.blocking_recv().expect("third event");
    }

    #[test]
    fn notify_helpers_dont_panic() {
        let bus = EventBus::new(8);
        notify_webhook(&bus, "evt_1");
        notify_sync(&bus, "conversations", 1);
        notify_rating(&bus, "r_1", 5);
    }
}
