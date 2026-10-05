# progress.md — current work item

This file must contain **only the single item currently being worked on** (see `rule.md`).

## In progress

| ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|
| SG-02 | Contact LIKE operators (contains/starts/ends) | Partial | Critical | S | C4 | Use single-backslash ESCAPE like segment.rs:1406; make ids() surface prepare errors |

**Scope of the fix (started 2026-10-06 UTC):**

1. Fix the broken Contact LIKE operators on the `name` / `organization` / `job_title` / `location` / `background` fields (segment.rs lines 1660, 1664, 1668 — currently `ESCAPE '\\\\'` in Rust source = `ESCAPE '\\'` (two chars) in SQL, which SQLite rejects because ESCAPE expects a single character). Replace with `ESCAPE '\\'` in Rust source = `ESCAPE '\'` (single backslash) in SQL — matching the correct pattern at segment.rs:1406.
2. Make `SegmentEngine::ids()` (segment.rs:777) surface prepare errors via `tracing::warn!` instead of silently returning an empty `Vec<i64>`. Currently a malformed SQL string (the bug above is exactly such a case) returns no rows with zero diagnostics — the user sees an empty segment preview and never knows the SQL was rejected. The fix logs the prepare error so it shows up in the application log; same for query_map errors.
3. Add unit tests covering all three previously-broken Contact LIKE operators (`contains` / `starts_with` / `ends_with`) on the `name` field — the audit's exact gap (no test exercised these paths, which is why the bug shipped). Plus a test that asserts `ids()` logs a warning when handed deliberately-malformed SQL (the prepare-error surfacing).
