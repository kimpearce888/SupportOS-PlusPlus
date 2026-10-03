//! SSE event subscription — mirrors the reference's `src/client/api/events.ts`.
//!
//! The reference React app mounts `ServerEventsBridge` once, which opens a
//! single `EventSource('/api/events')` and listens for the NAMED events
//! (`hello`, `ratings`, `sync`, `conversation`, `campaign`, `notification`,
//! `error`), converting them into query invalidations + toasts.
//!
//! The Leptos port exposes the same surface: `subscribe()` registers a
//! handler; a single global `EventSource` fans named events out to all
//! handlers. The URL is absolute (`http://127.0.0.1:3000/api/events`) because
//! the WASM bundle is served from the Tauri asset origin (or the Trunk dev
//! server), NOT from the Axum API server — a relative URL would hit the wrong
//! origin. The base can be overridden via `localStorage['spp.api_base']`
//! (used when the app runs on a non-default PORT).

use std::sync::Mutex;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// The named SSE events the server sends (reference wire names).
pub const EVENT_NAMES: [&str; 6] = [
    "hello",
    "ratings",
    "sync",
    "conversation",
    "campaign",
    "notification",
];

/// A live event from the server, parsed from the `data:` JSON of a named
/// SSE event. We duplicate the shapes here because the core crate isn't
/// WASM-safe (it depends on rusqlite which is native-only).
#[derive(Debug, Clone)]
pub enum LiveEvent {
    /// `event: hello` — sent on connect.
    Hello { at: String, version: String },
    /// `event: ratings` — rating-received payload.
    RatingReceived {
        rating: Option<String>,
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        customer_id: Option<i64>,
        customer_name: Option<String>,
        comments: Option<String>,
        at: String,
    },
    /// `event: ratings` — ratings-refreshed payload.
    RatingsRefreshed {
        processed: u32,
        fresh: u32,
        at: String,
    },
    /// `event: sync` — sync-completed payload.
    SyncCompleted {
        kind: String,
        processed: u32,
        errors: u32,
        at: String,
    },
    /// `event: conversation` — conversation-updated payload.
    ConversationUpdated {
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        mailbox_id: Option<i64>,
        subject: Option<String>,
        reason: String,
        at: String,
    },
    /// `event: campaign` — campaign-updated payload.
    CampaignUpdated {
        campaign_id: i64,
        status: String,
        sent: u32,
        failed: u32,
        unknown: u32,
        remaining: u32,
        at: String,
    },
    /// `event: notification` — notification-received payload.
    NotificationReceived {
        id: i64,
        kind: String,
        severity: String,
        title: String,
        conversation_id: Option<i64>,
        conversation_number: Option<i64>,
        customer_id: Option<i64>,
        target_user_local_id: Option<i64>,
        unread_count: u32,
        at: String,
    },
    /// `event: error` — stream-level error (e.g. too many streams).
    Error { message: String },
}

/// A handler for a live event.
pub type EventHandler = Box<dyn Fn(&LiveEvent) + Send + Sync + 'static>;

/// A subscriber registry. Multiple components can subscribe; each event
/// is forwarded to all subscribers.
static SUBSCRIBERS: Mutex<Vec<EventHandler>> = Mutex::new(Vec::new());

/// Subscribe to live events from the server.
///
/// Returns an unsubscribe function (a `Box<dyn FnOnce() -> Result<(), String>>`)
/// that the caller invokes to remove their handler.
pub fn subscribe(handler: EventHandler) -> Result<Box<dyn FnOnce() -> Result<(), String>>, String> {
    let mut subs = SUBSCRIBERS
        .lock()
        .map_err(|e| format!("subscriber lock poisoned: {e}"))?;
    subs.push(handler);
    let idx = subs.len() - 1;
    drop(subs);

    // Ensure the global EventSource is open.
    ensure_source()?;

    Ok(Box::new(move || -> Result<(), String> {
        let mut subs = SUBSCRIBERS
            .lock()
            .map_err(|e| format!("subscriber lock poisoned: {e}"))?;
        // Replace the removed slot with a no-op (cheaper than shifting).
        if idx < subs.len() {
            subs[idx] = Box::new(|_| {});
        }
        Ok(())
    }))
}

