# plan.md — active TODO list (parity fixes)

Source: code-evidence parity audit of SupportOS-PlusPlus (PORT @ 9bd73ce) vs supportos (MAIN @ c346fb51); report tables T1–T20.

- Population: every feature-matrix item whose status is **Partial (0.5)**, **Divergent (0.5)**, **Stub (0)** or **Missing (0)** — 104 items — plus 15 PORT-side defects from the audit issues register that have no dedicated matrix item (status `Defect`). Total: **119 items** — **43 remaining** (count corrected 2026-10-08: the header previously said 38, but the table held 48 rows; the true remaining count is row-verified).
- Severity: taken from the audit issues register where the item maps to an issue (see *Audit ref*); SEC-02 is Critical per the safety-invariant audit (T11); otherwise Missing/Stub/Divergent → Major and Partial → Minor (Partial with L/XL effort → Major).
- Order: severity first (Blocker → Critical → Major → Minor → Cosmetic), then the audit's blocker order (T17) and fix roadmap (T19), then effort (S < 1d, M < 1w, L < 1mo, XL > 1mo).
- Out of scope for this repo (MAIN-side audit findings, reference only): N9/N10 (MAIN webhook rate-limit exemption ignores querystring), the MAIN half of C1 (bundle schema guard in MAIN), N11 MAIN-side panic containment, K5 (MAIN clean).
- Workflow: five items at a time — plan.md → progress.md → implementation → verification → completed.md (see rule.md).

