# plan.md — active TODO list (parity fixes)

Source: code-evidence parity audit of SupportOS-PlusPlus (PORT @ 9bd73ce) vs supportos (MAIN @ c346fb51); report tables T1–T20.

- Population: every feature-matrix item whose status is **Partial (0.5)**, **Divergent (0.5)**, **Stub (0)** or **Missing (0)** — 104 items — plus 15 PORT-side defects from the audit issues register that have no dedicated matrix item (status `Defect`). Total: **119 items** — **116 remaining**.
- Severity: taken from the audit issues register where the item maps to an issue (see *Audit ref*); SEC-02 is Critical per the safety-invariant audit (T11); otherwise Missing/Stub/Divergent → Major and Partial → Minor (Partial with L/XL effort → Major).
- Order: severity first (Blocker → Critical → Major → Minor → Cosmetic), then the audit's blocker order (T17) and fix roadmap (T19), then effort (S < 1d, M < 1w, L < 1mo, XL > 1mo).
- Out of scope for this repo (MAIN-side audit findings, reference only): N9/N10 (MAIN webhook rate-limit exemption ignores querystring), the MAIN half of C1 (bundle schema guard in MAIN), N11 MAIN-side panic containment, K5 (MAIN clean).
- Workflow: one item at a time — plan.md → progress.md → implementation → verification → completed.md (see rule.md).

| # | ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|---|
| 1 | OR-02 | Campaign send path (batch 5, attempts 3, provider.createConversation, sync-back, unknown-state reconcile) | Missing | Blocker | L | B3 | Implement send executor: batch 5, attempts 3, provider.createConversation, sync-back, unknown-state handling |
| 2 | SG-02 | Contact LIKE operators (contains/starts/ends) | Partial | Critical | S | C4 | Use single-backslash ESCAPE like segment.rs:1406; make ids() surface prepare errors |
| 3 | C2 | Swallowed transaction errors in conversation routes report fake success | Defect | Critical | M | T16 C2 | Route conversation mutations through checked transactions and propagate DB errors (crates/core/src/http/routes/conversations.rs:290-308,453-487: `let _ = tx.execute(...)`, `tx.commit().ok()` then ok:true + SSE). |
| 4 | AI-19 | Interaction routes (GET card/evidence/profile, refresh) | Divergent | Critical | M | C7 | Fix GET queries to real schema; create evidence table; implement refresh; serve profile from signals |
| 5 | SEC-02 | HTML sanitizing (40 tags, attr map, schemes, css clip, a-hardening) | Partial | Critical | S | T11 inv.8 | Strip protocol-relative URLs (UrlRelative::Custom deny) ; remove whole img on data:text/html |
| 6 | SY-06 | Real provider wire protocol (inboxId, _links cursor, HAL docs, /v3/system-users, pagination loops) | Divergent | Critical | M | C5 | Use inboxId=, _links.next.href cursor, /v3/system-users, HAL _embedded parsing, page loops for users/tags/orgs/workflows |
| 7 | WK-03 | Job-kind executor coverage (~34 kinds) | Partial | Critical | L | C6 | Add missing job handlers (AI family, bulk ops, attachments, outreach send/reconcile, refresh_report) or stop enqueuing unrunnable kinds |
| 8 | SY-05 | Mirror write fidelity (tags/fields/emails/properties/recipients/ratings) | Divergent | Critical | L | C8 | Persist emails/properties/phones on customer upsert; store thread recipients/attachments |
| 9 | BK-04 | .sosync schema guard (prevent incompatible import) | Divergent | Critical | M | C1 | Real schema fingerprint in header (e.g. canonical DDL hash) checked on import; stop fabricating history |
| 10 | C3 | panic=abort profile + unrecovered panics kill the packaged app | Defect | Critical | M | T16 C3 | Switch release profile to unwind and add catch_unwind containment at handler/task boundary (Cargo.toml:85 panic=abort; operations.rs:276-278 tile panic; rate_limit.rs:69 poisoned-mutex expects). |
| 11 | AI-22 | LM Studio client (models/chat/embeddings, timeout, /v1 normalization) | Partial | Major | S | M10 | Add timeout from lmstudio_timeout_ms; normalize base URL /v1 on all paths |
| 12 | CL-01 | Side threads create (title/team/participants/first_message + 422s) | Partial | Major | S | M14 | Parse full schema; 422 on unknown participants/teams; store title/team/participants/first_message |
| 13 | CL-04 | Side thread participants add | Stub | Major | S | M14 | Insert participants with existence checks (sideThreadRepo.ts:173-186) |
| 14 | AU-03 | Auto-fire on conversation update (workers hook) | Missing | Major | M | - | Hook conversation-updated events to fire triggers (worker side) |
| 15 | AU-04 | Awaiting-approval gating (parked jobs + approve/reject) | Divergent | Major | M | - | Expose approve/reject routes; park via jobs (parity) or rewire tile+sweep to approvals |
| 16 | AU-02 | Trigger vocabulary (new_conversation/customer_reply/ai_low_confidence/manual) | Divergent | Major | L | M27 | Port MAIN trigger/condition/action vocabulary and risk tiers |
| 17 | DB-09 | jobRepo (enqueue/claim/complete/fail backoff min(300,5*2^n)s/park/recover) | Partial | Major | S | M17 | Port exact backoff seconds; add queue filter to claimNext; add max_attempts to enqueue |
| 18 | M15 | Thread delete skips FTS rows and runs outside a transaction | Defect | Major | S | T16 M15 | Delete fts_threads rows for deleted threads inside one transaction (sync_engine.rs:1802-1807 vs MAIN coordinator.ts:698-704). |
| 19 | M20 | SIGTERM not handled (ctrl_c only) | Defect | Major | S | T16 M20 | Handle SIGTERM alongside ctrl_c for graceful shutdown (crates/core/src/http/server.rs:1307-1312 vs MAIN index.ts:88-89). |
| 20 | SY-07 | OAuth (authorize-url, callback, refresh, client-credentials, disconnect, status) | Partial | Major | M | M21 | Implement or remove the Tauri loopback OAuth/webhook receiver (loopback.rs binds then drops; handlers are {ok:true,todo:M2} stubs); delete the dead exchange_code legacy path (oauth.rs:200-247). |
| 21 | TH-09 | bodyLimit 20MB (attachments) | Missing | Major | Add tower_http DefaultBodyLimit(20MB) to router | M19 | DefaultBodyLimit' crates/core/src (0 hits) |
| 22 | OR-03 | Campaign reply tracking (refreshReplies, reply_rate) | Missing | Major | S | M23 | Call reply scan in campaign report (refreshReplies port) |
| 23 | WK-05 | Background automatic AI (process_new_ticket on new/reply) | Missing | Major | M | M11 | Enqueue analyze_ticket on new/reply (sync hook + automation engine) and handle in worker loop |
| 24 | SY-10 | Provider write methods (createConversation, updateTags/Fields, snooze/schedule, runWorkflow, getAttachmentData, ping, routing) | Partial | Major | run_workflow\\ | - | snooze\\ |
| 25 | AI-18 | Interaction forbidden-claim text safety scan | Missing | Major | S | - | Port FORBIDDEN_PATTERNS text scan used on interaction free text |
| 26 | AI-17 | Interaction 2-stage AI enrichment (observe/recommend prompts + safety gates) | Missing | Major | Add 2-stage AI enrichment with enum filter + evidence whitelist + forbidden-claim gates | - | recommendInteraction' crates/core (absent); read ai_prompts.rs:399,420 (unused) |
| 27 | AI-16 | Interaction intelligence engine (observations/baselines/outcomes/recommendations/card/profile) | Partial | Major | XL | - | Port observation inserts, baseline rebuild, outcome/recommendation engines, profile assembly, playbook |
| 28 | DB-05 | Soft-delete + merge semantics (deleted_at filter, resurrect on upsert) | Divergent | Major | Add deleted_at/merged filters to inbox list; resurrect deleted rows on upsert | M18 | merged_into' crates/core/src/inbox.rs (0 hits); read sync.rs:183-200 |
| 29 | SY-09 | Priority API queue (concurrency 2, priority sort) | Missing | Major | Implement priority queue with concurrency 2 wrapping provider calls | M13 | api_queue' crates/core (stats counters only, helpscout_real.rs:279-299) |
| 30 | AI-14 | Report narrative (facts-only prompt) | Divergent | Major | S | - | Route /api/reports/narrative to the existing ai_pipeline implementation |
| 31 | AI-21 | /api/analytics/ai draft stats | Divergent | Major | S | - | Point /api/analytics/ai at the same implementation as /api/ai/analytics |
| 32 | AN-04 | Why-contacting report | Stub | Major | S | M3 | Implement why-contacting computation + MAIN response shape |
| 33 | AN-05 | Top questions report | Stub | Major | S | M3 | Implement top-questions from FTS/knowledge queries |
| 34 | AN-06 | Doc gaps report | Stub | Major | S | M3 | Implement doc-gaps report |
| 35 | AN-07 | Answer reuse report | Missing | Major | S | M3 | Implement answer-reuse candidates with MAIN shape |
| 36 | AN-08 | Issue radar (10 alert kinds) | Divergent | Major | S | M3 | Serve the 11 seeded metric definitions |
| 37 | AN-10 | Release correlation + release events CRUD | Missing | Major | S | M3 | Implement release events write + validation |
| 38 | AN-11 | Help Scout report proxy (4 keys, 404 unknown) | Missing | Major | S | M3 | Proxy the 4 Help Scout report keys via provider; 404 unknown |
| 39 | AN-01 | Dashboard (20 fields incl. by_tag/agent/team/channel/daily/mailbox_comparison/ratings/avg times) | Divergent | Major | M | M1 | Compute all 20 dashboard fields from mirror; accept days + mailboxIds + channel params |
| 40 | AN-09 | Metric definitions endpoint | Missing | Major | M | M3 | Implement release-correlation + release-events CRUD |
| 41 | AN-12 | Report builder run (metrics->SQL, parameterized, dimensions) | Partial | Major | M | M2 | Fix metric SQL to MAIN semantics (published+not-deleted, customer kind, channel via source_type) |
| 42 | AN-15 | Support health (no-score design, metrics+flags+incidents) | Divergent | Major | M | M22 | Replace verdict with MAIN's metrics+flags+incidents model |
| 43 | AC-04 | Events endpoint (actor names, metadata, counts) | Partial | Major | S | - | Join actor names, metadata, counts, limit param |
| 44 | AC-03 | rebuildAll admin action | Stub | Major | M | - | Run the rebuild (or enqueue a real job once workers support it) |
| 45 | DC-01 | Docs channel API (collections/stats/hybrid search/article read) | Stub | Major | M | - | Point docs routes at the synced docs mirror; implement hybrid search + article read |
| 46 | GR-02 | Human edges (5 relations, dup/self/404 checks) | Divergent | Major | M | M4 | Adopt MAIN wire contract + dup/self/404 checks + 5-relation vocab |
| 47 | IS-01 | Cluster list/detail/delete (+conversations) | Divergent | Major | M | - | Serve title/trend/members + conversations in cluster detail |
| 48 | IS-02 | Known issues CRUD (rich fields) + link/unlink | Divergent | Major | M | M5 | Write/serve all MAIN fields |
| 49 | IS-03 | Engineering refs + support cases (from-conversation capture) | Stub | Major | M | - | Implement refs CRUD + support-case capture from conversation |
| 50 | TL-02 | Customer/organization timeline reads (kind counts, filters) | Divergent | Major | M | - | Serve event-kind timeline for customers with filters |
| 51 | TS-03 | Transition history + per-state lifecycle serving | Missing | Major | M | - | Serve transition history + per-state lifecycle in conversation detail |
| 52 | VW-03 | Apply savedViewId/aiAttribute/filter params to inbox list | Missing | Major | M | M7 | Compile saved views + AI-attribute filters into the list query |
| 53 | GR-03 | Neighbors/subgraph(BFS depth<=2)/search/stats | Stub | Major | L | M4 | Implement bounded BFS subgraph, per-kind stats, per-kind capped search |
| 54 | ME-01 | Composed customer memory profile (9 sections + freshness) | Missing | Major | L | M6 | Compose profile at read time (issue history, outcomes, interaction, AI entries, freshness, quarantined) |
| 55 | GR-01 | Derived edge layer (~24 read-time branches) | Missing | Major | XL | M4 | Port read-time derived-edge layer (or populate edges on sync) |
| 56 | UI-17 | Notification center (tabs, filters, prefs, mention queue) | Partial | Major | M | M24 | Wire mark-read/read-all to API; persist prefs via PUT; parse MAIN field names; mention queue; filters |
| 57 | UI-19 | Sync health page (sync actions, checkpoints, health cards, webhook register) | Stub | Major | M | M25 | Port sync actions, checkpoints, runs, health cards, webhook register via API |
| 58 | UI-21 | Onboarding (6-step wizard + live checks) | Divergent | Major | M | - | 6-step wizard with live LM Studio/Qdrant checks |
| 59 | UI-22 | Command palette (live search, 12 hits, keyboard nav) | Stub | Major | M | M9 | Live search via POST /api/search, 200ms debounce, keyboard nav, 12 hits |
| 60 | UI-26 | SSE toasts + cross-page invalidation | Missing | Major | M | - | Toast center for rating/webhook/campaign/critical events; subscribe error |
| 61 | UI-27 | URL-backed state / deep links (view=, days=, doc=, article=, tab=) | Missing | Major | M | - | URL-backed filter state + deep links on dashboard/inbox/knowledge/docs/issues |
| 62 | UI-01 | Dashboard page (ranges, mailbox/channel filters, charts, radar card, KPI links) | Partial | Major | L | - | Ranges, mailbox/channel filters, charts, radar card, linked KPIs, days param |
| 63 | UI-04 | Customers + detail (properties, memories, ratings, interaction profile, health) | Partial | Major | L | - | Properties, memories, ratings, resolutions, interaction profile, support health, clickable conversations, pagination |
| 64 | UI-07 | Issues page (5 tabs: radar/clusters/known/gaps/reuse) | Stub | Major | L | M26 | Port 5 tabs (radar/clusters/known/gaps/reuse) calling existing APIs |
| 65 | UI-14 | Reports page (10 tabs) | Partial | Major | L | - | Port 6 missing tabs (SLA/why-contacting/intelligence/HS reports/definitions/release) |
| 66 | UI-02 | Inbox page (~51 subfeatures: views, filters, bulk, snooze, schedule, attachments, tags/fields editors, AI draft composer, audit, activity) | Partial | Major | XL | M8 | Port ~30 missing subfeatures (views/filterbar/bulk/snooze/schedule/attachments/editors/audit/activity/draft composer/saved replies/cc/bcc/Cmd+Enter/close-confirm/pagination/URL state/assignee picker) |
| 67 | BU-01 | CI pipeline (lint+typecheck+build+test+smoke, Node 20) | Missing | Major | S | M12 | Add CI: fmt+clippy+test+build+smoke on push/PR |
| 68 | AI-23 | AI evaluation mode + golden set | Partial | Major | M | M29 | Create golden_test_set + seed; restore evaluation runs |
| 69 | BU-02 | Desktop release pipeline (win/mac/linux matrix, MSI/NSIS/DMG/AppImage) | Partial | Major | L | - | Add Windows/macOS targets, icons (.ico/.icns), release automation |
| 70 | M16 | Multi-step writes untransacted in side threads / incident features | Defect | Major | M | T16 M16 | Wrap multi-step writes in transactions (side_threads.rs:179-303; intelligence_features.rs:700-830) to avoid partial states. |
| 71 | M28 | run_ai holds the only DB mutex across the LM Studio call | Defect | Major | M | T16 M28 | Move AI runs off the global AppState mutex (spawn_blocking + per-call connection, or apply the LM Studio timeout) in routes/ai.rs:118-140; one hung AI call currently freezes all requests and workers. |
| 72 | DB-01 | 16 forward-only migrations, versioned, transactional | Divergent | Major | L | - | Consolidate boot batches into versioned transactions; stop fabricating MAIN history rows; record real applied versions |
| 73 | CL-08 | Side-thread audit trail on mutations | Missing | Major | S | - | Write audit_log rows on side-thread mutations |
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
