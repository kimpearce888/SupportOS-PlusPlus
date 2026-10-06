# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 9 | BK-04 | .sosync schema guard (prevent incompatible import) | Divergent | Critical | M | C1 | Real schema fingerprint in header (e.g. canonical DDL hash) checked on import; stop fabricating history |
| 10 | C3 | panic=abort profile + unrecovered panics kill the packaged app | Defect | Critical | M | T16 C3 | Switch release profile to unwind and add catch_unwind containment at handler/task boundary (Cargo.toml:85 panic=abort; operations.rs:276-278 tile panic; rate_limit.rs:69 poisoned-mutex expects). |
| 11 | AI-22 | LM Studio client (models/chat/embeddings, timeout, /v1 normalization) | Partial | Major | S | M10 | Add timeout from lmstudio_timeout_ms; normalize base URL /v1 on all paths |
| 12 | CL-01 | Side threads create (title/team/participants/first_message + 422s) | Partial | Major | S | M14 | Parse full schema; 422 on unknown participants/teams; store title/team/participants/first_message |
| 13 | CL-04 | Side thread participants add | Stub | Major | S | M14 | Insert participants with existence checks (sideThreadRepo.ts:173-186) |
