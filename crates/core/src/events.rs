//! Live event emission (M2-T08).
//!
//! Per spec M2: "live events". The Rust core emits events when sync writes
//! data, when a webhook is received, and when a rating arrives. The Tauri
//! shell listens for these and forwards them to the UI via `app.emit()`.
//!
//! This module defines the event types + an `EventEmitter` trait that the
//! Tauri shell implements. Tests use a `RecordingEmitter` that captures
//! events for assertion.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The live event types emitted by the Rust core.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum LiveEvent {
    /// Emitted when a sync job writes data to SQLite (any resource).
    /// The UI uses this to refresh lists without polling.
    SyncUpdated { resource: String, count: u32 },
    /// Emitted when a webhook event is received and accepted (persisted +
    /// HMAC-verified + job enqueued). The UI uses this to show a live
    /// indicator on the Sync Health page.
    WebhookReceived { event_id: String },
    /// Emitted when a CSAT rating arrives (via the ratings watcher or
    /// demo-mode tool). The UI uses this to refresh the ratings display.
    RatingArrived { rating_id: String, rating: u32 },
}

impl LiveEvent {
    /// The Tauri event channel name (e.g. `"spp://sync/updated"`).
    #[must_use]
    pub fn channel(&self) -> &'static str {
        match self {
            Self::SyncUpdated { .. } => "spp://sync/updated",
            Self::WebhookReceived { .. } => "spp://webhook/received",
            Self::RatingArrived { .. } => "spp://rating/arrived",
        }
    }
}

/// The trait the Tauri shell implements to emit events to the UI.
/// In production this calls `app_handle.emit(channel, payload)`.
/// In tests, `RecordingEmitter` captures the events for assertion.
pub trait EventEmitter: Send + Sync {
    /// Emit a live event. Returns Ok(()) on success.
    fn emit(&self, event: &LiveEvent) -> Result<(), String>;
}

/// A test-only emitter that records all emitted events in a Vec.
/// Thread-safe via Mutex.
pub struct RecordingEmitter {
    events: Mutex<Vec<LiveEvent>>,
}

impl RecordingEmitter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Returns a clone of all recorded events.
    #[must_use]
    pub fn events(&self) -> Vec<LiveEvent> {
        self.events
            .lock()
            .expect("RecordingEmitter mutex poisoned")
            .clone()
    }

    /// Returns the count of recorded events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events
            .lock()
            .expect("RecordingEmitter mutex poisoned")
            .len()
    }

    /// Returns true if no events have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all recorded events.
    pub fn clear(&self) {
        self.events
            .lock()
            .expect("RecordingEmitter mutex poisoned")
            .clear();
    }
}

impl Default for RecordingEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter for RecordingEmitter {
    fn emit(&self, event: &LiveEvent) -> Result<(), String> {
        self.events
            .lock()
            .expect("RecordingEmitter mutex poisoned")
            .push(event.clone());
        Ok(())
    }
}

/// Emit a `SyncUpdated` event.
pub fn emit_sync_updated(emitter: &dyn EventEmitter, resource: &str, count: u32) {
    let event = LiveEvent::SyncUpdated {
        resource: resource.to_string(),
        count,
    };
    if let Err(e) = emitter.emit(&event) {
        tracing::warn!(error = %e, "failed to emit SyncUpdated event");
    }
}

/// Emit a `WebhookReceived` event.
pub fn emit_webhook_received(emitter: &dyn EventEmitter, event_id: &str) {
    let event = LiveEvent::WebhookReceived {
        event_id: event_id.to_string(),
    };
    if let Err(e) = emitter.emit(&event) {
        tracing::warn!(error = %e, "failed to emit WebhookReceived event");
    }
}

/// Emit a `RatingArrived` event.
pub fn emit_rating_arrived(emitter: &dyn EventEmitter, rating_id: &str, rating: u32) {
    let event = LiveEvent::RatingArrived {
        rating_id: rating_id.to_string(),
        rating,
    };
    if let Err(e) = emitter.emit(&event) {
        tracing::warn!(error = %e, "failed to emit RatingArrived event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_event_channels() {
        assert_eq!(
            LiveEvent::SyncUpdated {
                resource: "conversations".into(),
                count: 10
            }
            .channel(),
            "spp://sync/updated"
        );
        assert_eq!(
            LiveEvent::WebhookReceived {
                event_id: "evt_1".into()
            }
            .channel(),
            "spp://webhook/received"
        );
        assert_eq!(
            LiveEvent::RatingArrived {
                rating_id: "r_1".into(),
                rating: 5
            }
            .channel(),
            "spp://rating/arrived"
        );
    }

    #[test]
    fn recording_emitter_captures_events() {
        let emitter = RecordingEmitter::new();
        assert!(emitter.is_empty());

        emit_sync_updated(&emitter, "conversations", 10);
        assert_eq!(emitter.len(), 1);

        emit_webhook_received(&emitter, "evt_001");
        assert_eq!(emitter.len(), 2);

        emit_rating_arrived(&emitter, "r_001", 5);
        assert_eq!(emitter.len(), 3);

        let events = emitter.events();
        assert!(matches!(
            &events[0],
            LiveEvent::SyncUpdated { resource, count } if resource == "conversations" && *count == 10
        ));
        assert!(matches!(
            &events[1],
            LiveEvent::WebhookReceived { event_id } if event_id == "evt_001"
        ));
        assert!(matches!(
            &events[2],
            LiveEvent::RatingArrived { rating_id, rating } if rating_id == "r_001" && *rating == 5
        ));
    }

    #[test]
    fn recording_emitter_clear() {
        let emitter = RecordingEmitter::new();
        emit_sync_updated(&emitter, "mailboxes", 2);
        assert_eq!(emitter.len(), 1);
        emitter.clear();
        assert!(emitter.is_empty());
    }

    #[test]
    fn live_event_serializes_to_json() {
        let event = LiveEvent::SyncUpdated {
            resource: "conversations".into(),
            count: 42,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"SyncUpdated\""));
        assert!(json.contains("\"resource\":\"conversations\""));
        assert!(json.contains("\"count\":42"));
    }

    #[test]
    fn live_event_deserializes_from_json() {
        let json = r#"{"type":"WebhookReceived","event_id":"evt_123"}"#;
        let event: LiveEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, LiveEvent::WebhookReceived { event_id } if event_id == "evt_123"));
    }

    #[test]
    fn emit_helpers_dont_panic_on_emitter_error() {
        struct FailingEmitter;
        impl EventEmitter for FailingEmitter {
            fn emit(&self, _event: &LiveEvent) -> Result<(), String> {
                Err("simulated failure".into())
            }
        }
        let emitter = FailingEmitter;
        // These should log a warning but not panic.
        emit_sync_updated(&emitter, "test", 1);
        emit_webhook_received(&emitter, "test");
        emit_rating_arrived(&emitter, "test", 5);
    }
}
