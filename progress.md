# progress.md — current work item

This file must contain **only the single item currently being worked on** (see `rule.md`).

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 3 | C2 | Swallowed transaction errors in conversation routes report fake success | Defect | Critical | M | T16 C2 | Route conversation mutations through checked transactions and propagate DB errors (crates/core/src/http/routes/conversations.rs:290-308,453-487: `let _ = tx.execute(...)`, `tx.commit().ok()` then ok:true + SSE). |
