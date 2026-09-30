//! `supportos-plusplus-xtask` — shared library used by both the `xtask` and
//! `audit` binaries.
//!
//! Putting the shared logic (discover, verify_config, the audit-check
//! primitives) in a lib avoids code duplication between the two binaries
//! (A12: never write the same logic twice).

pub mod discover;
pub mod verify_config;