|---|---|---|---|---|---|---|---|
| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
| 74 | DB-12 | syncRepo cursors (getCursor/setCursor page tokens) | Missing | Major | S | - | Implement getCursor/setCursor equivalents used by initial sync page loop |
| 75 | SY-08 | HS rate limiter with persistence (hs_rate_limit) | Divergent | Major | S | - | Persist rate-limit state to a table like MAIN's hs_rate_limit |
| 76 | AI-04 | Draft send provenance (aiDraftId -> was_sent + ai_involvement audit) | Missing | Major | M | - | Accept aiDraftId/originalAiText on reply; mark draft sent; record was_sent feedback + ai_involvement audit |
| 77 | KN-02 | PDF/DOCX ingestion (pdf-parse, mammoth) | Missing | Major | M | - | Add PDF/DOCX parsing (e.g. pdf-extract/lopdf + docx-rs) or document unsupported |
| 78 | SY-11 | Demo simulate endpoints (incoming/rating/webhook) | Divergent | Major | M | - | Route simulate endpoints through fake provider mutation + sync job, persist simulated ratings |
| 79 | DB-02 | 123-table Help Scout mirror schema | Partial | Major | - | - | Complete the 123-table Help Scout mirror schema (add missing tables), keeping documented renames (threads->conversation_threads, segments->saved_segments, support_graph_edges->graph_edges, customer_memories->customer_memory, conversation_events->activity_events, knowledge_candidates->knowledge_gap_candidates). |
| 80 | DB-04 | threads actor model (user/customer/system split) | Divergent | Major | - | - | Restore MAIN's threads actor model (created_by_user/customer/system 3-way split) instead of the collapsed actor_id+actor_type, or map it at query boundaries. |
| 81 | DB-06 | FK enforcement + pragma parity (WAL, foreign_keys, busy_timeout) | Partial | Major | - | - | Declare foreign keys on base mirror tables (currently zero FKs) to match MAIN's enforcement; keep pragma parity (WAL, foreign_keys, busy_timeout). |
| 82 | OP-04 | Automation-approvals tile (parked jobs count) | Divergent | Major | - | - | Automation-approvals operations tile always returns 0; count parked/awaiting-approval jobs (automation_approvals) as MAIN's tile does. |
| 83 | DB-03 | conversations table shape (number UNIQUE, FK names, 47 cols) | Divergent | Major | L | - | Restore MAIN column names + UNIQUE(number) via new migration; rewrite ported SQL |
| 84 | UI-25 | Modal system (Escape stack, focus trap, backdrop) | Partial | Minor | S | - | Escape-stack close, focus trap, backdrop click |
| 85 | UI-03 | Search page (7 scopes, filters, semantic toggle, clickable hits) | Partial | Minor | M | - | 7 scopes, filters, semantic notices, <mark> snippets, clickable hits |
| 86 | UI-05 | Organizations + detail (+health/timeline) | Partial | Minor | M | - | Health/timeline sections, pagination |
| 87 | UI-11 | Graph explorer (stats, search, neighbors, human edges) | Partial | Minor | M | - | Stats card, node search, neighbor explorer with origin badges, human-edge assert/remove |
| 88 | UI-12 | Knowledge page (4 tabs, doc reader, deep links, freshness chips) | Partial | Minor | M | - | Doc reader modal + ?doc= deep links + freshness flag chips + recurring-questions card |
| 89 | UI-13 | Docs page (hybrid search, filters, reader, channel mix) | Partial | Minor | M | - | Hybrid search + filters + reader + ?article= links; align payload parsing |
| 90 | UI-16 | Operations page (tiles + workload tab + capacity editor) | Partial | Minor | M | - | Workload tab, capacity editor, suggested assignees; fix dead drill links (inbox must read view param) |
| 91 | UI-18 | Automation page (rules CRUD + runs) | Partial | Minor | M | - | Add rule-creation form with MAIN's trigger/condition/action schema |
| 92 | UI-28 | ErrorBoundary + RelativeTime + a11y (tabIndex, Enter/Space rows) | Partial | Minor | M | - | Global error boundary, relative-time component, keyboard-accessible rows |
| 93 | BU-05 | Operational scripts (migrate/backup/restore/seed/healthcheck CLIs) | Missing | Minor | M | N6 | Add migrate/backup/restore/seed/healthcheck subcommands |
| 94 | BK-02 | export-json (7 tables) / export-csv (8 cols) | Partial | Minor | S | - | Match MAIN column sets + deleted_at filters; add tests for create/restore/prune |
| 95 | CL-05 | Resolve/reopen (409 on bad state, status+updated_at) | Partial | Minor | S | - | Update status/updated_at; 409 on invalid transition |
| 96 | N2 | Remote id stored into local FK column on lookup miss | Defect | Minor | S | T16 N2 | Resolve people lookups to local ids or fail loudly instead of writing remote ids into FK columns (sync.rs:211,213). |
| 97 | N3 | export_db dumps oauth_tokens in plaintext (dead path) | Defect | Minor | S | T16 N3 | Remove or redact oauth_tokens in data_tools.rs:261-303 export (latent secret leak if ever wired). |
| 98 | N4 | do_not_contact table copy->drop->rename on every boot | Defect | Minor | S | T16 N4 | Replace boot churn with CREATE IF NOT EXISTS + guarded ALTER (outreach.rs:461-470). |
| 99 | N5 | ensure_pipeline_schema(&conn).ok() swallows init failures before reads | Defect | Minor | S | T16 N5 | Log and propagate schema-init failures in routes/ai.rs:390,448 instead of serving empty data. |
| 100 | N7 | RecordingEmitter (test-only) compiled into production | Defect | Minor | S | T16 N7 | Gate events.rs:289-342 RecordingEmitter behind #[cfg(test)]. |
| 101 | DB-11 | conversationRepo (upsert + 8 sync-diff events + tags/fields tx + FTS) | Partial | Minor | M | - | Emit all 8 sync-diff event kinds; write thread participants/recipients |
| 102 | EV-02 | Event vocabulary (ratings, sync, conversation, campaign, notification + error) | Partial | Minor | M | - | Subscribe to error event; port toast center; add cross-page invalidation |
| 103 | KN-03 | Freshness engine (6 flags, configurable thresholds, usage tracking) | Partial | Minor | M | - | Add usage recording + conflict/question correlation + configurable thresholds |
| 104 | OR-06 | Campaign reconcile (unknown recipients, stale sending requeue) | Partial | Minor | M | - | Use provider.listConversations when real provider active |
| 105 | TH-05 | Malformed JSON -> 400 (raw-body parser) | Partial | Minor | M | - | Replace Option<Json<Value>> with Json<T> + rejection mapper returning MAIN's 400 envelope; add a shared extractor |
| 106 | VT-01 | Qdrant vector store (collection supportos_vectors, payload schema, dims) | Partial | Minor | M | - | Document stack deviation or add external-Qdrant client mode for portability |
| 107 | WK-04 | Boot backfills (v1.5.0 activity, v1.9.0) | Partial | Minor | M | - | Port v1.5.0/v1.9.0 backfills (or document fresh-install-only) |
| 108 | BK-05 | Encrypted upload route (512MB, magic check) | Partial | Minor | - | - | Verify and match MAIN's encrypted upload route semantics (512MB cap, magic-byte check) and add tests; route exists but size/jail semantics were unverified in the audit. |
| 109 | BU-04 | Tauri packaging config (CSP, capabilities, window) | Partial | Minor | - | - | Extend Tauri packaging config toward MAIN's matrix (bundle targets, .ico/.icns icons); the app crate could not be compiled in the audit env (webkit missing), so verify in a capable build env. |
| 110 | DB-08 | Index coverage (~100 named indexes) | Partial | Minor | - | - | Complete 1:1 index coverage vs MAIN's ~100 named indexes (audit diff incomplete; derive from MAIN migration DDL). |
| 111 | TH-06 | Zod-equivalent validation -> 422 envelope | Partial | Minor | - | - | Add a systematic request-schema validation layer equivalent to MAIN's Zod schemas (shared extractors returning MAIN's 422 envelope with issue list); today only per-route field checks exist. Stack-adapted item. |
| 112 | TH-07 | Error envelope (statusCode/error/message/detail) + >=500 logging to application_errors | Partial | Minor | - | - | Add a global error handler producing MAIN's {statusCode,error,message,detail} envelope, log >=500 errors to application_errors, remove remaining ok:true-on-error paths. |
| 113 | K1 | Cosmetic string drift: provider none vs disabled; missing paren in string | Defect | Cosmetic | S | T16 K1 | Align provider label strings and fix outreach.rs:1221 missing paren (routes/ai.rs, outreach.rs). |
| 114 | K2 | base64_encode hex-encodes; dead marker fn in webhook.rs | Defect | Cosmetic | S | T16 K2 | Rename/remove misleading helpers (data_tools.rs:324-327; webhook.rs:94-95). |
| 115 | K3 | Port-only legal footer text | Defect | Cosmetic | S | T16 K3 | Decide keep/remove the layout.rs:127-132 legal footer (port-only addition). |
| 116 | K4 | Field naming drift (SSE hello version, dashboard daysBack) | Defect | Cosmetic | S | T16 K4 | Align SSE hello version (0.1.0 vs 2.2.1) and dashboard daysBack param naming with MAIN (ui/sse.rs; hooks.ts). |
