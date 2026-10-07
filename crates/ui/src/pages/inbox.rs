//! Inbox page — the 3-pane layout (M12-P5 — full inbox wiring).
//!
//! Per spec M3: "inbox." Per A11: visual reference is the reference repo's
//! `inbox.png` screenshot — 3-pane layout (list + detail + context).
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."
//!
//! Every control calls a real IPC command:
//! - List pane calls `inbox_list_conversations` on mount + on filter change.
//! - Filter dropdowns: status, mailbox (placeholder for now), assignee, priority.
//! - Saved view selector calls `inbox_list_saved_views`.
//! - Conversation click calls `inbox_get_conversation`.
//! - Reply composer calls `inbox_reply`.
//! - Note composer calls `inbox_add_note`.
//! - Status dropdown calls `inbox_change_status`.
//! - Assignee picker calls `inbox_assign`.
//! - Bulk select + apply calls `inbox_change_status` for each.

use leptos::*;
use leptos_router::{use_location, use_navigate, use_query_map};

use crate::components::attribute_snapshot::AttributeSnapshotCard;
use crate::components::coaching_panel::CoachingPanel;
use crate::components::copilot_panel::CopilotPanel;
use crate::components::memory_panel::MemoryPanel;
use crate::components::mention_textarea::MentionTextarea;
use crate::components::overlays::ConfirmDialog;
use crate::components::qa_panel::QaPanel;
use crate::components::safe_html::SafeHtml;
use crate::components::side_threads::SideThreadsPanel;
use crate::components::state_view::{EmptyState, LoadingState};
use crate::components::translation_panel::TranslationPanel;
use crate::toasts;

/// Percent-encode a query value (the query-string subset that needs it).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The conversation list item — matches the Rust `ConversationListItem`.
#[derive(Debug, Clone, Default)]
pub struct ConversationListItem {
    pub id: i64,
    pub remote_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub mailbox_name: Option<String>,
    pub assignee_id: Option<i64>,
    pub assignee_name: Option<String>,
    pub customer_id: i64,
    pub customer_name: Option<String>,
    pub priority: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub response_state: Option<String>,
}

/// A thread entry — matches the Rust `ThreadEntry`.
#[derive(Debug, Clone, Default)]
pub struct ThreadEntry {
    pub id: i64,
    pub conversation_id: i64,
    pub thread_type: String,
    pub body: Option<String>,
    pub actor_type: String,
    pub actor_id: Option<i64>,
    pub actor_name: Option<String>,
    pub created_at: String,
}

/// A conversation detail — matches the Rust `ConversationDetail`.
#[derive(Debug, Clone, Default)]
pub struct ConversationDetail {
    pub id: i64,
    pub remote_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub mailbox_name: Option<String>,
    pub assignee_id: Option<i64>,
    pub assignee_name: Option<String>,
    pub customer_id: i64,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub priority: Option<String>,
    pub response_state: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub thread: Vec<ThreadEntry>,
    pub tags: Vec<String>,
}

/// Saved view — matches the Rust `SavedView`.
#[derive(Debug, Clone, Default)]
pub struct SavedView {
    pub id: Option<i64>,
    pub name: String,
    pub mailbox_id: Option<i64>,
}

/// The filters selected by the user.
#[derive(Debug, Clone, Default)]
pub struct InboxFilters {
    pub status: Option<String>,
    pub mailbox_id: Option<i64>,
    pub priority: Option<String>,
    pub query: Option<String>,
    /// URL-backed deep-link filters (UI-27): `?tag=` + `?channel=` land
    /// here from the dashboard / search links (reference Inbox.tsx:46-66).
    pub tag: Option<String>,
    pub channel: Option<String>,
}

/// The composer mode — reply or note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerMode {
    Reply,
    Note,
}

/// The context-pane tab (reference ContextPane: 'ai' | 'customer' | 'copilot').
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextTab {
    Ai,
    Customer,
    Copilot,
}

/// One similar-conversation row (reference AiSidebar / /api/ai/similar/:id).
#[derive(Debug, Clone, Default)]
pub struct SimilarConversation {
    pub conversation_id: i64,
    pub number: i64,
    pub subject: String,
    pub resolution: String,
    pub score: f64,
    pub why: Vec<String>,
}

