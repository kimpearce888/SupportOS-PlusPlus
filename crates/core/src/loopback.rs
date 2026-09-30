//! Loopback HTTP listener (A2, D-002).
//!
//! Exactly ONE listener bound to 127.0.0.1, serving only two routes:
//!   - `POST /webhooks/helpscout` — webhook receiver (HMAC-SHA1 verified, persist-first, dedup).
//!   - `GET  /oauth/callback`     — OAuth redirect receiver (single-use state).
//!
//! Everything else uses Tauri IPC. No local web API for the UI.
//!
//! This module is the foundation: route handlers and the full HMAC/dedup pipeline are added in M2.
//! For now, the listener boots and 404s everything else.

use std::net::SocketAddr;

use axum::{
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Router,
};
use tokio::net::TcpListener;

use crate::error::Result;

/// The loopback listener. Owns the bound socket address.
pub struct Loopback {
    addr: SocketAddr,
}

impl Loopback {
    /// Bind to a random port on 127.0.0.1. Returns the listener; call `.serve()` to run.
    pub async fn bind(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        drop(listener);
        Ok(Self { addr })
    }

    /// The address actually bound. Persist this in settings so the OAuth redirect URI is stable.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Run the server forever (until the process is killed).
    ///
    /// Routes:
    ///   - `GET  /oauth/callback`           → 200 JSON `{ "ok": true }` (full handler lands in M2)
    ///   - `POST /webhooks/helpscout`       → 200 JSON `{ "ok": true }` (full handler lands in M2)
    ///   - everything else                  → 404 JSON `{ "error": "not found" }`
    pub async fn serve(self) -> std::io::Result<()> {
        let app = Router::new()
            .route("/oauth/callback", get(oauth_callback))
            .route("/webhooks/helpscout", post(webhook_helpscout))
            .fallback(any(not_found));

        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!(addr = %self.addr, "loopback listener bound");
        axum::serve(listener, app).await?;
        Ok(())
    }
}

/// JSON body for a route that's still a stub.
fn json_ok(route: &str, todo: &str) -> Response {
    let body = format!(r#"{{"ok":true,"route":"{route}","todo":"{todo}"}}"#);
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// 404 JSON body.
fn json_404() -> Response {
    let body = r#"{"error":"not found"}"#;
    (
        axum::http::StatusCode::NOT_FOUND,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn oauth_callback() -> Response {
    // M2: verify single-use state, exchange code for token, persist, redirect to app.
    json_ok("oauth_callback", "M2")
}

async fn webhook_helpscout() -> Response {
    // M2: Host-header validation + rate limit + timing-safe HMAC-SHA1 + persist-first + dedup.
    json_ok("webhook_helpscout", "M2")
}

async fn not_found() -> Response {
    json_404()
}

/// Smoke test helper: bind and immediately drop. Returns Ok on success.
pub async fn smoke_bind() -> Result<()> {
    let _l = Loopback::bind(0).await.map_err(crate::error::Error::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_to_loopback_only() {
        let l = Loopback::bind(0).await.unwrap();
        assert_eq!(l.local_addr().ip().to_string(), "127.0.0.1");
    }

    #[tokio::test]
    async fn smoke_bind_succeeds() {
        smoke_bind().await.unwrap();
    }
}