/// Open the global EventSource if not already open. Idempotent.
#[allow(clippy::missing_const_for_thread_local)]
fn ensure_source() -> Result<(), String> {
    thread_local! {
        static SOURCE: std::cell::RefCell<Option<EventSourceHandle>> = std::cell::RefCell::new(None);
    }

    SOURCE.with(|s| {
        if s.borrow().is_some() {
            return Ok(());
        }

        let _window = web_sys::window().ok_or("no window")?;
        // Absolute URL: the WASM bundle is served from the Tauri asset origin
        // (or the Trunk dev server on :1420), NOT from the Axum API server,
        // so a relative `/api/events` would resolve to the wrong origin.
        // Shared base from `crate::api` (localStorage override 'spp.api_base').
        let url = format!("{}/api/events", crate::api::api_base());
        let event_source = web_sys::EventSource::new(&url)
            .map_err(|e| format!("EventSource::new({url}) failed: {e:?}"))?;

        // The reference listens for NAMED events (one listener per wire
        // name); keep-alive comments arrive without a listener and are
        // ignored by the browser, exactly like the reference.
        for name in EVENT_NAMES {
            let cb = Closure::<dyn FnMut(web_sys::MessageEvent)>::new({
                let name = name.to_string();
                move |event: web_sys::MessageEvent| {
                    let data_str = event.data().as_string().unwrap_or_else(|| "{}".to_string());
                    dispatch_named(&name, &data_str);
                }
            });
            event_source
                .add_event_listener_with_callback(name, cb.as_ref().unchecked_ref())
                .map_err(|e| format!("addEventListener({name}) failed: {e:?}"))?;
            // NOTE: the closure is leaked intentionally — the EventSource lives
            // for the page lifetime and addEventListener holds only a JS ref.
            // One leaked closure per named event per page load (6 total).
            cb.forget();
        }

        let handle = EventSourceHandle {
            source: event_source,
        };
        *s.borrow_mut() = Some(handle);
        Ok(())
    })
}

/// Parse a named event's `data:` JSON and fan it out to all subscribers.
fn dispatch_named(name: &str, data: &str) {
    let event = match (name, serde_json::from_str::<serde_json::Value>(data)) {
        (_, Ok(v)) if !v.is_null() => v,
        _ => return, // malformed payload — ignore, like the reference
    };
    let live = match name {
        "hello" => parse_h(&event),
        "ratings" => parse_ratings(&event),
        "sync" => LiveEvent::SyncCompleted {
            kind: str_field(&event, "kind").unwrap_or_default(),
            processed: num_field(&event, "processed").unwrap_or(0).max(0) as u32,
            errors: num_field(&event, "errors").unwrap_or(0).max(0) as u32,
            at: str_field(&event, "at").unwrap_or_default(),
        },
        "conversation" => LiveEvent::ConversationUpdated {
            conversation_id: num_field(&event, "conversationId"),
            conversation_number: num_field(&event, "conversationNumber"),
            mailbox_id: num_field(&event, "mailboxId"),
            subject: str_field(&event, "subject"),
            reason: str_field(&event, "reason").unwrap_or_default(),
            at: str_field(&event, "at").unwrap_or_default(),
        },
        "campaign" => LiveEvent::CampaignUpdated {
            campaign_id: num_field(&event, "campaignId").unwrap_or(0),
            status: str_field(&event, "status").unwrap_or_default(),
            sent: num_field(&event, "sent").unwrap_or(0).max(0) as u32,
            failed: num_field(&event, "failed").unwrap_or(0).max(0) as u32,
            unknown: num_field(&event, "unknown").unwrap_or(0).max(0) as u32,
            remaining: num_field(&event, "remaining").unwrap_or(0).max(0) as u32,
            at: str_field(&event, "at").unwrap_or_default(),
        },
        "notification" => LiveEvent::NotificationReceived {
            id: num_field(&event, "id").unwrap_or(0),
            kind: str_field(&event, "type").unwrap_or_default(),
            severity: str_field(&event, "severity").unwrap_or_default(),
            title: str_field(&event, "title").unwrap_or_default(),
            conversation_id: num_field(&event, "conversationId"),
            conversation_number: num_field(&event, "conversationNumber"),
            customer_id: num_field(&event, "customerId"),
            target_user_local_id: num_field(&event, "targetUserLocalId"),
            unread_count: num_field(&event, "unreadCount").unwrap_or(0).max(0) as u32,
            at: str_field(&event, "at").unwrap_or_default(),
        },
        "error" => LiveEvent::Error {
            message: str_field(&event, "message").unwrap_or_default(),
        },
        _ => return,
    };
    let subs = match SUBSCRIBERS.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    for handler in subs.iter() {
        // Listener bugs never break the stream.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler(&live);
        }));
    }
}

