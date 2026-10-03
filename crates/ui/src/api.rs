//! HTTP API client — the WASM-side counterpart of the reference's
//! `src/client/api/client.ts` (`api.get`).
//!
//! Pages owned by the Tauri shell fetch data over IPC (`crate::ipc`), but the
//! app-shell wiring (onboarding guard, nav badge counts, the API-backed
//! Directory/Intelligence pages) talks to the HTTP API exactly like the
//! reference does. The UI bundle is served from the Tauri asset origin (or
//! the Trunk dev server on :1420), NOT from the Axum API server, so requests
//! use an absolute base URL — the same rule `sse.rs` applies to the
//! EventSource: `http://127.0.0.1:3000` by default, overridable via
//! `localStorage['spp.api_base']` (used when the server runs on a
//! non-default port).

use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// The absolute API base URL.
///
/// Default `http://127.0.0.1:3000` (the loopback server D-002); override
/// with `localStorage['spp.api_base']`.
pub fn api_base() -> String {
    // Native (test) builds have no window — js-sys statics panic off-wasm,
    // so the localStorage override is only read in the browser.
    #[cfg(target_arch = "wasm32")]
    if let Some(w) = web_sys::window() {
        if let Ok(Some(v)) = w.local_storage() {
            if let Ok(Some(base)) = v.get_item("spp.api_base") {
                return base.trim_end_matches('/').to_string();
            }
        }
    }
    "http://127.0.0.1:3000".to_string()
}

/// The absolute URL for an API path. `path` must start with `/`
/// (e.g. `/api/onboarding`).
#[must_use]
pub fn url_for(path: &str) -> String {
    format!("{}{}", api_base(), path)
}

/// GET `path` and parse the JSON body.
///
/// Errors carry the path and HTTP status so callers can render an honest
/// error state (per KNOWN PITFALLS: every view has an error state).
pub async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let window = web_sys::window().ok_or("no window")?;
    let url = url_for(path);
    let response = JsFuture::from(window.fetch_with_str(&url))
        .await
        .map_err(|e| format!("GET {path} failed: {e:?}"))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| format!("GET {path}: fetch did not return a Response"))?;
    if !response.ok() {
        return Err(format!("GET {path} -> HTTP {}", response.status()));
    }
    let text_promise = response
        .text()
        .map_err(|e| format!("GET {path}: body read failed: {e:?}"))?;
    let text = JsFuture::from(text_promise)
        .await
        .map_err(|e| format!("GET {path}: body read failed: {e:?}"))?
        .as_string()
        .ok_or_else(|| format!("GET {path}: body is not UTF-8 text"))?;
    serde_json::from_str(&text).map_err(|e| format!("GET {path}: invalid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_for_joins_base_and_path() {
        let url = url_for("/api/onboarding");
        assert!(url.ends_with("/api/onboarding"));
        assert!(
            url.starts_with("http://"),
            "url must be absolute (asset origin differs from API origin)"
        );
        assert!(!url.contains("//api"), "no double slash: {url}");
    }

    #[test]
    fn url_for_preserves_query_strings() {
        let url = url_for("/api/conversations?view=active&pageSize=1");
        assert!(url.ends_with("/api/conversations?view=active&pageSize=1"));
    }
}
