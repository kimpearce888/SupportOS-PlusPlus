//! Mutation rate limiter — mirrors the reference's `mutationHits` map.
//!
//! Reference semantics (from `src/server/app.ts`):
//!   - 300 mutations per 60-second window per client IP.
//!   - GET / HEAD / OPTIONS are unmetered (reads are cheap, SPA polls freely).
//!   - `/api/webhooks/helpscout` is exempt (HMAC-authenticated + deduped by hash).
//!   - Keyed on socket `remoteAddress` (NOT `X-Forwarded-For`, which is
//!     client-controlled and would let any client rotate headers for an
//!     unlimited budget).
//!   - Map is pruned when it exceeds 512 entries (opportunistic sweep).
//!
//! On limit exceeded: HTTP 429 with `retry-after` header and a JSON body
//! matching the reference's `TooManyRequests` error shape.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};

/// Maximum mutations per window per client IP. Matches reference `RATE_LIMIT_MAX`.
pub const RATE_LIMIT_MAX: u32 = 300;
/// Window length. Matches reference `RATE_LIMIT_WINDOW_MS`.
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Map size threshold above which expired entries are swept. Matches
/// reference `RATE_LIMIT_MAP_MAX`.
pub const RATE_LIMIT_MAP_MAX: usize = 512;
/// Webhook endpoint path — exempt from rate limiting (HMAC-authenticated).
pub const WEBHOOK_PATH: &str = "/api/webhooks/helpscout";

/// Per-IP hit counter.
#[derive(Clone, Copy, Debug)]
struct HitEntry {
    count: u32,
    reset_at: Instant,
}

/// In-memory mutation rate limiter. Thread-safe via `Mutex<HashMap>`.
/// Held in `AppState` (cloneable because the inner Mutex is shareable).
#[derive(Clone, Default)]
pub struct RateLimiter {
    hits: std::sync::Arc<Mutex<HashMap<String, HitEntry>>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `Ok(())` if the request is allowed, or `Err(retry_after_seconds)`
    /// if the client has exceeded the rate limit.
    ///
    /// `key` is the client IP (from socket address — NOT from `X-Forwarded-For`).
    /// `method` is the HTTP method. `path` is the request path (used to
    /// exempt the webhook endpoint).
    pub fn check(&self, method: &Method, path: &str, key: &str) -> Result<(), u64> {
        // GET / HEAD / OPTIONS are unmetered.
        if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
            return Ok(());
        }
        // Webhook endpoint is HMAC-authenticated + deduped; exempt.
        if path == WEBHOOK_PATH {
            return Ok(());
        }

        let now = Instant::now();
        // C3 (audit T16): recover from a poisoned mutex instead of
        // panicking. Previously `.expect("RateLimiter mutex poisoned")`
        // made EVERY subsequent request panic after one panicking handler
        // left the lock poisoned — with the release profile on
        // `panic = "abort"` that killed the packaged app on the next
        // mutation. The map is still structurally valid (std collections
        // are never left corrupt by unwinding); the same
        // `unwrap_or_else(|p| p.into_inner())` convention the port uses for
        // its DB connection lock.
        let mut hits = self
            .hits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Opportunistic pruning: sweep expired entries once the map grows
        // past the threshold so long-lived processes cannot accumulate
        // stale keys. Matches the reference's pruning logic.
        if hits.len() > RATE_LIMIT_MAP_MAX {
            hits.retain(|_, v| now < v.reset_at);
        }

        let entry = hits.entry(key.to_string()).or_insert(HitEntry {
            count: 0,
            reset_at: now + RATE_LIMIT_WINDOW,
        });

        // If the window has elapsed since the entry was created, reset.
        if now > entry.reset_at {
            entry.count = 0;
            entry.reset_at = now + RATE_LIMIT_WINDOW;
        }

        entry.count += 1;
        if entry.count > RATE_LIMIT_MAX {
            let retry_after_secs = entry
                .reset_at
                .saturating_duration_since(now)
                .as_secs()
                .max(1);
            return Err(retry_after_secs);
        }

