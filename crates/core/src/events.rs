//! Server event emission — mirrors the reference `ServerEventBus`
//! (`src/server/services/eventBus.ts`) exactly.
//!
//! The reference has SIX internal event types, each with a fixed payload
//! shape, fanned out to SSE clients on `/api/events` under the wire names:
//!
//! | internal (kebab)      | SSE `event:` name | payload shape                |
//! |-----------------------|-------------------|------------------------------|
//! | rating-received       | `ratings`         | RatingReceivedEvent          |
//! | ratings-refreshed     | `ratings`         | RatingsRefreshedEvent        |
//! | sync-completed        | `sync`            | SyncCompletedEvent           |
//! | conversation-updated  | `conversation`    | ConversationUpdatedEvent     |
//! | campaign-updated      | `campaign`        | CampaignUpdatedEvent         |
//! | notification-received | `notification`    | NotificationReceivedEvent    |
//!
//! Emitters (reference parity):
//! - sync coordinator → sync-completed, rating-received, ratings-refreshed
//! - workers (conversation writes, single-conversation sync) → conversation-updated
//! - campaign service → campaign-updated
//! - notification sweep → notification-received
//! - demo simulate-rating → rating-received + ratings-refreshed
//!
//! The Tauri shell forwards these over `app.emit()` channels; tests capture
//! them with a `RecordingEmitter`.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// A CSAT rating value ('great' | 'okay' | 'not-good' | null) — the exact
/// reference vocabulary.
pub type RatingValue = Option<String>;

/// `rating-received` — a NEW rating arrived (watcher, webhook or demo).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RatingReceivedEvent {
    pub rating: RatingValue,
    pub conversation_id: Option<i64>,
    pub conversation_number: Option<i64>,
    pub customer_id: Option<i64>,
    pub customer_name: Option<String>,
    pub comments: Option<String>,
    pub at: String,
}

/// `ratings-refreshed` — the periodic ratings sweep finished.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RatingsRefreshedEvent {
    pub processed: u32,
    pub fresh: u32,
    pub at: String,
}

/// `sync-completed` — a sync pass finished.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncCompletedEvent {
    /// 'initial' | 'incremental' | 'reconciliation' | 'single'
    pub kind: String,
    pub processed: u32,
    pub errors: u32,
    pub at: String,
}

/// `conversation-updated` — one conversation changed in the mirror.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationUpdatedEvent {
    pub conversation_id: Option<i64>,
    pub conversation_number: Option<i64>,
    pub mailbox_id: Option<i64>,
    pub subject: Option<String>,
    /// 'webhook' | 'sync' | 'manual'
    pub reason: String,
    pub at: String,
}

/// `campaign-updated` — an outreach campaign's state/progress changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CampaignUpdatedEvent {
    pub campaign_id: i64,
    pub status: String,
    pub sent: u32,
    pub failed: u32,
    pub unknown: u32,
    pub remaining: u32,
    pub at: String,
}

/// `notification-received` — a NEW notification was created (M2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationReceivedEvent {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    /// 'info' | 'warning' | 'critical'
    pub severity: String,
    pub title: String,
    pub conversation_id: Option<i64>,
    pub conversation_number: Option<i64>,
    pub customer_id: Option<i64>,
    pub target_user_local_id: Option<i64>,
    pub unread_count: u32,
    pub at: String,
}

/// The live event types emitted by the Rust core (reference ServerEventMap).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ServerEvent {
    RatingReceived(RatingReceivedEvent),
    RatingsRefreshed(RatingsRefreshedEvent),
    SyncCompleted(SyncCompletedEvent),
    ConversationUpdated(ConversationUpdatedEvent),
    CampaignUpdated(CampaignUpdatedEvent),
    NotificationReceived(NotificationReceivedEvent),
}

