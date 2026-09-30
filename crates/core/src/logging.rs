//! Logging setup for SupportOS++.
//!
//! `tracing` is the only logger. JSON in production, pretty in dev.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// - In `--release` or when `RUST_LOG=json`: emits JSON to stdout.
/// - In dev: emits pretty, coloured output to stderr.
///
/// Safe to call only once per process. Subsequent calls are no-ops.
pub fn init() {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,spp_core=debug"));

    let is_json =
        cfg!(not(debug_assertions)) || std::env::var("SPP_LOG_FORMAT").as_deref() == Ok("json");

    let registry = tracing_subscriber::registry().with(env_filter);

    if is_json {
        registry.with(fmt::layer().json()).try_init().ok();
    } else {
        registry
            .with(fmt::layer().with_target(true).pretty())
            .try_init()
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init();
        init(); // must not panic
    }
}
