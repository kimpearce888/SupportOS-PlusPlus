//! SSE events route — real-time push over HTTP.
//!
//! Mirrors `src/server/routes/events.ts` exactly:
//! - wire format `event: <name>\ndata: <json>\n\n` with named events
//! - `hello` sent immediately on connect: `{at, channels, version}`
//! - 25-stream cap → `error` event + close (drop-oldest is NOT acceptable)
//! - heartbeat comment `: ping\n\n` every 25 s
//! - headers: `text/event-stream; charset=utf-8`, `Cache-Control:
//!   no-cache, no-transform`, `Connection: keep-alive`, `X-Accel-Buffering: no`
//! - full listener cleanup on disconnect (broadcast receiver dropped)

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use futures_util::stream::{self, StreamExt as _};
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};

use super::super::server::AppState;
use crate::events::ServerEvent;

/// The reference's defensive concurrent-stream cap.
pub const MAX_STREAMS: usize = 25;

/// The SSE channel names advertised in the `hello` event (reference order).
pub const HELLO_CHANNELS: [&str; 5] = [
    "ratings",
    "sync",
    "conversations",
    "campaigns",
    "notifications",
];

/// GET /api/events — SSE stream for real-time updates.
pub async fn sse_handler(State(state): State<AppState>) -> axum::response::Response {
    // Cap concurrent streams defensively (the reference checks the bus's
    // subscriber count before subscribing; the port checks the receiver
    // count, which includes this handshake window).
    if state.bus.subscriber_count() >= MAX_STREAMS {
        let body = axum::body::Body::from(
            "event: error\ndata: {\"message\":\"Too many event streams open. Close another tab.\"}\n\n",
        );
        return axum::response::Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache, no-transform")
            .header("connection", "keep-alive")
            .header("x-accel-buffering", "no")
            .body(body)
            .expect("building SSE cap response from literals must succeed");
    }

    sse_stream(state)
}

/// The normal (under the cap) SSE stream.
fn sse_stream(state: AppState) -> axum::response::Response {
    let rx = state.bus.subscribe();

    // Serialize each ServerEvent with its reference wire name + payload.
    let event_stream = BroadcastStream::new(rx).filter_map(|result| async move {
        match result {
            Ok(event) => Some(Ok::<_, Infallible>(server_event_to_sse(&event))),
            Err(_lagged) => {
                // The subscriber lagged and dropped events. The reference has
                // no such concept (its bus is sync); surface as a comment so
                // the client knows to refresh.
                Some(Ok(Event::default().comment("lagged")))
            }
        }
    });

    // Interleave heartbeat comments every 25 seconds — the reference writes
    // `: ping\n\n` via setInterval.
    let keep_alive = IntervalStream::new(tokio::time::interval(Duration::from_secs(25)))
        .map(|_| Ok::<_, Infallible>(Event::default().comment("ping")));

    // The `hello` event goes out first (reference sends it immediately on
    // connect, before any bus subscription is registered).
    let hello = stream::once(async {
        Ok::<_, Infallible>(
            Event::default().event("hello").data(
                serde_json::json!({
                    "at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
                    "channels": HELLO_CHANNELS,
                    "version": env!("CARGO_PKG_VERSION"),
                })
                .to_string(),
            ),
        )
    });

    let merged = hello.chain(stream::select(event_stream, keep_alive));

    use axum::response::IntoResponse;
    let mut response = Sse::new(merged)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(25))
                .text("ping"),
        )
        .into_response();
    // The reference sets these exact headers (axum's SSE sets some itself;
    // normalize the rest).
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// Convert a `ServerEvent` into an SSE `Event` with the reference's
/// `event: <name>` + `data: <json>` shape.
fn server_event_to_sse(event: &ServerEvent) -> Event {
    Event::default()
        .event(event.wire_name())
        .data(event.payload_json())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ServerEvent;
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
            bus: EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
        }
    }

    #[tokio::test]
    async fn sse_handler_returns_response() {
        let state = make_state();
        let _response = sse_handler(State(state)).await;
    }

    #[test]
    fn server_event_uses_named_event_field() {
        let e = server_event_to_sse(&ServerEvent::conversation_updated(
            Some(1),
            Some(2),
            None,
            None,
            "webhook",
        ));
        let rendered = format!("{e:?}");
        assert!(
            rendered.contains("conversation"),
            "event name must be set: {rendered}"
        );
    }

    #[tokio::test]
    async fn cap_is_enforced_at_25_streams() {
        let state = make_state();
        // Fill the bus with MAX_STREAMS subscribers.
        let mut keep: Vec<_> = (0..MAX_STREAMS).map(|_| state.bus.subscribe()).collect();
        assert_eq!(state.bus.subscriber_count(), MAX_STREAMS);
        let response = sse_handler(State(state)).await;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("text/event-stream"));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("cap body must be readable");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("event: error"),
            "cap response must be an error event: {text}"
        );
        assert!(text.contains("Too many event streams open"));
        keep.clear();
    }
}
