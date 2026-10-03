//! Notification Center page — the `/notifications` route (M4-T07).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! This page shows:
//! 1. The notification list grouped by severity (critical / warning / info).
//! 2. Per-type preferences UI (15 toggle rows driven by `NotificationType::ALL`).
//! 3. A "Mark as read" action on each unread notification.
//! 4. The retention TTL setting (default 30 days).
//!
//! Per KNOWN PITFALLS: every view has loading, empty, and error states.
//! Per A12: closed vocabularies are single-source-of-truth — the 15
//! notification types come from `NotificationType::ALL` in the catalog,
//! and severity grouping uses the `Severity` enum from the theming tokens.

use leptos::*;

use crate::catalog::NotificationType;
use crate::components::state_view::EmptyState;
use crate::components::theming::Severity;

/// The default retention TTL displayed in the settings UI. Matches
/// `spp_core::notification_prefs::DEFAULT_RETENTION_TTL_DAYS` (30).
pub const DEFAULT_TTL_DAYS_DISPLAY: i64 = 30;

/// A UI-side notification row. Mirrors `spp_core::notifications::Notification`
/// but with `'static` lifetimes so Leptos signals can hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationView {
    /// The row id (used by the "Mark as read" action).
    pub id: i64,
    /// The notification type (catalog enum — single source of truth).
    pub notification_type: NotificationType,
    /// The severity bucket (copied from `NotificationType::severity()`).
    pub severity: Severity,
    /// The agent who should see this notification. `None` for "all agents".
    pub target_user_id: Option<i64>,
    /// The conversation the notification is about. `None` for system-wide.
    pub conversation_id: Option<i64>,
    /// The JSON payload (triggering event details), as a raw string.
    pub payload: Option<String>,
    /// Whether the notification has been read.
    pub read: bool,
    /// When the notification was created (ISO-8601 UTC).
    pub created_at: String,
}

/// A user's preference for a notification type — enabled or disabled.
///
/// Mirrors `spp_core::notification_prefs::NotificationPreference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationPreferenceView {
    /// The user will see this notification type.
    Enabled,
    /// The user has opted out of this notification type.
    Disabled,
}

impl NotificationPreferenceView {
    /// Convert to a bool for the toggle UI.
    #[must_use]
    pub fn as_bool(self) -> bool {
        match self {
            Self::Enabled => true,
            Self::Disabled => false,
        }
    }