/// Parse one row of the /api/ai/similar/:id `similar` array.
#[must_use]
fn parse_similar_conversation(v: &serde_json::Value) -> SimilarConversation {
    SimilarConversation {
        conversation_id: v
            .get("conversation_id")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        number: v.get("number").and_then(|x| x.as_i64()).unwrap_or(0),
        subject: v
            .get("subject")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        resolution: v
            .get("resolution")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        score: v.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0),
        why: v
            .get("why")
            .and_then(|w| w.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|w| w.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The inbox page — 3-pane layout.
///
/// `conversation_id` is set on the `/inbox/conversation/:id` route (the
/// reference's `useParams` id drives the initial selection; the reference
/// keeps both routes on the same page component).
#[component]
pub fn InboxPage(#[prop(optional, into)] conversation_id: Option<i64>) -> impl IntoView {
    let conversations = create_rw_signal(Vec::<ConversationListItem>::new());
    let total = create_rw_signal(0u32);
    let filters = create_rw_signal(InboxFilters::default());
    let selected_id = create_rw_signal(conversation_id);
    let detail = create_rw_signal(None::<ConversationDetail>);
    let detail_loading = create_rw_signal(false);
    let detail_error = create_rw_signal(None::<String>);
    let list_loading = create_rw_signal(true);
    let list_error = create_rw_signal(None::<String>);
    let saved_views = create_rw_signal(Vec::<SavedView>::new());
    let composer_mode = create_rw_signal(ComposerMode::Reply);
    let composer_body = create_rw_signal(String::new());
    let composer_error = create_rw_signal(None::<String>);
    let selected_ids = create_rw_signal(Vec::<i64>::new());
    // v1.9.0+ parity: context pane tabs + send confirmation + AI sidebar state.
    let context_tab = create_rw_signal(ContextTab::Ai);
    let context_open = create_rw_signal(true);
    let confirm_send = create_rw_signal(false);
    let ai_analysis = create_rw_signal(None::<serde_json::Value>);
    let ai_analyzing = create_rw_signal(false);
    let similar = create_rw_signal(Vec::<SimilarConversation>::new());

    // ── Cross-page invalidation (UI-26) ─────────────────────────────────
    // The app-level SSE bridge (lib.rs) owns the ONE subscription; it bumps
    // the shared version counters the reference invalidates. The inbox list
    // watches the 'conversations' counter — the same refetch the reference
    // gets from invalidating ['conversations'] on conversation/sync events
    // (previously this page held its own subscription; the bridge now does
    // the fan-out for every page at once).
    let sse_refresh = crate::queries::version("conversations");

    // ── URL-backed state (UI-27) ──────────────────────────────────────
    // `?view=`, `?tag=`, `?channel=` (+ `?page=` reads) are deep-link
    // targets (dashboard KPI/tag links, channel chips, search hits). The
    // query map seeds the filters on mount and re-syncs them when the URL
    // changes (browser back / a link into the page), like the reference's
    // useSearchParams-driven state. `q`/`priority` stay local signals — the
    // reference keeps them out of the URL too.
    let query_map = use_query_map();
    let location = use_location();
    let navigate = use_navigate();
    create_effect(move |_| {
        let m = query_map.get();
        let url_status = crate::url_state::query_str(&m, "view");
        let url_tag = crate::url_state::query_str(&m, "tag");
        let url_channel = crate::url_state::query_str(&m, "channel");
        // Only touch the URL-managed fields; a redundant set would
        // re-run the list effect for nothing.
        let mut changed = false;
        filters.update(|f| {
            if f.status != url_status {
                f.status = url_status;
                changed = true;
            }
            if f.tag != url_tag {
                f.tag = url_tag;
                changed = true;
            }
            if f.channel != url_channel {
                f.channel = url_channel;
                changed = true;
            }
        });
        if changed {
            list_loading.set(true);
        }
    });
    // Write the URL-managed filters back to the query string (replace,
    // like the reference's setSearchParams(..., { replace: true })).
    // StoredValue so the Fn view closure and every chip handler can call it
    // without moving it out (the close-handler pattern).
    let sync_url = StoredValue::new({
        let navigate = navigate.clone();
        let pathname = location.pathname;
        std::rc::Rc::new(move || {
            let f = filters.get_untracked();
            crate::url_state::replace_query(
                &navigate,
                &pathname.get_untracked(),
                &[
                    ("view", f.status.clone()),
                    ("tag", f.tag.clone()),
                    ("channel", f.channel.clone()),
                ],
            );
        }) as std::rc::Rc<dyn Fn()>
    });

    // ── Load conversation list on mount + on filter change + on SSE refresh ─
    create_effect(move |_| {
        let conversations = conversations;
        let total = total;
        let list_loading = list_loading;
        let list_error = list_error;
        let current_filters = filters.get();
        // Read sse_refresh so the effect re-runs when it changes.
        let _ = sse_refresh.get();
        wasm_bindgen_futures::spawn_local(async move {
            // GET /api/conversations — the reference list endpoint. The view
            // param carries the status filter; q the text filter; tag/channel
            // the URL-backed deep-link filters (UI-27).
            let mut path = String::from("/api/conversations?pageSize=50");
            if let Some(status) = current_filters.status {
                path.push_str(&format!("&view={status}"));
            }
            if let Some(priority) = current_filters.priority {
                path.push_str(&format!("&priority={priority}"));
            }
            if let Some(q) = current_filters.query {
                path.push_str(&format!("&q={}", urlencode(&q)));
            }
            if let Some(tag) = current_filters.tag {
                path.push_str(&format!("&tag={}", urlencode(&tag)));
            }
            if let Some(channel) = current_filters.channel {
                path.push_str(&format!("&channel={channel}"));
            }
            match crate::api::get_json::<serde_json::Value>(&path).await {
                Ok(data) => {
                    let items = data
                        .get("conversations")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .map(parse_conversation_list_item)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let total_val = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    conversations.set(items);
                    total.set(total_val);
                    list_loading.set(false);
                    list_error.set(None);
                }
                Err(e) => {
                    list_error.set(Some(e));
                    list_loading.set(false);
                }
            }
        });
    });

    // ── Load saved views on mount ───────────────────────────────────────
    create_effect(move |_| {
        let saved_views = saved_views;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::get_json::<serde_json::Value>("/api/inbox-views").await {
                Ok(payload) => {
                    let arr = payload
                        .get("views")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let views: Vec<SavedView> = arr
                        .iter()
                        .map(|v| SavedView {
                            id: v.get("id").and_then(|x| x.as_i64()),
                            name: v
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            mailbox_id: v.get("mailbox_id").and_then(|x| x.as_i64()),
                        })
                        .collect();
                    saved_views.set(views);
                }
                Err(_) => {
                    // Saved views are optional — silently ignore.
                }
            }
        });
    });

    // ── Load detail when a conversation is selected ────────────────────
    create_effect(move |_| {
        let id = selected_id.get();
        let detail = detail;
        let detail_loading = detail_loading;
        let detail_error = detail_error;
        if let Some(id) = id {
            detail_loading.set(true);
            detail_error.set(None);
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/conversations/{id}");
                match crate::api::get_json::<serde_json::Value>(&path).await {
                    Ok(data) => {
                        detail.set(Some(parse_conversation_detail(&data)));
                        detail_loading.set(false);
                    }
                    Err(e) => {
                        // A 404 from the API means the conversation is gone.
                        let msg = if e.contains("404") {
                            "Conversation not found".to_string()
                        } else {
                            e
                        };
                        detail_error.set(Some(msg));
                        detail.set(None);
                        detail_loading.set(false);
                    }
                }
            });
        } else {
            detail.set(None);
        }
    });

    // ── Similar conversations + AI analysis reset on selection change ──
    create_effect(move |_| {
        let id = selected_id.get();
        ai_analysis.set(None);
        similar.set(Vec::new());
        if let Some(id) = id {
            let similar = similar;
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(r) =
                    crate::api::get_json::<serde_json::Value>(&format!("/api/ai/similar/{id}"))
                        .await
                {
                    let rows = r
                        .get("similar")
                        .and_then(|s| s.as_array())
                        .map(|arr| arr.iter().map(parse_similar_conversation).collect())
                        .unwrap_or_default();
                    similar.set(rows);
                }
            });
        }
    });

    // ── Run the AI analysis on demand (reference AiSidebar analyze) ─────
    let run_analyze = move || {
        let Some(id) = selected_id.get_untracked() else {
            return;
        };
        if ai_analyzing.get_untracked() {
            return;
        }
        ai_analyzing.set(true);
        let body = serde_json::json!({ "force": true });
        let ai_analysis = ai_analysis;
        let ai_analyzing = ai_analyzing;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::post_json::<serde_json::Value>(
                &format!("/api/ai/analyze/{id}"),
                Some(&body),
            )
            .await
            {
                Ok(r) => {
                    if r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                        toasts::success("AI analysis completed.");
                        if let Some(a) = r.get("analysis") {
                            ai_analysis.set(Some(a.clone()));
                        }
                    } else {
                        let reason = r
                            .get("error")
                            .and_then(|x| x.as_str())
                            .unwrap_or("Analysis failed");
                        toasts::error(reason);
                    }
                }
                Err(e) => toasts::error(e),
            }
            ai_analyzing.set(false);
        });
    };

    // ── Submit composer ────────────────────────────────────────────────
    let send_composer = move || {
        let body = composer_body.get();
        if body.trim().is_empty() {
            composer_error.set(Some("Body cannot be empty".to_string()));
            return;
        }
        let mode = composer_mode.get();
        // Mutations address the conversation by its LOCAL id (reference
        // Inbox.tsx uses conversation.id for every write call).
        let conv_local_id = detail.with(|d| d.as_ref().map(|d| d.id));
        let conv_local_id = match conv_local_id {
            Some(id) => id,
            None => {
                composer_error.set(Some("No conversation selected".to_string()));
                return;
            }
        };

        let sub_path = match mode {
            ComposerMode::Reply => "reply",
            ComposerMode::Note => "note",
        };
        // Reference composer onError: the toast tells the user their typed
        // text survives the failure (spec #20 — state survives async failures).
        let is_reply = mode == ComposerMode::Reply;
        let path = format!("/api/conversations/{conv_local_id}/{sub_path}");
        // replyRequestSchema / noteRequestSchema field name: `text`.
        let body_payload = serde_json::json!({ "text": body });
        let composer_error = composer_error;
        let composer_body = composer_body;
        let selected_id = selected_id;
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::post_json::<serde_json::Value>(&path, Some(&body_payload)).await {
                Ok(result) => {
                    let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    let message = result
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or(if ok { "Sent" } else { "Operation rejected" })
                        .to_string();
                    let detail = result
                        .get("detail")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    if ok {
                        composer_error.set(None);
                        composer_body.set(String::new());
                        // Refresh the detail by re-selecting it.
                        if let Some(id) = selected_id.get() {
                            selected_id.set(None);
                            selected_id.set(Some(id));
                        }
                    }
                    let kind = if ok {
                        crate::toasts::ToastKind::Success
                    } else {
                        crate::toasts::ToastKind::Error
                    };
                    crate::toasts::push(kind, message, detail);
                }
                Err(e) => {
                    let prefix = if is_reply {
                        "Your text is preserved in the composer. "
                    } else {
                        "Your text is preserved. "
                    };
                    toasts::error(format!("{prefix}{e}"));
                }
            }
        });
    };

    // Reply sends are customer-visible: confirm first (reference ConfirmDialog).
    let request_submit = move || {
        if composer_mode.get() == ComposerMode::Reply {
            confirm_send.set(true);
        } else {
            send_composer();
        }
    };

    // ── Change status ──────────────────────────────────────────────────
    let change_status = move |new_status: String| {
        let conv_local_id = detail.with(|d| d.as_ref().map(|d| d.id));
        if let Some(conv_local_id) = conv_local_id {
            let selected_id = selected_id;
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/conversations/{conv_local_id}/status");
                let payload = serde_json::json!({ "status": new_status });
                match crate::api::post_json::<serde_json::Value>(&path, Some(&payload)).await {
                    Ok(result) => {
                        let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = result
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(if ok {
                                "Status updated."
                            } else {
                                "Status change rejected"
                            })
                            .to_string();
                        if ok {
                            // Refresh detail.
                            if let Some(id) = selected_id.get() {
                                selected_id.set(None);
                                selected_id.set(Some(id));
                            }
                        }
                        let kind = if ok {
                            crate::toasts::ToastKind::Success
                        } else {
                            crate::toasts::ToastKind::Error
                        };
                        crate::toasts::push(kind, message, None);
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    // ── Assign to user (placeholder — uses user_id=1 for now) ───────────
    let assign_to = move |assignee_id: Option<i64>| {
        let conv_local_id = detail.with(|d| d.as_ref().map(|d| d.id));
        if let Some(conv_local_id) = conv_local_id {
            let selected_id = selected_id;
            wasm_bindgen_futures::spawn_local(async move {
                let path = format!("/api/conversations/{conv_local_id}/assign");
                // assignRequestSchema: `userId` is the REMOTE user id
                // (nullable to unassign).
                let payload = serde_json::json!({ "userId": assignee_id });
                match crate::api::post_json::<serde_json::Value>(&path, Some(&payload)).await {
                    Ok(result) => {
                        let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                        let message = result
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(if ok {
                                "Assignee updated."
                            } else {
                                "Assignment rejected"
                            })
                            .to_string();
                        if ok {
                            if let Some(id) = selected_id.get() {
                                selected_id.set(None);
                                selected_id.set(Some(id));
                            }
                        }
                        let kind = if ok {
                            crate::toasts::ToastKind::Success
                        } else {
                            crate::toasts::ToastKind::Error
                        };
                        crate::toasts::push(kind, message, None);
                    }
                    Err(e) => toasts::error(e),
                }
            });
        }
    };

    view! {
        <div class="spp-inbox">
            // ── List pane (left) ──
            <aside class="spp-inbox__list">
                <div class="spp-inbox__list-header">
                    <h2 class="spp-inbox__title">"Inbox"</h2>
                    <span class="spp-inbox__count">
                        {move || format!("{} conversations", total.get())}
                    </span>
                </div>

                // Filters
                <div class="spp-inbox__filters">
                    <select
                        class="spp-inbox__filter"
                        prop:value=move || filters.with(|f| f.status.clone().unwrap_or_default())
                        on:change=move |ev| {
                            let val = event_target_value(&ev);
                            let val = if val.is_empty() { None } else { Some(val) };
                            filters.update(|f| f.status = val);
                            sync_url.with_value(|f| f());
                            list_loading.set(true);
                        }
                    >
                        <option value="">"All statuses"</option>
                        <option value="active">"Active"</option>
                        <option value="pending">"Pending"</option>
                        <option value="closed">"Closed"</option>
                    </select>
                    <select
                        class="spp-inbox__filter"
                        on:change=move |ev| {
                            let val = event_target_value(&ev);
                            let val = if val.is_empty() { None } else { Some(val) };
                            filters.update(|f| f.priority = val);
                            list_loading.set(true);
                        }
                    >
                        <option value="">"All priorities"</option>
                        <option value="low">"Low"</option>
                        <option value="normal">"Normal"</option>
                        <option value="high">"High"</option>
                        <option value="urgent">"Urgent"</option>
                    </select>
                    <input
                        class="spp-inbox__search"
                        type="text"
                        placeholder="Search subject + preview..."
                        on:input=move |ev| {
                            let val = event_target_value(&ev);
                            let val = if val.is_empty() { None } else { Some(val) };
                            filters.update(|f| f.query = val);
                            list_loading.set(true);
                        }
                    />
                </div>

                // Deep-link filter chips (UI-27): tag/channel arrive via
                // ?tag=/?channel= — show them so the filtered list is
                // explainable and clearable (the reference exposes them in
                // the FilterBar; that bar is UI-02 scope).
                <Show when=move || filters.with(|f| f.tag.is_some() || f.channel.is_some()) fallback=|| ()>
                    <div class="spp-inbox__url-chips">
                        {move || {
                            let f = filters.get();
                            let mut chips: Vec<leptos::View> = Vec::new();
                            if let Some(channel) = f.channel {
                                let clear = {
                                    let filters = filters;
                                    let sync_url = sync_url;
                                    move |_| {
                                        filters.update(|f| f.channel = None);
                                        sync_url.with_value(|f| f());
                                        list_loading.set(true);
                                    }
                                };
                                let label = if channel == "chat" {
                                    "channel: chat (Beacon)".to_string()
                                } else {
                                    format!("channel: {channel}")
                                };
                                chips.push(
                                    view! {
                                        <span class="spp-chip">
                                            <span class="spp-chip__label">{label}</span>
                                            <button class="spp-chip__clear" aria-label="Clear channel filter" on:click=clear>"\u{d7}"</button>
                                        </span>
                                    }.into_view(),
                                );
                            }
                            if let Some(tag) = f.tag {
                                let clear = {
                                    let filters = filters;
                                    let sync_url = sync_url;
                                    move |_| {
                                        filters.update(|f| f.tag = None);
                                        sync_url.with_value(|f| f());
                                        list_loading.set(true);
                                    }
                                };
                                chips.push(
                                    view! {
                                        <span class="spp-chip">
                                            <span class="spp-chip__label">{format!("tag: {tag}")}</span>
                                            <button class="spp-chip__clear" aria-label="Clear tag filter" on:click=clear>"\u{d7}"</button>
                                        </span>
                                    }.into_view(),
                                );
                            }
                            chips
                        }}
                    </div>
                </Show>

                // Saved views
                <Show when=move || !saved_views.with(|v| v.is_empty()) fallback=|| ()>
                    <div class="spp-inbox__saved-views">
                        <label>"Saved views:"</label>
                        <select class="spp-inbox__filter">
                            <option value="">"— select —"</option>
                            {move || saved_views.with(|views| views.iter().map(|v| {
                                let id = v.id.unwrap_or(0);
                                let name = v.name.clone();
                                view! {
                                    <option value={id.to_string()}>{name.clone()}</option>
                                }
                            }).collect::<Vec<_>>())}
                        </select>
                    </div>
                </Show>

                // List body
                <div class="spp-inbox__list-body">
                    <Show when=move || list_loading.get() fallback=|| ()>
                        <LoadingState />
                    </Show>
                    <Show when=move || list_error.get().is_some() fallback=|| ()>
                        <div class="spp-state spp-state--error">
                            <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                            <p class="spp-state__body">
                                {move || list_error.get().unwrap_or_default()}
                            </p>
                        </div>
                    </Show>
                    <Show
                        when=move || !list_loading.get() && list_error.get().is_none()
                        fallback=|| ()
                    >
                        <Show
                            when=move || !conversations.with(|c| c.is_empty())
                            fallback=move || {
                                view! {
                                    <EmptyState message="No conversations match the current filters. Try clearing filters or syncing with Help Scout." />
                                }
                            }
                        >
                            {move || conversations.with(|items| {
                                items.iter().map(|item| {
                                    let id = item.id;
                                    let is_selected = move || selected_id.get() == Some(id);
                                    let is_checked = move || selected_ids.with(|ids| ids.contains(&id));
                                    view! {
                                        <div
                                            class="spp-inbox__item"
                                            class:is-selected=is_selected
                                            on:click=move |_| {
                                                selected_id.set(Some(id));
                                            }
                                        >
                                            <input
                                                type="checkbox"
                                                class="spp-inbox__item-checkbox"
                                                checked=is_checked
                                                on:click=move |ev| {
                                                    // Stop the click from bubbling to the item.
                                                    ev.stop_propagation();
                                                    selected_ids.update(|ids| {
                                                        if ids.contains(&id) {
                                                            ids.retain(|x| *x != id);
                                                        } else {
                                                            ids.push(id);
                                                        }
                                                    });
                                                }
                                            />
                                            <div class="spp-inbox__item-body">
                                                <div class="spp-inbox__item-subject">
                                                    {item.subject.clone().unwrap_or_else(|| format!("#{}", item.number))}
                                                </div>
                                                <div class="spp-inbox__item-preview">
                                                    {item.preview.clone().unwrap_or_default()}
                                                </div>
                                                <div class="spp-inbox__item-meta">
                                                    <span class="spp-inbox__item-status">
                                                        {item.status.clone()}
                                                    </span>
                                                    <span class="spp-inbox__item-priority">
                                                        {item.priority.clone().unwrap_or_default()}
                                                    </span>
                                                    <span class="spp-inbox__item-customer">
                                                        {item.customer_name.clone().unwrap_or_else(|| format!("#{}", item.customer_id))}
                                                    </span>
                                                </div>
                                                <div class="spp-inbox__item-response-state">
                                                    {item.response_state.clone().unwrap_or_default()}
                                                </div>
                                            </div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>()
                            })}
                        </Show>
                    </Show>
                </div>

                // Bulk actions bar (shown when items are selected)
                <Show when=move || !selected_ids.with(|ids| ids.is_empty()) fallback=|| ()>
                    <div class="spp-inbox__bulk-actions">
                        <span>
                            {move || format!("{} selected", selected_ids.with(|ids| ids.len()))}
                        </span>
                        <button
                            class="spp-button spp-button--ghost"
                            on:click=move |_| {
                                // Bulk close (reference: one POST /api/conversations/bulk
                                // with action "close"; the queue applies it per
                                // conversation). Outcomes surface as a toast like
                                // the reference's bulk mutation.
                                let ids = selected_ids.get();
                                let selected_id = selected_id;
                                let filters = filters;
                                let open_in_selection = selected_id
                                    .get()
                                    .is_some_and(|open| ids.contains(&open));
                                wasm_bindgen_futures::spawn_local(async move {
                                    let body = serde_json::json!({
                                        "conversationIds": ids,
                                        "action": "close",
                                        "params": {},
                                    });
                                    match crate::api::post_json::<serde_json::Value>(
                                        "/api/conversations/bulk",
                                        Some(&body),
                                    )
                                    .await
                                    {
                                        Ok(r) => {
                                            let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                                            let message = r
                                                .get("message")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("Bulk close failed")
                                                .to_string();
                                            if ok {
                                                // v2.2.1 audit fix: the OPEN detail also
                                                // showed the pre-bulk state when the
                                                // selection included it — refresh it too.
                                                if open_in_selection {
                                                    if let Some(id) = selected_id.get() {
                                                        selected_id.set(None);
                                                        selected_id.set(Some(id));
                                                    }
                                                }
                                                let current = filters.get();
                                                filters.set(InboxFilters::default());
                                                filters.set(current);
                                            }
                                            let kind = if ok {
                                                crate::toasts::ToastKind::Success
                                            } else {
                                                crate::toasts::ToastKind::Error
                                            };
                                            crate::toasts::push(kind, message, None);
                                        }
                                        Err(e) => toasts::error(e),
                                    }
                                });
                                selected_ids.set(Vec::new());
                            }
                        >
                            "Close all"
                        </button>
                        <button
                            class="spp-button spp-button--ghost"
                            on:click=move |_| {
                                selected_ids.set(Vec::new());
                            }
                        >
                            "Clear"
                        </button>
                    </div>
                </Show>
            </aside>

            // ── Detail pane (center) ──
            <main class="spp-inbox__detail">
                <Show
                    when=move || selected_id.get().is_some()
                    fallback=|| {
                        view! {
                            <EmptyState message="Select a conversation from the list to view its details." />
                        }
                    }
                >
                    <Show when=move || detail_loading.get() fallback=|| ()>
                        <LoadingState />
                    </Show>
                    <Show when=move || detail_error.get().is_some() fallback=|| ()>
                        <div class="spp-state spp-state--error">
                            <span class="spp-state__icon" aria-hidden="true">"⚠"</span>
                            <p class="spp-state__body">
                                {move || detail_error.get().unwrap_or_default()}
                            </p>
                        </div>
                    </Show>
                    <Show
                        when=move || !detail_loading.get() && detail_error.get().is_none()
                        fallback=|| ()
                    >
                        {move || detail.with(|d| {
                            let d = match d {
                                Some(d) => d.clone(),
                                None => return view! { <div></div> }.into_view(),
                            };
                            let d_subject = d.subject.clone().unwrap_or_else(|| format!("#{}", d.number));
                            let d_status = d.status.clone();
                            let d_priority = d.priority.clone();
                            let d_response_state = d.response_state.clone();
                            let d_assignee_id = d.assignee_id.unwrap_or(0).to_string();
                            let d_thread = d.thread.clone();
                            let d_thread_clone = d_thread.clone();
                            // Locals for the domain panels (the view! parser
                            // can't host brace-expressions in attributes).
                            let d_id = d.id;
                            let d_closed = d.status == "closed";
                            let d_customer_opt = if d.customer_id > 0 { Some(d.customer_id) } else { None };
                            view! {
                                <div class="spp-inbox__detail-content">
                                    <header class="spp-inbox__detail-header">
                                        <h3 class="spp-inbox__detail-subject">
                                            {d_subject}
                                        </h3>
                                        <div class="spp-inbox__detail-meta">
                                            <span class="spp-badge spp-badge--status">
                                                {d_status.clone()}
                                            </span>
                                            <span class="spp-badge spp-badge--priority">
                                                {d_priority.clone().unwrap_or_default()}
                                            </span>
                                            <span class="spp-badge spp-badge--response-state">
                                                {d_response_state.clone().unwrap_or_default()}
                                            </span>
                                        </div>
                                    </header>

                                    // Status + assignment controls
                                    <div class="spp-inbox__controls">
                                        <label>"Status:"</label>
                                        <select
                                            class="spp-inbox__control"
                                            value=d_status.clone()
                                            on:change=move |ev| {
                                                change_status(event_target_value(&ev));
                                            }
                                        >
                                            <option value="active">"Active"</option>
                                            <option value="pending">"Pending"</option>
                                            <option value="closed">"Closed"</option>
                                        </select>

                                        <label>"Assignee:"</label>
                                        <select
                                            class="spp-inbox__control"
                                            value=d_assignee_id.clone()
                                            on:change=move |ev| {
                                                let val = event_target_value(&ev);
                                                let id: i64 = val.parse().unwrap_or(0);
                                                let assignee_id = if id == 0 { None } else { Some(id) };
                                                assign_to(assignee_id);
                                            }
                                        >
                                            <option value="0">"Unassigned"</option>
                                            <option value="1">"User 1 (Alice)"</option>
                                            <option value="2">"User 2 (Bob)"</option>
                                        </select>
                                    </div>

                                    // Side collaboration threads (internal only).
                                    <SideThreadsPanel conversation_id=d_id />

                                    // Thread
                                    <div class="spp-inbox__thread">
                                        {if d_thread_clone.is_empty() {
                                            view! {
                                                <EmptyState message="No thread entries yet. Send a reply or note to start the conversation." />
                                            }.into_view()
                                        } else {
                                            d_thread_clone.iter().map(|entry| {
                                                let class = match entry.thread_type.as_str() {
                                                    "customer_message" => "spp-thread-entry spp-thread-entry--customer",
                                                    "reply" => "spp-thread-entry spp-thread-entry--reply",
                                                    "note" => "spp-thread-entry spp-thread-entry--note",
                                                    "system" => "spp-thread-entry spp-thread-entry--system",
                                                    _ => "spp-thread-entry",
                                                };
                                                let entry_type = entry.thread_type.clone();
                                                let entry_actor = entry.actor_name.clone().unwrap_or_else(|| entry.actor_type.clone());
                                                let entry_time = entry.created_at.clone();
                                                let entry_body = entry.body.clone();
                                                view! {
                                                    <div class={class}>
                                                        <div class="spp-thread-entry__header">
                                                            <span class="spp-thread-entry__type">
                                                                {entry_type}
                                                            </span>
                                                            <span class="spp-thread-entry__actor">
                                                                {entry_actor}
                                                            </span>
                                                            <span class="spp-thread-entry__time">
                                                                {entry_time}
                                                            </span>
                                                        </div>
                                                        // Sanitized server-side (reference SafeHtml:
                                                        // body_html with body_text fallback).
                                                        <SafeHtml html=entry_body fallback_text=String::new() />
                                                    </div>
                                                }
                                            }).collect::<Vec<_>>().into_view()
                                        }}
                                    </div>

                                    // Post-resolution QA + translation + memory panels
                                    // (reference: after the thread, before the composer).
                                    <QaPanel conversation_id=d_id closed=d_closed />
                                    <TranslationPanel conversation_id=d_id />
                                    <MemoryPanel
                                        customer_id=d_customer_opt
                                        conversation_id=d_id
                                    />

                                    // Composer
                                    <div class="spp-inbox__composer">
                                        <div class="spp-inbox__composer-tabs">
                                            <button
                                                class="spp-inbox__composer-tab"
                                                class:is-active=move || composer_mode.get() == ComposerMode::Reply
                                                on:click=move |_| composer_mode.set(ComposerMode::Reply)
                                            >
                                                "Reply"
                                            </button>
                                            <button
                                                class="spp-inbox__composer-tab"
                                                class:is-active=move || composer_mode.get() == ComposerMode::Note
                                                on:click=move |_| composer_mode.set(ComposerMode::Note)
                                            >
                                                "Internal note"
                                            </button>
                                        </div>
                                        <Show
                                            when=move || composer_mode.get() == ComposerMode::Reply
                                            fallback=move || view! {
                                                <MentionTextarea
                                                    value=composer_body
                                                    placeholder="Type an internal note (visible only to your team) — @ to mention"
                                                    rows=6
                                                />
                                            }
                                        >
                                            <MentionTextarea
                                                value=composer_body
                                                placeholder="Type your reply to the customer..."
                                                rows=6
                                            />
                                        </Show>
                                        <div class="spp-inbox__composer-actions">
                                            <button
                                                class="spp-button"
                                                on:click=move |_| request_submit()
                                            >
                                                {move || match composer_mode.get() {
                                                    ComposerMode::Reply => "Send reply",
                                                    ComposerMode::Note => "Add note",
                                                }}
                                            </button>
                                        </div>
                                        <Show when=move || composer_mode.get() == ComposerMode::Reply fallback=|| ()>
                                            <CoachingPanel conversation_id=d_id draft=composer_body />
                                        </Show>
                                        <Show when=move || composer_error.get().is_some() fallback=|| ()>
                                            <div class="spp-state spp-state--error">
                                                {move || composer_error.get().unwrap_or_default()}
                                            </div>
                                        </Show>
                                        <Show when=move || confirm_send.get() fallback=|| ()>
                                            {move || {
                                                let email = detail.get()
                                                    .and_then(|d| d.customer_email)
                                                    .unwrap_or_else(|| "the customer".to_string());
                                                let on_confirm = std::sync::Arc::new(move || {
                                                    confirm_send.set(false);
                                                    send_composer();
                                                });
                                                let on_cancel = std::sync::Arc::new(move || confirm_send.set(false));
                                                view! {
                                                    <ConfirmDialog
                                                        title="Send reply to customer"
                                                        message=format!(
                                                            "Send this reply to {email} via Help Scout? This is a customer-visible action."
                                                        )
                                                        confirm_label="Send reply"
                                                        on_confirm
                                                        on_cancel
                                                    />
                                                }
                                            }}
                                        </Show>
                                    </div>
                                </div>
                            }.into_view()
                        })}
                    </Show>
                </Show>
            </main>

            // ── Context pane (right) ──
            <aside class="spp-inbox__context">
                <Show
                    when=move || selected_id.get().is_some()
                    fallback=|| {
                        view! {
                            <EmptyState message="Customer context will appear here when a conversation is selected." />
                        }
                    }
                >
                    {move || {
                        let conv_id = selected_id.get().unwrap_or(0);
                        view! {
                            <div class="spp-inbox__context-content">
                                <div class="spp-inbox__context-tabs">
                                    <div class="spp-inbox__context-tabbar">
                                        <button
                                            class="spp-inbox__context-tab"
                                            class:is-active=move || context_tab.get() == ContextTab::Ai
                                            on:click=move |_| context_tab.set(ContextTab::Ai)
                                        >
                                            "AI"
                                        </button>
                                        <button
                                            class="spp-inbox__context-tab"
                                            class:is-active=move || context_tab.get() == ContextTab::Customer
                                            on:click=move |_| context_tab.set(ContextTab::Customer)
                                        >
                                            "Customer"
                                        </button>
                                        <button
                                            class="spp-inbox__context-tab"
                                            class:is-active=move || context_tab.get() == ContextTab::Copilot
                                            on:click=move |_| context_tab.set(ContextTab::Copilot)
                                            title="Ask the Local Copilot about this ticket (read-only, evidence-cited)"
                                        >
                                            "Copilot"
                                        </button>
                                    </div>
                                    <button
                                        class="spp-button spp-button--ghost spp-button--small"
                                        aria-label="Hide context pane"
                                        on:click=move |_| context_open.set(false)
                                    >
                                        "Hide"
                                    </button>
                                </div>

                                <Show
                                    when=move || !context_open.get()
                                    fallback=|| ()
                                >
                                    <button
                                        class="spp-button spp-button--small spp-inbox__context-reopen"
                                        on:click=move |_| context_open.set(true)
                                    >
                                        "Show context"
                                    </button>
                                </Show>

                                <Show
                                    when=move || context_open.get() && context_tab.get() == ContextTab::Ai
                                    fallback=|| ()
                                >
                                    <div class="spp-inbox__context-body">
                                        // Per-ticket AI attribute snapshot (M3).
                                        <AttributeSnapshotCard conversation_id=conv_id />

                                        // What is the customer asking? (reference AiSidebar)
                                        <div class="spp-ai-sidebar-section">
                                            <h4>"What is the customer asking?"</h4>
                                            <button
                                                class="spp-button spp-button--small"
                                                on:click=move |_| run_analyze()
                                                disabled=move || ai_analyzing.get()
                                            >
                                                {move || if ai_analyzing.get() { "Analyzing…" } else { "Analyze" }.to_string()}
                                            </button>
                                            {move || {
                                                match ai_analysis.get() {
                                                    Some(a) => {
                                                        let intent = a.get("intent").and_then(|x| x.as_str()).unwrap_or("—").to_string();
                                                        let confidence = a.get("confidence").and_then(|x| x.as_str()).unwrap_or("unknown").to_string();
                                                        let topics = a.get("topics")
                                                            .and_then(|t| t.as_array())
                                                            .map(|rows| rows.iter().filter_map(|t| t.as_str().map(str::to_string)).collect::<Vec<_>>())
                                                            .unwrap_or_default();
                                                        view! {
                                                            <div class="spp-text-sm spp-ai-analysis">
                                                                <div class="spp-ai-analysis__row">
                                                                    <strong>"Intent: "</strong>
                                                                    {intent.clone()}
                                                                    <span class="spp-badge">{format!("conf: {confidence}")}</span>
                                                                </div>
                                                                {if !topics.is_empty() {
                                                                    view! {
                                                                        <div class="spp-ai-analysis__row">
                                                                            <strong>"Topics: "</strong>
                                                                            {topics.join(", ")}
                                                                        </div>
                                                                    }.into_view()
                                                                } else {
                                                                    ().into_view()
                                                                }}
                                                            </div>
                                                        }.into_view()
                                                    }
                                                    None => ().into_view(),
                                                }
                                            }}

                                            // Similar past cases (evidence-backed).
                                            <h4>"Similar past cases"</h4>
                                            {move || {
                                                let rows = similar.get();
                                                if rows.is_empty() {
                                                    view! {
                                                        <div class="spp-muted spp-text-xs">"No similar conversations found yet."</div>
                                                    }.into_view()
                                                } else {
                                                    rows.iter().map(|s| {
                                                        let href = format!("/inbox/conversation/{}", s.conversation_id);
                                                        view! {
                                                            <div class="spp-ai-similar">
                                                                <a class="spp-ai-similar__subject" href=href.clone()>
                                                                    {format!("#{} {}", s.number, s.subject.clone())}
                                                                </a>
                                                                <span class="spp-badge">{format!("{:.0}%", s.score * 100.0)}</span>
                                                                {if !s.why.is_empty() {
                                                                    view! {
                                                                        <div class="spp-muted spp-text-xs">{s.why.join(" · ")}</div>
                                                                    }.into_view()
                                                                } else {
                                                                    ().into_view()
                                                                }}
                                                                {if !s.resolution.is_empty() {
                                                                    view! {
                                                                        <div class="spp-text-xs spp-ai-similar__resolution">{s.resolution.clone()}</div>
                                                                    }.into_view()
                                                                } else {
                                                                    ().into_view()
                                                                }}
                                                            </div>
                                                        }
                                                    }).collect::<Vec<_>>().into_view()
                                                }
                                            }}
                                        </div>
                                    </div>
                                </Show>

                                <Show
                                    when=move || context_open.get() && context_tab.get() == ContextTab::Customer
                                    fallback=|| ()
                                >
                                    {move || detail.with(|d| {
                                        let d = match d {
                                            Some(d) => d.clone(),
                                            None => return view! { <div></div> }.into_view(),
                                        };
                                        let c_name = d.customer_name.clone().unwrap_or_else(|| format!("#{}", d.customer_id));
                                        let c_email = d.customer_email.clone();
                                        let c_mailbox = d.mailbox_name.clone().unwrap_or_else(|| format!("#{}", d.mailbox_id));
                                        let c_assignee = d.assignee_name.clone().unwrap_or_else(|| "Unassigned".to_string());
                                        let c_created = d.created_at.clone().unwrap_or_default();
                                        let c_updated = d.updated_at.clone().unwrap_or_default();
                                        let c_tags = d.tags.clone();
                                        let c_tags_clone = c_tags.clone();
                                        view! {
                                            <div class="spp-inbox__context-body">
                                                <h3>"Customer"</h3>
                                                <dl class="spp-inbox__context-list">
                                                    <dt>"Name"</dt>
                                                    <dd>{c_name}</dd>
                                                    <dt>"Email"</dt>
                                                    <dd>{c_email.clone().unwrap_or_default()}</dd>
                                                    <dt>"Mailbox"</dt>
                                                    <dd>{c_mailbox}</dd>
                                                    <dt>"Assignee"</dt>
                                                    <dd>{c_assignee}</dd>
                                                    <dt>"Created"</dt>
                                                    <dd>{c_created}</dd>
                                                    <dt>"Updated"</dt>
                                                    <dd>{c_updated}</dd>
                                                </dl>

                                                {if !c_tags_clone.is_empty() {
                                                    view! {
                                                        <h3>"Tags"</h3>
                                                        <ul class="spp-inbox__tags">
                                                            {c_tags_clone.iter().map(|tag| {
                                                                let tag = tag.clone();
                                                                view! {
                                                                    <li class="spp-inbox__tag">{tag}</li>
                                                                }
                                                            }).collect::<Vec<_>>()}
                                                        </ul>
                                                    }.into_view()
                                                } else {
                                                    ().into_view()
                                                }}
                                            </div>
                                        }.into_view()
                                    })}
                                </Show>

                                <Show
                                    when=move || context_open.get() && context_tab.get() == ContextTab::Copilot
                                    fallback=|| ()
                                >
                                    <div class="spp-inbox__context-body">
                                        <CopilotPanel conversation_id=conv_id />
                                    </div>
                                </Show>
                            </div>
                        }.into_view()
                    }}
                </Show>
            </aside>
        </div>
    }
}

// ─── JSON parsers ─────────────────────────────────────────────────────────

fn parse_conversation_list_item(v: &serde_json::Value) -> ConversationListItem {
    ConversationListItem {
        id: v.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        remote_id: v.get("remote_id").and_then(|v| v.as_i64()).unwrap_or(0),
        number: v.get("number").and_then(|v| v.as_i64()).unwrap_or(0),
        subject: v
            .get("subject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        preview: v
            .get("preview")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        status: v
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            .to_string(),
        mailbox_id: v.get("mailbox_id").and_then(|v| v.as_i64()).unwrap_or(0),
        mailbox_name: v
            .get("mailbox_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        assignee_id: v.get("assignee_id").and_then(|v| v.as_i64()),
        assignee_name: v
            .get("assignee_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        customer_id: v.get("customer_id").and_then(|v| v.as_i64()).unwrap_or(0),
        customer_name: v
            .get("customer_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        priority: v
            .get("priority")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        created_at: v
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        updated_at: v
            .get("updated_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        closed_at: v
            .get("closed_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        response_state: v
            .get("response_state")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

fn parse_conversation_detail(v: &serde_json::Value) -> ConversationDetail {
    let thread = v
        .get("thread")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(parse_thread_entry).collect::<Vec<_>>())
        .unwrap_or_default();
    let tags = v
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ConversationDetail {
        id: v.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        remote_id: v.get("remote_id").and_then(|v| v.as_i64()).unwrap_or(0),
        number: v.get("number").and_then(|v| v.as_i64()).unwrap_or(0),
        subject: v
            .get("subject")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        preview: v
            .get("preview")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        status: v
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("active")
            .to_string(),
        mailbox_id: v.get("mailbox_id").and_then(|v| v.as_i64()).unwrap_or(0),
        mailbox_name: v
            .get("mailbox_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        assignee_id: v.get("assignee_id").and_then(|v| v.as_i64()),
        assignee_name: v
            .get("assignee_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        customer_id: v.get("customer_id").and_then(|v| v.as_i64()).unwrap_or(0),
        customer_name: v
            .get("customer_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        customer_email: v
            .get("customer_email")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        priority: v
            .get("priority")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        response_state: v
            .get("response_state")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        created_at: v
            .get("created_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        updated_at: v
            .get("updated_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        closed_at: v
            .get("closed_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        thread,
        tags,
    }
}

fn parse_thread_entry(v: &serde_json::Value) -> ThreadEntry {
    ThreadEntry {
        id: v.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        conversation_id: v
            .get("conversation_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        thread_type: v
            .get("thread_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        body: v
            .get("body")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        actor_type: v
            .get("actor_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        actor_id: v.get("actor_id").and_then(|v| v.as_i64()),
        actor_name: v
            .get("actor_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        created_at: v
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conversation_list_item_handles_empty_json() {
        let v = serde_json::json!({});
        let item = parse_conversation_list_item(&v);
        assert_eq!(item.id, 0);
        assert_eq!(item.status, "active");
    }

    #[test]
    fn parse_conversation_list_item_extracts_all_fields() {
        let v = serde_json::json!({
            "id": 1,
            "remote_id": 1001,
            "number": 1001,
            "subject": "Hello",
            "preview": "World",
            "status": "active",
            "mailbox_id": 1,
            "mailbox_name": "Support",
            "assignee_id": 2,
            "assignee_name": "Alice Agent",
            "customer_id": 3,
            "customer_name": "Bob Customer",
            "priority": "high",
            "response_state": "needs_first_response",
            "created_at": "2025-01-01T00:00:00Z",
        });
        let item = parse_conversation_list_item(&v);
        assert_eq!(item.id, 1);
        assert_eq!(item.subject.as_deref(), Some("Hello"));
        assert_eq!(item.status, "active");
        assert_eq!(item.mailbox_name.as_deref(), Some("Support"));
        assert_eq!(item.assignee_name.as_deref(), Some("Alice Agent"));
        assert_eq!(item.customer_name.as_deref(), Some("Bob Customer"));
        assert_eq!(item.priority.as_deref(), Some("high"));
        assert_eq!(item.response_state.as_deref(), Some("needs_first_response"));
    }

    #[test]
    fn parse_conversation_detail_includes_thread() {
        let v = serde_json::json!({
            "id": 1,
            "remote_id": 1001,
            "number": 1001,
            "subject": "Hello",
            "status": "active",
            "mailbox_id": 1,
            "customer_id": 3,
            "thread": [
                {
                    "id": 10,
                    "conversation_id": 1,
                    "thread_type": "customer_message",
                    "body": "Hello world",
                    "actor_type": "customer",
                    "created_at": "2025-01-01T00:00:00Z",
                },
                {
                    "id": 11,
                    "conversation_id": 1,
                    "thread_type": "reply",
                    "body": "Hi there",
                    "actor_type": "user",
                    "actor_id": 2,
                    "actor_name": "Alice Agent",
                    "created_at": "2025-01-01T00:01:00Z",
                },
            ],
            "tags": ["urgent", "billing"],
        });
        let d = parse_conversation_detail(&v);
        assert_eq!(d.id, 1);
        assert_eq!(d.thread.len(), 2);
        assert_eq!(d.thread[0].thread_type, "customer_message");
        assert_eq!(d.thread[0].body.as_deref(), Some("Hello world"));
        assert_eq!(d.thread[1].actor_name.as_deref(), Some("Alice Agent"));
        assert_eq!(d.tags, vec!["urgent", "billing"]);
    }

    #[test]
    fn parse_thread_entry_handles_missing_fields() {
        let v = serde_json::json!({
            "id": 1,
            "conversation_id": 1,
            "thread_type": "system",
            "actor_type": "system",
            "created_at": "2025-01-01T00:00:00Z",
        });
        let e = parse_thread_entry(&v);
        assert_eq!(e.id, 1);
        assert_eq!(e.thread_type, "system");
        assert!(e.body.is_none());
        assert!(e.actor_id.is_none());
        assert!(e.actor_name.is_none());
    }
}
