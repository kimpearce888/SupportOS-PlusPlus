//! SupportOS++ core library.
//!
//! This crate contains the pure-Rust business core:
//! - error type + logging setup (foundation, A12)
//! - configuration loader
//! - SQLite connection + migrations runner
//! - job queue
//! - settings store (secrets redacted on read)
//! - the loopback HTTP listener (A2) for webhook + OAuth
//! - the closed-vocabulary catalog (D-008)
//! - traits for `VectorStore`, `LocalAiProvider`, `HelpScoutProvider`
//!
//! The Tauri shell (`crates/app`) calls into this crate via Tauri commands.
//! The Leptos UI (`crates/ui`) depends on this crate for shared types only.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all, missing_docs)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    missing_docs
)]

pub mod catalog;
pub mod config;
pub mod db;
pub mod error;
pub mod jobs;
pub mod logging;
pub mod loopback;
pub mod migrations;
pub mod settings;

pub use error::{Error, Result};