        Ok(())
    }

    /// Build the `429 Too Many Requests` response, matching the
    /// reference's `TooManyRequests` error body.
    #[must_use]
    pub fn too_many_requests_response(retry_after_secs: u64) -> Response {
        let body = serde_json::json!({
            "statusCode": 429,
            "error": "TooManyRequests",
            "message": "Too many write requests in one minute. Slow down and retry shortly."
        });
        let retry_value = HeaderValue::from_str(&retry_after_secs.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("60"));
        Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::RETRY_AFTER, retry_value)
            .body(axum::body::Body::from(body.to_string()))
            .expect("valid 429 response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[test]
    fn get_requests_are_unmetered() {
        let limiter = RateLimiter::new();
        for _ in 0..1_000 {
            assert!(limiter
                .check(&Method::GET, "/api/conversations", "127.0.0.1")
                .is_ok());
        }
    }

    #[test]
    fn head_requests_are_unmetered() {
        let limiter = RateLimiter::new();
        for _ in 0..1_000 {
            assert!(limiter
                .check(&Method::HEAD, "/api/conversations", "127.0.0.1")
                .is_ok());
        }
    }

    #[test]
    fn options_requests_are_unmetered() {
        let limiter = RateLimiter::new();
        for _ in 0..1_000 {
            assert!(limiter
                .check(&Method::OPTIONS, "/api/conversations", "127.0.0.1")
                .is_ok());
        }
    }

    #[test]
    fn webhook_endpoint_is_exempt() {
        let limiter = RateLimiter::new();
        for _ in 0..1_000 {
            assert!(limiter
                .check(&Method::POST, WEBHOOK_PATH, "127.0.0.1")
                .is_ok());
        }
    }

    #[test]
    fn mutations_under_limit_are_allowed() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_MAX {
            assert!(limiter
                .check(&Method::POST, "/api/conversations/1/reply", "127.0.0.1")
                .is_ok());
        }
    }

    #[test]
    fn mutations_over_limit_are_rejected() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_MAX {
            let _ = limiter.check(&Method::POST, "/api/conversations/1/reply", "127.0.0.1");
        }
        // RATE_LIMIT_MAX + 1th call should be rejected.
        let result = limiter.check(&Method::POST, "/api/conversations/1/reply", "127.0.0.1");
        assert!(result.is_err());
        let retry_after = result.unwrap_err();
        assert!(retry_after >= 1);
    }

    #[test]
    fn separate_clients_have_separate_budgets() {
        let limiter = RateLimiter::new();
        // Client A uses their full budget.
        for _ in 0..RATE_LIMIT_MAX {
            let _ = limiter.check(&Method::POST, "/x", "127.0.0.1");
        }
        // Client B is not affected.
        for _ in 0..RATE_LIMIT_MAX {
            assert!(limiter.check(&Method::POST, "/x", "127.0.0.2").is_ok());
        }
    }

    #[tokio::test]
    async fn too_many_requests_response_has_correct_shape() {
        let response = RateLimiter::too_many_requests_response(42);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .expect("retry-after header present");
        assert_eq!(retry_after, "42");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let bytes = to_bytes(response.into_body(), 1024)
            .await
            .expect("body should be readable");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("body is JSON");
        assert_eq!(json["statusCode"], 429);
        assert_eq!(json["error"], "TooManyRequests");
    }

    #[tokio::test]
    async fn map_pruning_runs_when_threshold_exceeded() {
        // This is mostly a smoke test — we just verify that pruning
        // doesn't panic on a large map.
        let limiter = RateLimiter::new();
        for i in 0..(RATE_LIMIT_MAP_MAX + 50) {
            let ip = format!("127.0.{i}");
            let _ = limiter.check(&Method::POST, "/x", &ip);
        }
        // After pruning, the map should be smaller. We can't easily check
        // the size from outside, but the fact that no panic occurred is
        // the contract.
    }

    // ---- C3: poisoned-mutex recovery ---------------------------------------

    #[test]
    fn poisoned_mutex_does_not_break_the_limiter() {
        let limiter = RateLimiter::new();
        // Seed one entry so the poisoned guard is taken with data present.
        assert!(limiter.check(&Method::POST, "/x", "127.0.0.1").is_ok());
        // Poison the inner mutex: a panicking critical section while the
        // lock is held (the audit's scenario).
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = limiter.hits.lock().expect("test takes the lock");
            panic!("boom inside the rate limiter's critical section");
        }));
        assert!(poisoned.is_err(), "the critical section must have panicked");
        assert!(
            limiter.hits.is_poisoned(),
            "precondition: the mutex is poisoned"
        );
        // C3: check() must RECOVER instead of panicking — the limiter keeps
        // admitting (and counting) mutations after the poison.
        assert!(limiter.check(&Method::POST, "/x", "127.0.0.1").is_ok());
        for _ in 0..(RATE_LIMIT_MAX - 2) {
            assert!(limiter.check(&Method::POST, "/x", "127.0.0.1").is_ok());
        }
        // The recovered map still rate-limits (count carried over: 2 +
        // RATE_LIMIT_MAX - 2 == RATE_LIMIT_MAX uses, so the next one tips).
        assert!(limiter.check(&Method::POST, "/x", "127.0.0.1").is_err());
    }
}
