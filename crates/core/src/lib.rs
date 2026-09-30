//! SupportOS++ core library.
//!
//! This crate contains the pure-Rust business core:
//! - error type + logging setup (foundation, A12)
//! - configuration loader
//! - SQLite connection + migrations runner
//! - job queue
//! - settings store (secrets redacted on read)
//! - the loopback HTTP listener (A2) for webhook + OAuth
//! - traits for `VectorStore`, `LocalAiProvider`, `HelpScoutProvider`
//!
//! The closed-vocabulary catalog lives in the `supportos-plusplus-catalog`
//! crate (WASM-safe, no I/O deps) so the UI and core can share a single
//! source of truth.
//!
//! The Tauri shell (`crates/app`) calls into this crate via Tauri commands.
//! The Leptos UI (`crates/ui`) depends on the catalog crate directly.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all, missing_docs)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    missing_docs
)]

pub use spp_catalog;

pub mod config;
pub mod db;
pub mod error;
pub mod helpscout;
pub mod jobs;
pub mod logging;
pub mod loopback;
pub mod migrations;
pub mod oauth;
pub mod oauth_state;
pub mod runner;
pub mod settings;
pub mod sync;
pub mod webhook;

pub use error::{Error, Result};

/// Convenience re-export so callers can write `spp_core::catalog::Foo` exactly
/// as before the catalog was extracted into its own crate.
pub mod catalog {
    pub use spp_catalog::*;
}
