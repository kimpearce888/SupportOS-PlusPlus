//! SSE events route — real-time push over HTTP.
//!
//! Mirrors: src/server/routes/events.ts

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::extract::State;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

use super::super::server::AppState;

/// GET /api/events — SSE stream for real-time updates.
pub async fn sse_handler(State(_state): State<AppState>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let interval = tokio::time::interval(Duration::from_secs(25));
    let stream = IntervalStream::new(interval)
        .map(|_| Event::default().comment("keep-alive"))
        .map(Ok);

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(25)))
}
