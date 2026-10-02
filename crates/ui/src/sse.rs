//! SSE event subscription — mirrors the reference's `src/client/api/events.ts`.
//!
//! In the reference React app, `ServerEventsBridge` is mounted once in App
//! and subscribes to `/api/events` via `EventSource`. Server events are
//! forwarded to all subscribed listeners, which then invalidate their
//! TanStack Query caches to refresh views without polling.
//!
//! In the Leptos port we expose a similar API: `subscribe()` returns a
//! `wasm_bindgen::closure::Closure` handle that the caller must keep alive.
//! Each event is deserialized from JSON and forwarded to all registered
//! listeners.
//!
//! The Leptos UI uses `create_resource` + `create_signal` patterns; on
//! receiving a `LiveEvent`, callers can re-fetch their resources.

use std::sync::Mutex;

use serde::Deserialize;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// A live event from the server (mirrors spp_core::events::LiveEvent).
/// We duplicate the type here because the core crate isn't WASM-safe
/// (it depends on rusqlite which is native-only).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum LiveEvent {
    SyncUpdated { resource: String, count: u32 },
    WebhookReceived { event_id: String },
    RatingArrived { rating_id: String, rating: u32 },
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
        let event_source = web_sys::EventSource::new("/api/events")
            .map_err(|e| format!("EventSource::new failed: {e:?}"))?;

        // The reference listens for named events ('hello', 'ratings',
        // 'sync', 'conversation', 'campaign', 'notification', 'error').
        // The port's SSE handler sends unnamed `data:` lines (one JSON
        // object per event with a `type` field), so we listen on the
        // default 'message' event and dispatch by `type` in the handler.
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let data_str = event.data().as_string().unwrap_or_else(|| "{}".to_string());
                let event: LiveEvent = match serde_json::from_str(&data_str) {
                    Ok(e) => e,
                    Err(_) => return, // keep-alive comments or malformed
                };
                let subs = match SUBSCRIBERS.lock() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                for handler in subs.iter() {
                    // Listener bugs never break the stream.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler(&event);
                    }));
                }
            },
        );
        event_source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // Keep the closure alive.
        let handle = EventSourceHandle {
            source: event_source,
            _closure: on_message,
        };
        *s.borrow_mut() = Some(handle);
        Ok(())
    })
}

/// RAII handle for an EventSource + its onmessage closure.
struct EventSourceHandle {
    #[allow(dead_code)]
    source: web_sys::EventSource,
    _closure: Closure<dyn FnMut(web_sys::MessageEvent)>,
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn subscribe_mock_invokes_handler() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();
        let _unsub = subscribe_mock(Box::new(move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        }));

        emit_test(&LiveEvent::SyncUpdated {
            resource: "x".into(),
            count: 1,
        });
        emit_test(&LiveEvent::WebhookReceived {
            event_id: "evt1".into(),
        });

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn unsubscribe_removes_handler() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();
        let unsub = subscribe_mock(Box::new(move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        }));

        emit_test(&LiveEvent::SyncUpdated {
            resource: "x".into(),
            count: 1,
        });
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        let _ = unsub();
        emit_test(&LiveEvent::SyncUpdated {
            resource: "x".into(),
            count: 1,
        });
        // No-op handler replaced the real one; counter should not increment.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn multiple_subscribers_all_invoked() {
        let c1 = Arc::new(AtomicU32::new(0));
        let c2 = Arc::new(AtomicU32::new(0));
        let c1_clone = c1.clone();
        let c2_clone = c2.clone();
        let _u1 = subscribe_mock(Box::new(move |_| {
            c1_clone.fetch_add(1, Ordering::SeqCst);
        }));
        let _u2 = subscribe_mock(Box::new(move |_| {
            c2_clone.fetch_add(1, Ordering::SeqCst);
        }));

        emit_test(&LiveEvent::RatingArrived {
            rating_id: "r1".into(),
            rating: 5,
        });
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn live_event_deserializes() {
        let json = r#"{"type":"WebhookReceived","event_id":"evt_42"}"#;
        let event: LiveEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            LiveEvent::WebhookReceived { event_id } if event_id == "evt_42"
        ));

        let json = r#"{"type":"SyncUpdated","resource":"conversations","count":7}"#;
        let event: LiveEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            LiveEvent::SyncUpdated { resource, count } if resource == "conversations" && count == 7
        ));

        let json = r#"{"type":"RatingArrived","rating_id":"r_99","rating":5}"#;
        let event: LiveEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            LiveEvent::RatingArrived { rating_id, rating } if rating_id == "r_99" && rating == 5
        ));
    }
}