impl ServerEvent {
    /// The SSE wire name (`event:` field) for this event — exactly the
    /// reference mapping in `routes/events.ts`.
    #[must_use]
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::RatingReceived { .. } | Self::RatingsRefreshed { .. } => "ratings",
            Self::SyncCompleted { .. } => "sync",
            Self::ConversationUpdated { .. } => "conversation",
            Self::CampaignUpdated { .. } => "campaign",
            Self::NotificationReceived { .. } => "notification",
        }
    }

    /// The Tauri event channel name (forwarded by the shell to the webview).
    #[must_use]
    pub fn channel(&self) -> &'static str {
        match self {
            Self::RatingReceived { .. } | Self::RatingsRefreshed { .. } => "spp://rating/arrived",
            Self::SyncCompleted { .. } | Self::ConversationUpdated { .. } => "spp://sync/updated",
            Self::CampaignUpdated { .. } => "spp://campaign/updated",
            Self::NotificationReceived { .. } => "spp://notification/received",
        }
    }

    /// The JSON payload for the SSE `data:` line (camelCase, reference shape).
    #[must_use]
    pub fn payload_json(&self) -> String {
        match self {
            Self::RatingReceived(e) => serde_json::to_string(e).unwrap_or_else(|_| "{}".into()),
            Self::RatingsRefreshed(e) => serde_json::to_string(e).unwrap_or_else(|_| "{}".into()),
            Self::SyncCompleted(e) => serde_json::to_string(e).unwrap_or_else(|_| "{}".into()),
            Self::ConversationUpdated(e) => {
                serde_json::to_string(e).unwrap_or_else(|_| "{}".into())
            }
            Self::CampaignUpdated(e) => serde_json::to_string(e).unwrap_or_else(|_| "{}".into()),
            Self::NotificationReceived(e) => {
                serde_json::to_string(e).unwrap_or_else(|_| "{}".into())
            }
        }
    }

    /// UTC now in the reference's ISO format.
    fn now_iso() -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    /// Build a `conversation-updated` event with `at` stamped now.
    #[must_use]
    pub fn conversation_updated(
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        mailbox_id: Option<i64>,
        subject: Option<String>,
        reason: &str,
    ) -> Self {
        Self::ConversationUpdated(ConversationUpdatedEvent {
            conversation_id,
            conversation_number,
            mailbox_id,
            subject,
            reason: reason.to_string(),
            at: Self::now_iso(),
        })
    }

    /// Build a `sync-completed` event with `at` stamped now.
    #[must_use]
    pub fn sync_completed(kind: &str, processed: u32, errors: u32) -> Self {
        Self::SyncCompleted(SyncCompletedEvent {
            kind: kind.to_string(),
            processed,
            errors,
            at: Self::now_iso(),
        })
    }

    /// Build a `rating-received` event with `at` stamped now.
    #[must_use]
    pub fn rating_received(
        rating: RatingValue,
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        customer_id: Option<i64>,
        customer_name: Option<String>,
        comments: Option<String>,
    ) -> Self {
        Self::RatingReceived(RatingReceivedEvent {
            rating,
            conversation_id,
            conversation_number,
            customer_id,
            customer_name,
            comments,
            at: Self::now_iso(),
        })
    }

    /// Build a `ratings-refreshed` event with `at` stamped now.
    #[must_use]
    pub fn ratings_refreshed(processed: u32, fresh: u32) -> Self {
        Self::RatingsRefreshed(RatingsRefreshedEvent {
            processed,
            fresh,
            at: Self::now_iso(),
        })
    }

    /// Build a `campaign-updated` event with `at` stamped now.
    #[must_use]
    pub fn campaign_updated(
        campaign_id: i64,
        status: &str,
        sent: u32,
        failed: u32,
        unknown: u32,
        remaining: u32,
    ) -> Self {
        Self::CampaignUpdated(CampaignUpdatedEvent {
            campaign_id,
            status: status.to_string(),
            sent,
            failed,
            unknown,
            remaining,
            at: Self::now_iso(),
        })
    }

    /// Build a `notification-received` event with `at` stamped now.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn notification_received(
        id: i64,
        kind: &str,
        severity: &str,
        title: &str,
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        customer_id: Option<i64>,
        target_user_local_id: Option<i64>,
        unread_count: u32,
    ) -> Self {
        Self::NotificationReceived(NotificationReceivedEvent {
            id,
            kind: kind.to_string(),
            severity: severity.to_string(),
            title: title.to_string(),
            conversation_id,
            conversation_number,
            customer_id,
            target_user_local_id,
            unread_count,
            at: Self::now_iso(),
        })
    }
}

/// The trait the Tauri shell implements to emit events to the UI.
/// In production this calls `app_handle.emit(channel, payload)`.
/// In tests, `RecordingEmitter` captures the events for assertion.
pub trait EventEmitter: Send + Sync {
    /// Emit a live event. Returns Ok(()) on success.
    fn emit(&self, event: &ServerEvent) -> Result<(), String>;
}

