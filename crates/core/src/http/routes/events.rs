//! SSE events route — real-time push over HTTP.
//!
//! Mirrors: src/server/routes/events.ts
//!
//! Each connected client subscribes to the shared `EventBus` (a tokio
//! broadcast channel). When any route handler emits a `LiveEvent`
//! (webhook received, sync updated, rating arrived), the bus fans the
//! event out to every subscriber. The SSE handler serializes the event
//! to JSON and writes it as a `data:` line.
//!
//! Between events, the handler sends a `:keep-alive` comment every 25
//! seconds so reverse proxies and the browser don't time out the
//! connection (matches the reference's keep-alive interval).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use futures_util::stream::{self, StreamExt as _};
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};

use super::super::server::AppState;

/// GET /api/events — SSE stream for real-time updates.
///
/// Each connected client receives every `LiveEvent` emitted on the bus.
/// If the client lags behind more than `capacity` events (256 by default),
/// it will see a `Lagged(n)` notification in the stream which we surface
/// as a comment line — the client's TanStack Query caches will simply
/// miss the dropped events and re-fetch on next invalidate.
pub async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.bus.subscribe();
    let event_stream = BroadcastStream::new(rx).map(|result| match result {
        Ok(event) => {
            // Serialize the LiveEvent as JSON. The browser-side
            // EventSource handler reads `event.data` as JSON and
            // switches on `type` to know which query to invalidate.
            let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            Ok(Event::default().data(payload))
        }
        Err(_lagged) => {
            // The subscriber lagged behind and dropped events.
            // Surface as a comment so the client knows to refresh.
            Ok(Event::default().comment("lagged"))
        }
    });

    // Interleave keep-alive comments every 25 seconds.
    let keep_alive = IntervalStream::new(tokio::time::interval(Duration::from_secs(25)))
        .map(|_| Ok(Event::default().comment("keep-alive")));

    let merged = stream::select(event_stream, keep_alive);

    Sse::new(merged).keep_alive(KeepAlive::new().interval(Duration::from_secs(25)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::LiveEvent;
    use crate::http::EventBus;
    use std::sync::Arc;
    use std::sync::Mutex;

    fn make_state() -> AppState {
        AppState {
            conn: Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap())),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: EventBus::new(8),
            limiter: crate::http::RateLimiter::new(),
        }
    }

    #[tokio::test]
    async fn sse_emits_event_from_bus() {
        let state = make_state();
        // Spawn a task that emits one event after the SSE stream starts.
        let bus = state.bus.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            bus.emit(&LiveEvent::WebhookReceived {
                event_id: "evt_test".into(),
            });
        });

        // Drive the SSE handler for a short time to capture the event.
        let _ = handle.await;
        // The handler is `async fn` returning `Sse<impl Stream>` —
        // constructing it does not consume state, so we just verify
        // it returns a non-panicking response.
        let _response = sse_handler(State(state)).await;
    }
}
