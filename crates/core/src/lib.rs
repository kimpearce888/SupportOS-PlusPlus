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

pub mod activity;
pub mod ai_analysis;
pub mod ai_center;
pub mod ai_features;
pub mod ai_lm_studio;
pub mod ai_provider;
pub mod audit;
pub mod automation;
pub mod backup;
pub mod backup_service;
pub mod config;
pub mod conformance;
pub mod copilot;
pub mod customers;
pub mod data_tools;
pub mod db;
pub mod demo;
pub mod embeddings;
pub mod encrypted_sync;
pub mod error;
pub mod events;
pub mod helpscout;
pub mod helpscout_real;
pub mod http;
pub mod hybrid_search;
pub mod inbox;
pub mod intelligence;
pub mod intelligence_features;
pub mod jobs;
pub mod logging;
pub mod loopback;
pub mod mentions;
pub mod migrations;
pub mod mirror_readouts;
pub mod notification_prefs;
pub mod notification_sweep;
pub mod notifications;
pub mod oauth;
pub mod oauth_state;
pub mod operations;
pub mod outreach;
pub mod perf_guards;
pub mod reports;
pub mod response_state_sql;
pub mod runner;
pub mod saved_views;
pub mod search;
pub mod self_check;
pub mod settings;
pub mod side_threads;
pub mod sync;
pub mod sync_engine;
pub mod sync_schema;
pub mod ticket_ops;
pub mod ticket_states;
pub mod vectorstore;
pub mod vectorstore_contract;
#[cfg(feature = "qdrant")]
pub mod vectorstore_qdrant;
pub mod webhook;
pub mod webhook_handler;
pub mod workload;

pub use error::{Error, Result};

/// Convenience re-export so callers can write `spp_core::catalog::Foo` exactly
/// as before the catalog was extracted into its own crate.
pub mod catalog {
    pub use spp_catalog::*;
}

#[cfg(test)]
mod cross_compat_test;