fn parse_h(v: &serde_json::Value) -> LiveEvent {
    LiveEvent::Hello {
        at: str_field(v, "at").unwrap_or_default(),
        version: str_field(v, "version").unwrap_or_default(),
    }
}

fn parse_ratings(v: &serde_json::Value) -> LiveEvent {
    // `ratings` carries either a rating-received or a ratings-refreshed
    // payload; distinguish by field presence (reference does the same by
    // listener registration order).
    if v.get("processed").is_some() {
        LiveEvent::RatingsRefreshed {
            processed: num_field(v, "processed").unwrap_or(0).max(0) as u32,
            fresh: num_field(v, "fresh").unwrap_or(0).max(0) as u32,
            at: str_field(v, "at").unwrap_or_default(),
        }
    } else {
        LiveEvent::RatingReceived {
            rating: str_field(v, "rating"),
            conversation_id: num_field(v, "conversationId"),
            conversation_number: num_field(v, "conversationNumber"),
            customer_id: num_field(v, "customerId"),
            customer_name: str_field(v, "customerName"),
            comments: str_field(v, "comments"),
            at: str_field(v, "at").unwrap_or_default(),
        }
    }
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(String::from)
}

fn num_field(v: &serde_json::Value, key: &str) -> Option<i64> {
    v.get(key)?.as_i64()
}

/// RAII handle for the EventSource.
struct EventSourceHandle {
    #[allow(dead_code)]
    source: web_sys::EventSource,
}

/// Convenience: emit a test event to all subscribers (test-only).
#[cfg(test)]
pub fn emit_test(event: &LiveEvent) {
    let subs = match SUBSCRIBERS.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    for handler in subs.iter() {
        handler(event);
    }
}

/// Mock the SSE subscription in tests — installs a handler without
/// opening a real EventSource.
#[cfg(test)]
pub fn subscribe_mock(handler: EventHandler) -> Box<dyn FnOnce() -> Result<(), String>> {
    let mut subs = SUBSCRIBERS
        .lock()
        .expect("subscriber lock poisoned in test");
    subs.push(handler);
    let idx = subs.len() - 1;
    drop(subs);
    Box::new(move || -> Result<(), String> {
        let mut subs = SUBSCRIBERS
            .lock()
            .map_err(|e| format!("subscriber lock poisoned: {e}"))?;
        if idx < subs.len() {
            subs[idx] = Box::new(|_| {});
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_named_conversation_event() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let _unsub = subscribe_mock(Box::new(move |e| {
            if let LiveEvent::ConversationUpdated { reason, .. } = e {
                if reason == "webhook" {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }));
        dispatch_named(
            "conversation",
            r#"{"conversationId":5,"conversationNumber":42,"mailboxId":2,"subject":"SSO","reason":"webhook","at":"now"}"#,
        );
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn dispatch_named_ratings_distinguishes_shapes() {
        let got_refresh = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = got_refresh.clone();
        let _unsub = subscribe_mock(Box::new(move |e| {
            if let LiveEvent::RatingsRefreshed {
                ref processed,
                ref fresh,
                ..
            } = e
            {
                if *processed == 3 && *fresh == 2 {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }));
        dispatch_named("ratings", r#"{"processed":3,"fresh":2,"at":"now"}"#);
        assert!(got_refresh.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn malformed_payload_is_ignored() {
        // Must not panic.
        dispatch_named("sync", "not json");
        dispatch_named("ratings", "");
    }
}
