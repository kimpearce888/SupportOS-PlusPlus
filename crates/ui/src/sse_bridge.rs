//! The SSE → toast + invalidation bridge (UI-26) — the Leptos counterpart of
//! the reference `ServerEventsBridge` (src/client/api/events.ts:115-190).
//!
//! Mounted ONCE in `AppEffects` (one app-level subscription; pages must not
//! subscribe per-page). Every named event fans out to:
//! - [`queries::invalidate`] calls with the reference's exact query-key
//!   roots (cross-page invalidation), plus a nav-counts refresh wherever
//!   the reference invalidates `'nav-counts'`/`'notification-unread'`;
//! - the reference's toasts: new ratings, webhook pushes, campaign
//!   completion/failure, and CRITICAL notifications (only critical ones
//!   interrupt — the rest wait in the bell badge).
//!
//! The toast text/kind builders are pure functions so the exact reference
//! wording is unit-testable.

use crate::sse::LiveEvent;
use crate::state::UiState;
use crate::toasts::ToastKind;

/// The reference `RATING_LABEL` map.
fn rating_label(rating: &str) -> Option<&'static str> {
    match rating {
        "great" => Some("Great"),
        "okay" => Some("Okay"),
        "not-good" => Some("Not good"),
        _ => None,
    }
}

/// The rating toast (reference: only when `d.rating != null` — the
/// ratings-refreshed payload carries no rating and must not toast).
///
/// Kind: `great` → success, `okay` → info, `not-good` → warning.
/// Message: `New {Label} rating on #{conversationNumber} · {customerName}`.
#[must_use]
pub fn rating_toast(
    rating: Option<&str>,
    conversation_number: Option<i64>,
    customer_name: Option<&str>,
    comments: Option<&str>,
) -> Option<(ToastKind, String, Option<String>)> {
    let rating = rating?;
    let label = rating_label(rating)?;
    let kind = match rating {
        "great" => ToastKind::Success,
        "okay" => ToastKind::Info,
        _ => ToastKind::Warning,
    };
    let mut message = format!("New {label} rating");
    if let Some(n) = conversation_number {
        message.push_str(&format!(" on #{n}"));
    }
    if let Some(name) = customer_name {
        message.push_str(&format!(" · {name}"));
    }
    let detail = comments.filter(|c| !c.is_empty()).map(String::from);
    Some((kind, message, detail))
}

/// The webhook-push toast (reference: only `reason === 'webhook'` AND
/// `conversationNumber != null`). Kind: info. Message:
/// `#{number} updated — pushed by webhook: {subject (≤80 chars)}`.
#[must_use]
pub fn webhook_conversation_toast(
    reason: &str,
    conversation_number: Option<i64>,
    subject: Option<&str>,
) -> Option<(ToastKind, String)> {
    if reason != "webhook" {
        return None;
    }
    let n = conversation_number?;
    let mut message = format!("#{n} updated — pushed by webhook");
    if let Some(subject) = subject.filter(|s| !s.is_empty()) {
        message.push_str(&format!(": {}", truncate_chars(subject, 80)));
    }
    Some((ToastKind::Info, message))
}

/// The campaign toasts (reference): `completed` → success toast; otherwise
/// a run with failures and nothing remaining → warning toast.
#[must_use]
pub fn campaign_toasts(
    campaign_id: i64,
    status: &str,
    sent: u32,
    failed: u32,
    remaining: u32,
) -> Vec<(ToastKind, String)> {
    let mut out = Vec::new();
    if status == "completed" {
        let mut message = format!("Campaign #{campaign_id} completed — {sent} sent");
        if failed > 0 {
            message.push_str(&format!(", {failed} failed"));
        }
        message.push('.');
        out.push((ToastKind::Success, message));
    } else if failed > 0 && remaining == 0 {
        out.push((
            ToastKind::Warning,
            format!("Campaign #{campaign_id} finished with {failed} failed recipient(s)."),
        ));
    }
    out
}

/// The critical-notification toast (reference: only `severity ===
/// 'critical'` interrupts with a toast — SLA breach, sync failure). Kind:
/// error; message: the notification title.
#[must_use]
pub fn critical_notification_toast(severity: &str, title: &str) -> Option<(ToastKind, String)> {
    if severity == "critical" {
        Some((ToastKind::Error, title.to_string()))
    } else {
        None
    }
}