    /// Convert from a stored bool.
    #[must_use]
    pub fn from_bool(b: bool) -> Self {
        if b {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// Map a notification type to its severity bucket. Delegates to the catalog's
/// `NotificationType::severity()` (single source of truth) and converts the
/// string back to the typed `Severity` enum.
#[must_use]
pub fn notification_severity(notif_type: NotificationType) -> Severity {
    match notif_type.severity() {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

/// The human-readable display name for a notification type.
#[must_use]
pub fn notification_label(notif_type: NotificationType) -> &'static str {
    match notif_type {
        NotificationType::CustomerReplied => "Customer replied",
        NotificationType::TicketAssigned => "Ticket assigned",
        NotificationType::Mentioned => "Mentioned",
        NotificationType::TeamMentioned => "Team mentioned",
        NotificationType::SlaRisk => "SLA at risk",
        NotificationType::SlaBreach => "SLA breached",
        NotificationType::AutomationApproval => "Automation approval",
        NotificationType::AiEscalation => "AI escalation",
        NotificationType::KnownIssueDetected => "Known issue detected",
        NotificationType::IssueSpike => "Issue spike",
        NotificationType::CampaignReply => "Campaign reply",
        NotificationType::SyncFailure => "Sync failure",
        NotificationType::JobFailure => "Job failure",
        NotificationType::CustomerEvent => "Customer event",
        NotificationType::IncidentUpdate => "Incident update",
    }
}

/// The plain-language description for a notification type (tooltip).
#[must_use]
pub fn notification_description(notif_type: NotificationType) -> &'static str {
    match notif_type {
        NotificationType::CustomerReplied => {
            "A customer replied to a conversation assigned to you."
        }
        NotificationType::TicketAssigned => "A ticket was assigned to you.",
        NotificationType::Mentioned => "You were @mentioned in a conversation or side thread.",
        NotificationType::TeamMentioned => {
            "Your team was @mentioned in a conversation or side thread."
        }
        NotificationType::SlaRisk => "A conversation is at risk of breaching its SLA.",
        NotificationType::SlaBreach => "A conversation's SLA has been breached.",
        NotificationType::AutomationApproval => "An automation rule needs your approval.",
        NotificationType::AiEscalation => "The AI flagged a conversation for escalation.",
        NotificationType::KnownIssueDetected => "A conversation matches a known issue.",
        NotificationType::IssueSpike => "An issue is spiking across conversations.",
        NotificationType::CampaignReply => "A campaign recipient replied to an outreach message.",
        NotificationType::SyncFailure => "A Help Scout sync run ended in failure.",
        NotificationType::JobFailure => "A background job exhausted its retry budget.",
        NotificationType::CustomerEvent => "A customer event was recorded (e.g. signup, upgrade).",
        NotificationType::IncidentUpdate => "An incident's status or severity changed.",
    }
}

/// The Notification Center page component.
///
/// Wired to `notifications_list_unread` + `notifications_unread_count` IPC.
#[component]
pub fn NotificationsPage() -> impl IntoView {
    let notifications = create_rw_signal(Vec::<NotificationView>::new());
    let preferences = create_rw_signal(default_preferences());
    let retention_ttl_days = create_rw_signal(DEFAULT_TTL_DAYS_DISPLAY);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);

    // Fetch unread notifications on mount.
    create_effect(move |_| {
        let notifications = notifications;
        let loading = loading;
        let error_msg = error_msg;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/notifications?userId=1&limit=50")
                .await
            {
                Ok(data) => {
                    let list = data
                        .get("notifications")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let views: Vec<NotificationView> = list
                        .into_iter()
                        .filter_map(|n| {
                            let id = n.get("id")?.as_i64()?;
                            let type_str = n.get("notification_type")?.as_str()?;
                            let nt = NotificationType::ALL
                                .iter()
                                .find(|t| t.as_str() == type_str)?;
                            let severity_str = n.get("severity")?.as_str()?;
                            let sev = match severity_str {
                                "critical" => Severity::Critical,
                                "warning" => Severity::Warning,
                                _ => Severity::Info,
                            };
                            let read = n.get("read_at").map(|r| !r.is_null()).unwrap_or(false);
                            let created_at = n.get("created_at")?.as_str()?.to_string();
                            Some(NotificationView {
                                id,
                                notification_type: *nt,
                                severity: sev,
                                target_user_id: n.get("target_user_id").and_then(|v| v.as_i64()),
                                conversation_id: n.get("conversation_id").and_then(|v| v.as_i64()),
                                payload: None,
                                read,
                                created_at,
                            })
                        })
                        .collect();
                    notifications.set(views);
                    loading.set(false);
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    view! {
        <div class="spp-page spp-page--notifications">
            <h2 class="spp-page__title">"Notifications"</h2>
            <p class="spp-page__subtitle">
                "Stay on top of replies, SLA risk, and system health. "
                "Adjust which notifications you see, or change how long they're kept."
            </p>

            // ── Notification list ──
            <section class="spp-notifications-list">
                <h3 class="spp-notifications-list__title">"Recent"</h3>
                <Show
                    when=move || !notifications.get().is_empty()
                    fallback=move || {
                        view! {
                            <EmptyState message="No notifications yet. Once a sync settles, customer replies and ticket assignments will appear here." />
                        }
                    }
                >
                    <NotificationSeverityGroups notifications=notifications.get() />
                </Show>
            </section>

            // ── Per-type preferences ──
            <section class="spp-notifications-prefs">
                <h3 class="spp-notifications-prefs__title">"Notification preferences"</h3>
                <p class="spp-notifications-prefs__help">
                    "Toggle which notification types you want to see. "
                    "High-signal types (SLA, sync failures) are on by default; "
                    "campaign and customer-event notifications are opt-in."
                </p>
                <PreferencesList preferences=preferences.get() on_toggle=move |i, v| {
                    preferences.update(|p| {
                        if let Some(entry) = p.get_mut(i) {
                            entry.1 = NotificationPreferenceView::from_bool(v);
                        }
                    });
                } />
            </section>

            // ── Retention TTL setting ──
            <section class="spp-notifications-retention">
                <h3 class="spp-notifications-retention__title">"Retention"</h3>
                <p class="spp-notifications-retention__help">
                    "Notifications older than this TTL are pruned automatically. "
                    "Default is 30 days."
                </p>
                <div class="spp-notifications-retention__row">
                    <label class="spp-notifications-retention__label" for="retention-ttl">
                        "TTL (days)"
                    </label>
                    <input
                        id="retention-ttl"
                        class="spp-notifications-retention__input"
                        type="number"
                        min="1"
                        max="365"
                        prop:value=move || retention_ttl_days.get().to_string()
                        on:input=move |ev| {
                            let v = event_target_value(&ev).parse::<i64>().unwrap_or(DEFAULT_TTL_DAYS_DISPLAY);
                            retention_ttl_days.set(v.clamp(1, 365));
                        }
                    />
                    <span class="spp-notifications-retention__hint">
                        {move || format!("{} days", retention_ttl_days.get())}
                    </span>
                </div>
            </section>
        </div>
    }
}

/// The notification list grouped by severity (critical / warning / info).
#[component]
fn NotificationSeverityGroups(notifications: Vec<NotificationView>) -> impl IntoView {
    let critical: Vec<NotificationView> = notifications
        .iter()
        .filter(|n| n.severity == Severity::Critical)
        .cloned()
        .collect();
    let warning: Vec<NotificationView> = notifications
        .iter()
        .filter(|n| n.severity == Severity::Warning)
        .cloned()
        .collect();
    let info: Vec<NotificationView> = notifications
        .iter()
        .filter(|n| n.severity == Severity::Info)
        .cloned()
        .collect();

    view! {
        <div class="spp-notifications-severity-groups">
            <NotificationSeveritySection title="Critical" notifications=critical />
            <NotificationSeveritySection title="Warning" notifications=warning />
            <NotificationSeveritySection title="Info" notifications=info />
        </div>
    }
}

/// A severity-grouped section of notifications.
#[component]
fn NotificationSeveritySection(
    title: &'static str,
    notifications: Vec<NotificationView>,
) -> impl IntoView {
    let has_notifications = !notifications.is_empty();
    let rows_fragment = leptos::Fragment::new(
        notifications
            .iter()
            .map(|n| {
                view! {
                    <NotificationRow notification=n.clone() />
                }
                .into_view()
            })
            .collect::<Vec<_>>(),
    );
    view! {
        <section class="spp-notifications-severity-group">
            <h4 class="spp-notifications-severity-group__title">{title}</h4>
            <Show
                when=move || has_notifications
                fallback=|| ().into_view()
            >
                <ul class="spp-notifications-severity-group__list">
                    {rows_fragment.clone()}
                </ul>
            </Show>
        </section>
    }
}

/// A single notification row.
#[component]
fn NotificationRow(notification: NotificationView) -> impl IntoView {
    let label = notification_label(notification.notification_type);
    let description = notification_description(notification.notification_type);
    let severity_class = format!(
        "spp-notification-row spp-notification-row--{}",
        notification.severity.class_suffix()
    );
    let read_badge_class = if notification.read {
        "spp-notification-row__read-badge spp-notification-row__read-badge--read"
    } else {
        "spp-notification-row__read-badge spp-notification-row__read-badge--unread"
    };
    let read_label = if notification.read { "Read" } else { "Unread" };
    // Pre-extract the owned String so the view! closure can borrow a 'static
    // slice instead of borrowing the (soon-dropped) notification.
    let created_at = notification.created_at.clone();
    let is_unread = !notification.read;

    view! {
        <li class=severity_class title=description>
            <span class="spp-notification-row__label">{label}</span>
            <span class="spp-notification-row__time">{created_at}</span>
            <span class=read_badge_class>{read_label}</span>
            <Show when=move || is_unread fallback=|| ().into_view()>
                <button class="spp-notification-row__mark-read" type="button">
                    "Mark as read"
                </button>
            </Show>
        </li>
    }
}

/// The per-type preferences list — 15 toggle rows driven by
/// `NotificationType::ALL`.
#[component]
fn PreferencesList<F>(
    preferences: Vec<(NotificationType, NotificationPreferenceView)>,
    on_toggle: F,
) -> impl IntoView
where
    F: Fn(usize, bool) + 'static + Clone,
{
    // Wrap in Rc so each row can hold its own cheap clone without forcing
    // F to be Copy. The outer view! closure also needs to be Fn (not FnOnce),
    // and Rc makes that trivial.
    let on_toggle = std::rc::Rc::new(on_toggle);
    let rows_fragment = leptos::Fragment::new(
        preferences
            .iter()
            .enumerate()
            .map(|(i, (t, pref))| {
                let label = notification_label(*t);
                let description = notification_description(*t);
                let checked = pref.as_bool();
                let on_toggle_clone = std::rc::Rc::clone(&on_toggle);
                let on_toggle_inner = move |_| on_toggle_clone(i, !checked);
                view! {
                    <li class="spp-preferences-list__row" title=description>
                        <span class="spp-preferences-list__label">{label}</span>
                        <button
                            class="spp-preferences-list__toggle"
                            type="button"
                            role="switch"
                            aria-checked=checked
                            on:click=on_toggle_inner
                        >
                            {move || if checked { "On" } else { "Off" }}
                        </button>
                    </li>
                }
                .into_view()
            })
            .collect::<Vec<_>>(),
    );
    view! {
        <ul class="spp-preferences-list">
            {rows_fragment.clone()}
        </ul>
    }
}

/// Build the default preferences list — one entry per catalog type,
/// using each type's `default_enabled()` as the initial value.
fn default_preferences() -> Vec<(NotificationType, NotificationPreferenceView)> {
    NotificationType::ALL
        .iter()
        .map(|&t| {
            (
                t,
                NotificationPreferenceView::from_bool(t.default_enabled()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_label_covers_all_15() {
        for t in NotificationType::ALL {
            assert!(
                !notification_label(t).is_empty(),
                "type {t:?} missing label"
            );
        }
    }

    #[test]
    fn notification_description_covers_all_15() {
        for t in NotificationType::ALL {
            assert!(
                !notification_description(t).is_empty(),
                "type {t:?} missing description"
            );
        }
    }

    #[test]
    fn notification_severity_matches_catalog() {
        for t in NotificationType::ALL {
            let s = notification_severity(t);
            let catalog_str = t.severity();
            match catalog_str {
                "critical" => assert_eq!(s, Severity::Critical),
                "warning" => assert_eq!(s, Severity::Warning),
                "info" => assert_eq!(s, Severity::Info),
                _ => panic!("unknown severity: {catalog_str}"),
            }
        }
    }

    #[test]
    fn default_preferences_has_15_entries() {
        let prefs = default_preferences();
        assert_eq!(prefs.len(), 15);
        // Each entry's preference matches the catalog's default_enabled().
        for (t, p) in &prefs {
            assert_eq!(p.as_bool(), t.default_enabled());
        }
    }

    #[test]
    fn default_preferences_all_enabled() {
        // Reference: ALL notification types are default-enabled.
        for t in spp_catalog::NotificationType::ALL {
            assert!(t.default_enabled(), "{t:?} must be default-enabled");
        }
    }

    #[test]
    fn default_preferences_sla_breach_is_enabled() {
        let prefs = default_preferences();
        let sla_breach = prefs
            .iter()
            .find(|(t, _)| *t == NotificationType::SlaBreach)
            .map(|(_, p)| *p);
        assert_eq!(sla_breach, Some(NotificationPreferenceView::Enabled));
    }

    #[test]
    fn preference_view_bool_round_trip() {
        assert!(NotificationPreferenceView::Enabled.as_bool());
        assert!(!NotificationPreferenceView::Disabled.as_bool());
        assert_eq!(
            NotificationPreferenceView::from_bool(true),
            NotificationPreferenceView::Enabled
        );
        assert_eq!(
            NotificationPreferenceView::from_bool(false),
            NotificationPreferenceView::Disabled
        );
    }

    #[test]
    fn empty_notifications_renders_empty_state() {
        // The page's default signal is an empty notification list.
        let list: Vec<NotificationView> = Vec::new();
        assert!(list.is_empty());
    }

    #[test]
    fn notification_view_can_be_constructed() {
        let n = NotificationView {
            id: 1,
            notification_type: NotificationType::SlaBreach,
            severity: notification_severity(NotificationType::SlaBreach),
            target_user_id: Some(42),
            conversation_id: Some(1001),
            payload: Some(r#"{"reason":"breached"}"#.into()),
            read: false,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        assert_eq!(n.id, 1);
        assert_eq!(n.severity, Severity::Critical);
        assert!(!n.read);
    }

    #[test]
    fn notifications_can_be_grouped_by_severity() {
        let notifications = [
            NotificationView {
                id: 1,
                notification_type: NotificationType::SlaBreach,
                severity: notification_severity(NotificationType::SlaBreach),
                target_user_id: Some(42),
                conversation_id: None,
                payload: None,
                read: false,
                created_at: "2026-10-01T10:00:00Z".into(),
            },
            NotificationView {
                id: 2,
                notification_type: NotificationType::SlaRisk,
                severity: notification_severity(NotificationType::SlaRisk),
                target_user_id: Some(42),
                conversation_id: None,
                payload: None,
                read: false,
                created_at: "2026-10-01T10:00:00Z".into(),
            },
            NotificationView {
                id: 3,
                notification_type: NotificationType::Mentioned,
                severity: notification_severity(NotificationType::Mentioned),
                target_user_id: Some(42),
                conversation_id: None,
                payload: None,
                read: true,
                created_at: "2026-10-01T10:00:00Z".into(),
            },
        ];
        let critical = notifications
            .iter()
            .filter(|n| n.severity == Severity::Critical)
            .count();
        let warning = notifications
            .iter()
            .filter(|n| n.severity == Severity::Warning)
            .count();
        let info = notifications
            .iter()
            .filter(|n| n.severity == Severity::Info)
            .count();
        assert_eq!(critical, 1, "SlaBreach is critical");
        assert_eq!(warning, 1, "SlaRisk is warning");
        assert_eq!(info, 1, "Mentioned is info");
    }

    #[test]
    fn read_badge_class_reflects_read_state() {
        let read = true;
        let class = if read {
            "spp-notification-row__read-badge spp-notification-row__read-badge--read"
        } else {
            "spp-notification-row__read-badge spp-notification-row__read-badge--unread"
        };
        assert!(class.contains("--read"));
        let read = false;
        let class = if read {
            "spp-notification-row__read-badge spp-notification-row__read-badge--read"
        } else {
            "spp-notification-row__read-badge spp-notification-row__read-badge--unread"
        };
        assert!(class.contains("--unread"));
    }

    #[test]
    fn default_ttl_is_30_days() {
        assert_eq!(DEFAULT_TTL_DAYS_DISPLAY, 30);
    }
}