/// A test-only emitter that records all emitted events in a Vec.
/// Thread-safe via Mutex.
pub struct RecordingEmitter {
    events: Mutex<Vec<ServerEvent>>,
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
    pub fn events(&self) -> Vec<ServerEvent> {
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

    /// Whether no events were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for RecordingEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter for RecordingEmitter {
    fn emit(&self, event: &ServerEvent) -> Result<(), String> {
        self.events
            .lock()
            .expect("RecordingEmitter mutex poisoned")
            .push(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_reference() {
        assert_eq!(
            ServerEvent::rating_received(None, None, None, None, None, None).wire_name(),
            "ratings"
        );
        assert_eq!(ServerEvent::ratings_refreshed(1, 1).wire_name(), "ratings");
        assert_eq!(
            ServerEvent::sync_completed("initial", 10, 0).wire_name(),
            "sync"
        );
        assert_eq!(
            ServerEvent::conversation_updated(
                Some(1),
                Some(100),
                Some(2),
                Some("s".into()),
                "webhook"
            )
            .wire_name(),
            "conversation"
        );
        assert_eq!(
            ServerEvent::campaign_updated(1, "running", 1, 0, 0, 9).wire_name(),
            "campaign"
        );
        assert_eq!(
            ServerEvent::notification_received(
                1,
                "mentioned",
                "info",
                "t",
                None,
                None,
                None,
                None,
                1
            )
            .wire_name(),
            "notification"
        );
    }

    #[test]
    fn conversation_updated_payload_is_camel_case_reference_shape() {
        let e = ServerEvent::conversation_updated(
            Some(5),
            Some(1042),
            Some(2),
            Some("SSO broken".into()),
            "webhook",
        );
        let json = e.payload_json();
        assert!(json.contains("\"conversationId\":5"));
        assert!(json.contains("\"conversationNumber\":1042"));
        assert!(json.contains("\"mailboxId\":2"));
        assert!(json.contains("\"subject\":\"SSO broken\""));
        assert!(json.contains("\"reason\":\"webhook\""));
        assert!(json.contains("\"at\":\""));
    }

    #[test]
    fn rating_received_payload_shape() {
        let e = ServerEvent::rating_received(
            Some("not-good".into()),
            Some(9),
            Some(1042),
            Some(3),
            Some("Ada Lovelace".into()),
            Some("still broken".into()),
        );
        let json = e.payload_json();
        assert!(json.contains("\"rating\":\"not-good\""));
        assert!(json.contains("\"conversationId\":9"));
        assert!(json.contains("\"customerName\":\"Ada Lovelace\""));
        assert!(json.contains("\"comments\":\"still broken\""));
    }

    #[test]
    fn sync_completed_payload_shape() {
        let json = ServerEvent::sync_completed("incremental", 12, 1).payload_json();
        assert!(json.contains("\"kind\":\"incremental\""));
        assert!(json.contains("\"processed\":12"));
        assert!(json.contains("\"errors\":1"));
    }

    #[test]
    fn notification_payload_uses_type_field() {
        let json = ServerEvent::notification_received(
            7,
            "sla_breach",
            "critical",
            "SLA breached",
            Some(3),
            Some(99),
            Some(1),
            None,
            4,
        )
        .payload_json();
        assert!(json.contains("\"type\":\"sla_breach\""));
        assert!(json.contains("\"severity\":\"critical\""));
        assert!(json.contains("\"unreadCount\":4"));
        assert!(json.contains("\"targetUserLocalId\":null"));
    }

    #[test]
    fn campaign_payload_shape() {
        let json = ServerEvent::campaign_updated(2, "completed", 50, 2, 1, 0).payload_json();
        assert!(json.contains("\"campaignId\":2"));
        assert!(json.contains("\"status\":\"completed\""));
        assert!(json.contains("\"sent\":50"));
        assert!(json.contains("\"unknown\":1"));
        assert!(json.contains("\"remaining\":0"));
    }

    #[test]
    fn channels_are_stable() {
        assert_eq!(
            ServerEvent::rating_received(None, None, None, None, None, None).channel(),
            "spp://rating/arrived"
        );
        assert_eq!(
            ServerEvent::sync_completed("single", 1, 0).channel(),
            "spp://sync/updated"
        );
        assert_eq!(
            ServerEvent::campaign_updated(1, "running", 0, 0, 0, 10).channel(),
            "spp://campaign/updated"
        );
        assert_eq!(
            ServerEvent::notification_received(
                1,
                "mentioned",
                "info",
                "t",
                None,
                None,
                None,
                None,
                1
            )
            .channel(),
            "spp://notification/received"
        );
    }

    #[test]
    fn recording_emitter_captures() {
        let em = RecordingEmitter::new();
        assert!(em.is_empty());
        em.emit(&ServerEvent::ratings_refreshed(3, 2)).unwrap();
        assert_eq!(em.len(), 1);
        assert!(matches!(em.events()[0], ServerEvent::RatingsRefreshed(_)));
    }

    #[test]
    fn serde_roundtrip_conversation_updated() {
        let e = ServerEvent::conversation_updated(Some(1), None, None, None, "manual");
        let json = serde_json::to_string(&e).unwrap();
        let back: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
