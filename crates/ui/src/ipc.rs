//! Tauri IPC bridge — WASM-side wrappers for invoking Tauri commands.
//!
//! Per spec A2: "Everything else uses Tauri IPC commands and events."
//! The WASM frontend calls these functions to invoke Rust-side commands.
//! In the browser (without Tauri), these gracefully return errors.

use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// Invoke a Tauri IPC command by name with arguments.
/// In a Tauri webview, this calls `window.__TAURI__.invoke()`.
/// In a plain browser (no Tauri), this returns an error.
pub async fn invoke<R: DeserializeOwned>(cmd: &str, args: &impl Serialize) -> Result<R, String> {
    // Serialize args to a JS object.
    let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
    let args_val = js_sys::JSON::parse(&args_json).map_err(|e| format!("parse error: {e:?}"))?;

    // Get window.__TAURI__.invoke
    let window = web_sys::window().ok_or("no window")?;
    let tauri_obj = js_sys::Reflect::get(&window, &"__TAURI__".into())
        .map_err(|e| format!("no __TAURI__: {e:?}"))?;
    let invoke_fn = js_sys::Reflect::get(&tauri_obj, &"invoke".into())
        .map_err(|e| format!("no invoke: {e:?}"))?;
    let invoke_fn: js_sys::Function = invoke_fn
        .dyn_into()
        .map_err(|_| "invoke is not a function".to_string())?;

    // Call invoke(cmd, args) → returns a Promise.
    let promise = invoke_fn
        .call2(&tauri_obj, &cmd.into(), &args_val)
        .map_err(|e| format!("invoke call failed: {e:?}"))?;

    // Await the promise.
    let result = JsFuture::from(js_sys::Promise::from(promise))
        .await
        .map_err(|e| format!("invoke promise rejected: {e:?}"))?;

    // Convert the JS result back to Rust.
    let result_str = js_sys::JSON::stringify(&result)
        .map_err(|e| format!("stringify failed: {e:?}"))?
        .as_string()
        .ok_or("result is not a string")?;

    serde_json::from_str(&result_str).map_err(|e| format!("deserialization failed: {e}"))
}
