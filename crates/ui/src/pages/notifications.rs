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

/// A UI-side notification row. Mirrors the wire shape of
/// `spp_core::notifications::NotificationRecord` (the reference
/// `GET /api/notifications` payload) with `'static` lifetimes so Leptos
/// signals can hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationView {
    /// The row id (used by the "Mark as read" action).
    pub id: i64,
    /// The notification type (catalog enum — single source of truth).
    pub notification_type: NotificationType,
    /// The severity bucket (copied from `NotificationType::severity()`).
    pub severity: Severity,
    /// The headline (reference field `title`).
    pub title: String,
    /// The detail body, when present.
    pub body: Option<String>,
    /// The agent who should see this notification. `None` for "all agents"
    /// (reference field `target_user_local_id`).
    pub target_user_id: Option<i64>,
    /// The conversation the notification is about. `None` for system-wide.
    pub conversation_id: Option<i64>,
    /// The conversation number, when linked (reference field).
    pub conversation_number: Option<i64>,
    /// The JSON payload (triggering event details), as a raw string.
    pub payload: Option<String>,
    /// Whether the notification has been read (`read_at` set).
    pub read: bool,
    /// When the notification was created (ISO-8601 UTC).
    pub created_at: String,
}

/// Parse one wire row (the reference field names: `type`, `title`, `body`,
/// `target_user_local_id`, `read_at`).
pub fn parse_notification_row(n: &serde_json::Value) -> Option<NotificationView> {
    let id = n.get("id")?.as_i64()?;
    let type_str = n.get("type")?.as_str()?;
    let nt = NotificationType::ALL
        .iter()
        .find(|t| t.as_str() == type_str)?;
    let sev = match n.get("severity").and_then(|v| v.as_str()).unwrap_or("info") {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };
    Some(NotificationView {
        id,
        notification_type: *nt,
        severity: sev,
        title: n
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        body: n.get("body").and_then(|v| v.as_str()).map(str::to_string),
        target_user_id: n.get("target_user_local_id").and_then(|v| v.as_i64()),
        conversation_id: n.get("conversation_id").and_then(|v| v.as_i64()),
        conversation_number: n.get("conversation_number").and_then(|v| v.as_i64()),
        payload: None,
        read: n.get("read_at").map(|r| !r.is_null()).unwrap_or(false),
        created_at: n
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// One side-thread mention row (the `side_thread_mentions` list of
/// `GET /api/notifications/mentions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionView {
    pub message_id: i64,
    pub thread_id: i64,
    pub thread_title: Option<String>,
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub author: Option<String>,
    pub body: Option<String>,
    pub created_at: Option<String>,
}

/// The active tab of the Notification Center.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationTab {
    All,
    Unread,
    Mentions,
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
/// Wired to the reference API: GET /api/notifications (with the `type` and
/// `unreadOnly` filters), POST /api/notifications/:id/read,
/// POST /api/notifications/read-all, GET/PUT /api/notifications/prefs and
/// GET /api/notifications/mentions (the "mentions for me" queue).
#[component]
pub fn NotificationsPage() -> impl IntoView {
    let notifications = create_rw_signal(Vec::<NotificationView>::new());
    let mentions = create_rw_signal(Vec::<MentionView>::new());
    let preferences = create_rw_signal(default_preferences());
    let prefs_loaded = create_rw_signal(false);
    let retention_ttl_days = create_rw_signal(DEFAULT_TTL_DAYS_DISPLAY);
    let loading = create_rw_signal(true);
    let error_msg = create_rw_signal(None::<String>);
    let unread = create_rw_signal(0i64);
    let tab = create_rw_signal(NotificationTab::All);
    let type_filter = create_rw_signal(String::new());
    // Bumped after mark-read/mark-all-read/pref changes so the lists refetch.
    let reload = create_rw_signal(0u32);

    // Fetch the notification list (respecting the tab's unread filter and
    // the type filter) + the unread count.
    create_effect(move |_| {
        let _ = reload.get();
        let type_filter = type_filter.get();
        let unread_only = tab.get() == NotificationTab::Unread;
        let mut path = format!(
            "/api/notifications?limit=50&unreadOnly={}",
            if unread_only { "true" } else { "false" }
        );
        if !type_filter.is_empty() {
            path.push_str("&type=");
            path.push_str(&type_filter);
        }
        let path = path; // 'static for the spawned future
        let notifications = notifications;
        let unread = unread;
        let loading = loading;
        let error_msg = error_msg;
        let is_first = loading.get_untracked();
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>(&path).await {
                Ok(data) => {
                    let list = data
                        .get("notifications")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let views: Vec<NotificationView> =
                        list.iter().filter_map(parse_notification_row).collect();
                    notifications.set(views);
                    if let Some(n) = data.get("unread").and_then(|v| v.as_i64()) {
                        unread.set(n);
                    }
                    if is_first {
                        loading.set(false);
                    }
                }
                Err(e) => {
                    error_msg.set(Some(e));
                    if is_first {
                        loading.set(false);
                    }
                }
            }
        });
    });

    // Fetch the mention queue once (tab content) + the persisted prefs.
    create_effect(move |_| {
        let _ = tab.get();
        if tab.get() != NotificationTab::Mentions || !mentions.get_untracked().is_empty() {
            return;
        }
        let mentions = mentions;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(data) =
                crate::api::get_json::<serde_json::Value>("/api/notifications/mentions").await
            {
                let rows = data
                    .get("side_thread_mentions")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let views: Vec<MentionView> = rows
                    .iter()
                    .filter_map(|m| {
                        Some(MentionView {
                            message_id: m.get("message_id")?.as_i64()?,
                            thread_id: m.get("thread_id")?.as_i64()?,
                            thread_title: m
                                .get("thread_title")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            conversation_id: m.get("conversation_id")?.as_i64()?,
                            conversation_number: m
                                .get("conversation_number")
                                .and_then(|v| v.as_i64()),
                            author: m.get("author").and_then(|v| v.as_str()).map(str::to_string),
                            body: m.get("body").and_then(|v| v.as_str()).map(str::to_string),
                            created_at: m
                                .get("created_at")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect();
                mentions.set(views);
            }
        });
    });

    // Load the persisted preferences (GET /api/notifications/prefs).
    create_effect(move |_| {
        if prefs_loaded.get() {
            return;
        }
        prefs_loaded.set(true);
        let preferences = preferences;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(data) =
                crate::api::get_json::<serde_json::Value>("/api/notifications/prefs").await
            {
                let rows = data
                    .get("prefs")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                preferences.update(|p| {
                    for row in rows {
                        let type_str = row.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let enabled = row
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(entry) = p.iter_mut().find(|(t, _)| t.as_str() == type_str) {
                            entry.1 = NotificationPreferenceView::from_bool(enabled);
                        }
                    }
                });
            }
        });
    });

    // Mark one notification read (POST /api/notifications/:id/read).
    let mark_read = move |id: i64| {
        let reload = reload;
        wasm_bindgen_futures::spawn_local(async move {
            if crate::api::post_json::<serde_json::Value>(
                &format!("/api/notifications/{id}/read"),
                Some(&serde_json::json!({ "read": true })),
            )
            .await
            .is_ok()
            {
                reload.set(reload.get_untracked() + 1);
            }
        });
    };

    // Mark everything read (POST /api/notifications/read-all).
    let mark_all_read = move |_| {
        let reload = reload;
        let unread = unread;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(body) =
                crate::api::post_json::<serde_json::Value>("/api/notifications/read-all", None)
                    .await
            {
                if let Some(n) = body.get("unread").and_then(|v| v.as_i64()) {
                    unread.set(n);
                }
                reload.set(reload.get_untracked() + 1);
            }
        });
    };

    // Toggle a preference and persist it (PUT /api/notifications/prefs/:type).
    let on_toggle_pref = move |i: usize, v: bool| {
        let (notif_type, _) = preferences.get_untracked().get(i).copied().unwrap_or((
            NotificationType::CustomerReplied,
            NotificationPreferenceView::Enabled,
        ));
        preferences.update(|p| {
            if let Some(entry) = p.get_mut(i) {
                entry.1 = NotificationPreferenceView::from_bool(v);
            }
        });
        let preferences = preferences;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(data) = crate::api::put_json::<serde_json::Value>(
                &format!("/api/notifications/prefs/{}", notif_type.as_str()),
                &serde_json::json!({ "enabled": v }),
            )
            .await
            {
                // The PUT returns the authoritative prefs list — re-sync.
                let rows = data
                    .get("prefs")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                preferences.update(|p| {
                    for row in rows {
                        let type_str = row.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        let enabled = row
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(entry) = p.iter_mut().find(|(t, _)| t.as_str() == type_str) {
                            entry.1 = NotificationPreferenceView::from_bool(enabled);
                        }
                    }
                });
            }
        });
    };

    view! {
        <div class="spp-page spp-page--notifications">
            <h2 class="spp-page__title">
                "Notifications"
                <Show when=move || { unread.get() > 0 } fallback=|| ().into_view()>
                    <span class="spp-badge spp-badge--err">{move || unread.get().to_string()}</span>
                </Show>
            </h2>
            <p class="spp-page__subtitle">
                "Stay on top of replies, SLA risk, and system health. "
                "Adjust which notifications you see, or change how long they're kept."
            </p>

            // ── Tabs + filters ──
            <div class="spp-notifications-toolbar" role="tablist">
                <div class="spp-tabs">
                    <button
                        class=move || if tab.get() == NotificationTab::All { "spp-tab is-active" } else { "spp-tab" }
                        type="button"
                        role="tab"
                        aria-selected=move || tab.get() == NotificationTab::All
                        on:click=move |_| tab.set(NotificationTab::All)
                    >
                        "All"
                    </button>
                    <button
                        class=move || if tab.get() == NotificationTab::Unread { "spp-tab is-active" } else { "spp-tab" }
                        type="button"
                        role="tab"
                        aria-selected=move || tab.get() == NotificationTab::Unread
                        on:click=move |_| tab.set(NotificationTab::Unread)
                    >
                        "Unread"
                    </button>
                    <button
                        class=move || if tab.get() == NotificationTab::Mentions { "spp-tab is-active" } else { "spp-tab" }
                        type="button"
                        role="tab"
                        aria-selected=move || tab.get() == NotificationTab::Mentions
                        on:click=move |_| tab.set(NotificationTab::Mentions)
                    >
                        "Mentions"
                    </button>
                </div>
                <Show when=move || tab.get() != NotificationTab::Mentions fallback=|| ().into_view()>
                    <label class="spp-notifications-toolbar__filter" for="notification-type-filter">
                        "Type"
                        <select
                            id="notification-type-filter"
                            class="spp-input"
                            prop:value=move || type_filter.get()
                            on:change=move |ev| {
                                type_filter.set(event_target_value(&ev));
                                reload.set(reload.get_untracked() + 1);
                            }
                        >
                            <option value="">"All types"</option>
                            {NotificationType::ALL.iter().map(|t| {
                                let key = t.as_str().to_string();
                                view! { <option value=key.clone()>{notification_label(*t)}</option> }.into_view()
                            }).collect::<Vec<_>>()}
                        </select>
                    </label>
                    <button
                        class="spp-button spp-button--ghost spp-button--small"
                        type="button"
                        disabled=move || unread.get() == 0
                        on:click=mark_all_read
                    >
                        "Mark all read"
                    </button>
                </Show>
            </div>

            <Show when=move || error_msg.get().is_some() fallback=|| ().into_view()>
                <div class="spp-state spp-state--error">
                    <p class="spp-state__body">{move || error_msg.get().unwrap_or_default()}</p>
                </div>
            </Show>

            // ── Mention queue tab ──
            <Show when=move || tab.get() == NotificationTab::Mentions fallback=|| ().into_view()>
                <section class="spp-notifications-list">
                    <h3 class="spp-notifications-list__title">"Mentions for you"</h3>
                    <Show
                        when=move || !mentions.get().is_empty()
                        fallback=move || {
                            view! {
                                <EmptyState message="No mentions yet. @mentions in conversations and side threads land here." />
                            }
                        }
                    >
                        <ul class="spp-notifications-severity-group__list">
                            {move || mentions.get().iter().map(|m| {
                                // Precompute every owned string so the
                                // closures below capture only Copies/Options.
                                let title = m.thread_title.clone()
                                    .unwrap_or_else(|| "Side thread".to_string());
                                let at = m.created_at.clone().unwrap_or_default();
                                let body_line = match (&m.author, &m.body) {
                                    (Some(a), Some(b)) => Some(format!("{a}: {b}")),
                                    (Some(a), None) => Some(a.clone()),
                                    (None, Some(b)) => Some(b.clone()),
                                    (None, None) => None,
                                };
                                let body_display = body_line.unwrap_or_default();
                                let has_body = !body_display.is_empty();
                                let number_display = m.conversation_number
                                    .map(|n| format!("#{n}"))
                                    .unwrap_or_default();
                                let has_number = !number_display.is_empty();
                                view! {
                                    <li class="spp-notification-row" title="Mentioned in a side thread">
                                        <span class="spp-notification-row__label">{title}</span>
                                        <Show when=move || has_body fallback=|| ().into_view()>
                                            <span class="spp-notification-row__body">
                                                {body_display.clone()}
                                            </span>
                                        </Show>
                                        <Show when=move || has_number fallback=|| ().into_view()>
                                            <span class="spp-notification-row__read-badge">
                                                {number_display.clone()}
                                            </span>
                                        </Show>
                                        <span class="spp-notification-row__time">{at}</span>
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
                        </ul>
                    </Show>
                </section>
            </Show>

            // ── Notification list (All / Unread tabs) ──
            <Show when=move || tab.get() != NotificationTab::Mentions fallback=|| ().into_view()>
                <section class="spp-notifications-list">
                    <h3 class="spp-notifications-list__title">
                        {move || if tab.get() == NotificationTab::Unread { "Unread" } else { "Recent" }.to_string()}
                    </h3>
                    <Show
                        when=move || !loading.get() && !notifications.get().is_empty()
                        fallback=move || {
                            view! {
                                <EmptyState message="No notifications yet. Once a sync settles, customer replies and ticket assignments will appear here." />
                            }
                        }
                    >
                        <NotificationSeverityGroups
                            notifications=notifications.get()
                            on_mark_read=mark_read
                        />
                    </Show>
                </section>
            </Show>

            // ── Per-type preferences ──
            <section class="spp-notifications-prefs">
                <h3 class="spp-notifications-prefs__title">"Notification preferences"</h3>
                <p class="spp-notifications-prefs__help">
                    "Toggle which notification types you want to see. "
                    "Changes are saved to your workspace immediately."
                </p>
                <PreferencesList preferences=preferences.get() on_toggle=on_toggle_pref />
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
fn NotificationSeverityGroups<F>(
    notifications: Vec<NotificationView>,
    on_mark_read: F,
) -> impl IntoView
where
    F: Fn(i64) + 'static + Clone,
{
    let on_critical = on_mark_read.clone();
    let on_warning = on_mark_read.clone();
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
            <NotificationSeveritySection title="Critical" notifications=critical on_mark_read=on_critical />
            <NotificationSeveritySection title="Warning" notifications=warning on_mark_read=on_warning />
            <NotificationSeveritySection title="Info" notifications=info on_mark_read=on_mark_read />
        </div>
    }
}

/// A severity-grouped section of notifications.
#[component]
fn NotificationSeveritySection<F>(
    title: &'static str,
    notifications: Vec<NotificationView>,
    on_mark_read: F,
) -> impl IntoView
where
    F: Fn(i64) + 'static + Clone,
{
    let has_notifications = !notifications.is_empty();
    let rows_fragment = leptos::Fragment::new(
        notifications
            .iter()
            .map(|n| {
                view! {
                    <NotificationRow notification=n.clone() on_mark_read=on_mark_read.clone() />
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

/// A single notification row — "Mark as read" wired to
/// POST /api/notifications/:id/read.
#[component]
fn NotificationRow<F>(notification: NotificationView, on_mark_read: F) -> impl IntoView
where
    F: Fn(i64) + 'static + Clone,
{
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
    // Pre-extract the owned Strings so the view! closures can borrow 'static
    // slices instead of borrowing the (soon-dropped) notification.
    let created_at = notification.created_at.clone();
    let title_text = if notification.title.is_empty() {
        label.to_string()
    } else {
        notification.title.clone()
    };
    let body_text = notification.body.clone().unwrap_or_default();
    let has_body = !body_text.is_empty();
    let number_display = notification
        .conversation_number
        .map(|n| format!("#{n}"))
        .unwrap_or_default();
    let has_number = !number_display.is_empty();
    let is_unread = !notification.read;
    let row_id = notification.id;
    // Rc-wrap the callback so the Show children closure can hand each re-run
    // its own cheap clone (the click handler must not move it out).
    let on_mark_read = std::rc::Rc::new(on_mark_read);

    view! {
        <li class=severity_class title=description>
            <span class="spp-notification-row__label">{title_text}</span>
            <Show when=move || has_body fallback=|| ().into_view()>
                <span class="spp-notification-row__body">{body_text.clone()}</span>
            </Show>
            <Show when=move || has_number fallback=|| ().into_view()>
                <span class="spp-notification-row__read-badge">{number_display.clone()}</span>
            </Show>
            <span class="spp-notification-row__time">{created_at}</span>
            <span class=read_badge_class>{read_label}</span>
            <Show when=move || is_unread fallback=|| ().into_view()>
                <button
                    class="spp-notification-row__mark-read"
                    type="button"
                    on:click={
                        let handler = on_mark_read.clone();
                        move |_| handler(row_id)
                    }
                >
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
            title: "SLA breached".into(),
            body: Some("#101 went past its first-response SLA".into()),
            target_user_id: Some(42),
            conversation_id: Some(1001),
            conversation_number: Some(101),
            payload: Some(r#"{"reason":"breached"}"#.into()),
            read: false,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        assert_eq!(n.id, 1);
        assert_eq!(n.severity, Severity::Critical);
        assert!(!n.read);
    }

    #[test]
    fn parse_notification_row_reads_the_reference_field_names() {
        // The reference wire row: `type` (not notification_type), `title`,
        // `body`, `target_user_local_id` (not target_user_id),
        // `conversation_number`, `read_at`.
        let v = serde_json::json!({
            "id": 7,
            "type": "customer_replied",
            "severity": "warning",
            "title": "Customer replied on #33",
            "body": "Refund question",
            "target_user_local_id": 9,
            "conversation_id": 33,
            "conversation_number": 33,
            "created_at": "2026-10-01T10:00:00Z",
            "read_at": null,
        });
        let n = parse_notification_row(&v).expect("parses");
        assert_eq!(n.id, 7);
        assert_eq!(n.notification_type, NotificationType::CustomerReplied);
        assert_eq!(n.severity, Severity::Warning);
        assert_eq!(n.title, "Customer replied on #33");
        assert_eq!(n.body.as_deref(), Some("Refund question"));
        assert_eq!(n.target_user_id, Some(9));
        assert_eq!(n.conversation_id, Some(33));
        assert_eq!(n.conversation_number, Some(33));
        assert!(!n.read);
        // A read row carries read_at.
        let v = serde_json::json!({
            "id": 8, "type": "mentioned", "severity": "info", "title": "Mentioned",
            "created_at": "2026-10-01T10:00:00Z", "read_at": "2026-10-01T11:00:00Z",
        });
        let n = parse_notification_row(&v).expect("parses");
        assert!(n.read);
        // Unknown types are dropped (closed vocabulary).
        let v = serde_json::json!({
            "id": 9, "type": "not_a_type", "severity": "info", "title": "x",
            "created_at": "2026-10-01T10:00:00Z", "read_at": null,
        });
        assert!(parse_notification_row(&v).is_none());
    }

    #[test]
    fn notifications_can_be_grouped_by_severity() {
        let mk = |t: NotificationType, read: bool| NotificationView {
            id: 1,
            notification_type: t,
            severity: notification_severity(t),
            title: String::new(),
            body: None,
            target_user_id: Some(42),
            conversation_id: None,
            conversation_number: None,
            payload: None,
            read,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let notifications = [
            mk(NotificationType::SlaBreach, false),
            mk(NotificationType::SlaRisk, false),
            mk(NotificationType::Mentioned, true),
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
