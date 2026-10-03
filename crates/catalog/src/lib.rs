//! SupportOS++ catalog — the closed-vocabulary source of truth.
//!
//! Per spec amendment A12 / D-008: every closed vocabulary (condition kinds,
//! tiles, notification types, metrics, dimensions, attribute keys, graph node
//! kinds, Copilot tools, etc.) is a single Rust enum. The DB stores the enum's
//! discriminant as TEXT; validation, FTS, UI, and tests all derive from these
//! enums.
//!
//! This crate is intentionally minimal (no I/O deps) so it compiles to both
//! native Rust and WASM, letting the UI and the core share the same source of
//! truth.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![warn(clippy::all, missing_docs)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    missing_docs
)]

pub mod activity;
pub mod attributes;
pub mod copilot;
pub mod graph;
pub mod notifications;
pub mod operations;
pub mod reporting;
pub mod workspace;

// Re-export the most-used items at the catalog root for ergonomics.
pub use activity::{ActivityField, ConditionKind, DateMode, ResponseState};
pub use attributes::{AiAttributeKey, AttributeValueType, AI_ATTRIBUTE_SCHEMA_VERSION};
pub use copilot::CopilotTool;
pub use graph::GraphNodeKind;
pub use notifications::NotificationType;
pub use operations::OperationsTileKey;
pub use reporting::{ReportDimensionKey, ReportMetricKey};
pub use workspace::{
    ConnectorAuthMode, ConnectorKind, CustomFieldType, IncidentSeverity, IncidentSource,
    IncidentStatus,
};