/// Truncate on a char boundary (JS `String.slice(0, 80)` counts UTF-16
/// code units; the port counts chars — both keep whole graphemes intact
/// for the subjects Help Scout serves).
#[must_use]
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Handle one live event the way the reference bridge does: invalidate the
/// query roots, refresh the nav badges where the reference invalidates
/// nav-counts/notification-unread, and push the event's toasts.
pub fn handle_event(ui: &UiState, event: &LiveEvent) {
    match event {
        LiveEvent::RatingReceived {
            rating,
            conversation_number,
            customer_name,
            comments,
            ..
        } => {
            for key in crate::queries::keys_for_event("ratings", None, 0) {
                crate::queries::invalidate(key);
            }
            if let Some((kind, message, detail)) = rating_toast(
                rating.as_deref(),
                *conversation_number,
                customer_name.as_deref(),
                comments.as_deref(),
            ) {
                crate::toasts::push(kind, message, detail);
            }
        }
        LiveEvent::RatingsRefreshed { .. } => {
            for key in crate::queries::keys_for_event("ratings", None, 0) {
                crate::queries::invalidate(key);
            }
        }
        LiveEvent::SyncCompleted {
            kind, processed, ..
        } => {
            for key in crate::queries::keys_for_event("sync", Some(kind), *processed) {
                crate::queries::invalidate(key);
            }
            ui.refresh_nav_counts();
        }
        LiveEvent::ConversationUpdated {
            conversation_number,
            subject,
            reason,
            ..
        } => {
            for key in crate::queries::keys_for_event("conversation", None, 0) {
                crate::queries::invalidate(key);
            }
            ui.refresh_nav_counts();
            if let Some((kind, message)) =
                webhook_conversation_toast(reason, *conversation_number, subject.as_deref())
            {
                crate::toasts::push(kind, message, None);
            }
        }
        LiveEvent::CampaignUpdated {
            campaign_id,
            status,
            sent,
            failed,
            remaining,
            ..
        } => {
            for key in crate::queries::keys_for_event("campaign", None, 0) {
                crate::queries::invalidate(key);
            }
            for (kind, message) in campaign_toasts(*campaign_id, status, *sent, *failed, *remaining)
            {
                crate::toasts::push(kind, message, None);
            }
        }
        LiveEvent::NotificationReceived {
            severity, title, ..
        } => {
            for key in crate::queries::keys_for_event("notification", None, 0) {
                crate::queries::invalidate(key);
            }
            ui.refresh_nav_counts();
            if let Some((kind, message)) = critical_notification_toast(severity, title) {
                crate::toasts::push(kind, message, None);
            }
        }
        // `hello` is the connect handshake; the `error` event (e.g. the
        // too-many-streams stream error) is subscribed to but carries no
        // bridge action in the reference either — the browser reconnects.
        LiveEvent::Hello { .. } | LiveEvent::Error { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rating_toast_matches_reference_wording() {
        // great → success, "New Great rating on #42 · Ada Lovelace"
        let (kind, message, detail) = rating_toast(
            Some("great"),
            Some(42),
            Some("Ada Lovelace"),
            Some("Fast fix, thanks!"),
        )
        .expect("rating toast");
        assert_eq!(kind, ToastKind::Success);
        assert_eq!(message, "New Great rating on #42 · Ada Lovelace");
        assert_eq!(detail.as_deref(), Some("Fast fix, thanks!"));

        // okay → info, no name
        let (kind, message, _) =
            rating_toast(Some("okay"), None, None, None).expect("rating toast");
        assert_eq!(kind, ToastKind::Info);
        assert_eq!(message, "New Okay rating");

        // not-good → warning, no comments → no detail
        let (kind, message, detail) =
            rating_toast(Some("not-good"), Some(7), None, Some("")).expect("rating toast");
        assert_eq!(kind, ToastKind::Warning);
        assert_eq!(message, "New Not good rating on #7");
        assert!(detail.is_none(), "empty comments → no detail");
    }

    #[test]
    fn rating_toast_skips_refreshed_payload_and_unknown_ratings() {
        // RatingsRefreshed carries rating: None — no toast.
        assert!(rating_toast(None, Some(1), None, None).is_none());
        // Unknown rating value (label miss) — no toast.
        assert!(rating_toast(Some("meh"), Some(1), None, None).is_none());
    }

    #[test]
    fn webhook_toast_only_for_webhook_reason() {
        let (kind, message) = webhook_conversation_toast("webhook", Some(42), Some("SSO broken"))
            .expect("webhook toast");
        assert_eq!(kind, ToastKind::Info);
        assert_eq!(message, "#42 updated — pushed by webhook: SSO broken");

        // sync/manual reasons never toast
        assert!(webhook_conversation_toast("sync", Some(42), Some("x")).is_none());
        assert!(webhook_conversation_toast("manual", Some(42), Some("x")).is_none());
        // webhook reason but no number — the reference guard fails
        assert!(webhook_conversation_toast("webhook", None, Some("x")).is_none());
    }

    #[test]
    fn webhook_toast_truncates_subject_to_eighty() {
        let long = "x".repeat(200);
        let (_, message) =
            webhook_conversation_toast("webhook", Some(9), Some(&long)).expect("toast");
        assert_eq!(
            message,
            format!("#9 updated — pushed by webhook: {}", "x".repeat(80))
        );
    }

    #[test]
    fn campaign_toasts_completed_and_failed_shapes() {
        // completed, no failures
        assert_eq!(
            campaign_toasts(3, "completed", 12, 0, 0),
            vec![(
                ToastKind::Success,
                "Campaign #3 completed — 12 sent.".to_string()
            )]
        );
        // completed WITH failures
        assert_eq!(
            campaign_toasts(3, "completed", 10, 2, 0),
            vec![(
                ToastKind::Success,
                "Campaign #3 completed — 10 sent, 2 failed.".to_string()
            )]
        );
        // not completed, failures exhausted → warning
        assert_eq!(
            campaign_toasts(4, "sending", 10, 3, 0),
            vec![(
                ToastKind::Warning,
                "Campaign #4 finished with 3 failed recipient(s).".to_string()
            )]
        );
        // still sending with failures remaining → silence (more events will come)
        assert!(campaign_toasts(4, "sending", 10, 3, 5).is_empty());
        // healthy running campaign → silence
        assert!(campaign_toasts(4, "sending", 10, 0, 5).is_empty());
    }

    #[test]
    fn critical_notification_toast_only_for_critical() {
        assert_eq!(
            critical_notification_toast("critical", "SLA breach on #100"),
            Some((ToastKind::Error, "SLA breach on #100".to_string()))
        );
        assert!(critical_notification_toast("warning", "x").is_none());
        assert!(critical_notification_toast("info", "x").is_none());
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo");
        assert_eq!(truncate_chars("abc", 80), "abc");
    }
}
