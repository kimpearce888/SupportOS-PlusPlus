//! Closed-vocabulary catalog (D-008).
//!
//! Every closed vocabulary in SupportOS++ lives here as a single Rust enum.
//! The DB stores the enum's discriminant as TEXT; validation, FTS, UI, and tests all derive from these enums.
//! Counts verified against the reference repo in session 1 — see `docs/PARITY-MATRIX.md`.

// This module is the canonical source. Sub-modules below contain the actual enums.

pub mod activity;
pub mod attributes;
pub mod graph;
pub mod notifications;
pub mod operations;
pub mod reporting;
pub mod workspace;

// Re-export the most-used items at the catalog root for ergonomics.
pub use activity::{ActivityField, ConditionKind, DateMode, ResponseState};
pub use attributes::AiAttributeKey;
pub use graph::GraphNodeKind;
pub use notifications::NotificationType;
pub use operations::OperationsTileKey;
pub use reporting::{ReportDimensionKey, ReportMetricKey};
