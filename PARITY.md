# PARITY.md — SupportOS++ Strict Reference Parity Audit (Canonical Record)

> This is the canonical audit record. It supersedes every earlier parity/audit
> document in this repository. Statuses below were **recalculated from current
> source** (reference HEAD `c346fb5`, port HEAD `ac80872`) — not copied from
> prior reports, README claims, or commit messages.
>
> **Reference (source of truth):** https://github.com/kimpearce888/supportos —
> TypeScript/React/Fastify/SQLite, v2.2.1, branch `main`.
> **Port (audit target):** https://github.com/kimpearce888/SupportOS-PlusPlus —
> Rust/Tauri 2/Leptos/WASM, branch `main`.
>
> Implementation substitutions (Rust crates, Leptos, serde, Axum, Cargo) are
> acceptable only when observable behavior remains equivalent.
> **Project scope: Linux x86_64 only; packages: `.deb` + `.AppImage` only.**

## Status legend

| Status | Meaning |
|---|---|
| MATCH | Verified equivalent to the reference (evidence required) |
| PARTIAL | Some behavior exists; coverage incomplete |
| MISSING | Reference has it; the port does not |
| DIFFERENT | Exists but materially different |
| BROKEN | Exists but does not function correctly |
| EXTRA | Port-only behavior not in the reference and not required by Linux-only packaging scope |

## Phase 0 — Baseline (frozen)

- REFERENCE_HEAD = `c346fb51466e237a89e70156ae20a3386be0b322` (branch `main`, clean tree)
- PORT_HEAD = `ac808722f849f593dd2ba0e01eb316a2b570816c` (branch `main`, clean tree)

## Verified inventory (recalculated from source)

| Metric | Reference (verified) | Port (verified) |
|---|---|---|
| HTTP routes registered | **310** (32 route files) | **205** (175 distinct paths) + 33 handlers written but unregistered |
| Route handlers that are canned/stub | 0 | **58** |
| DB migrations | 16, all recorded in `schema_migrations` | 2 recorded in `_migrations` + 26 boot-time idempotent batches (M003–M028) |
| Tables | 124 ordinary + 8 FTS5 = 132 | ~47 + 2 FTS5 |
| FTS5 tables | fts_conversations, fts_threads, fts_knowledge, fts_known_issues, fts_saved_replies, fts_ai_analyses, docs_fts, fts_custom_objects | conversations_fts, customers_fts |
| SSE event types | 7 (`hello`, `ratings`, `sync`, `conversation`, `campaign`, `notification`, `error`) | 3 (`SyncUpdated`, `WebhookReceived`, `RatingArrived`), no `event:` field |
| Notification kinds | 16 | 15 (catalog) — sweep emits only 2 |
| Operations tiles | 16 computed | 9 SQL + 7 `NotAvailable` |
| Views condition kinds | 22 (max depth 10, ≤50 nodes) | 22 (max depth **5**, ≤50 nodes) |
| AI tools | 22 read-only | 22 (same list) |
| AI providers | LM Studio + Disabled | LM Studio + **Ollama + Generic (EXTRA)** + Fake |
| Vector store | Local Qdrant REST client, graceful FTS5 fallback | Qdrant Edge adapter behind off-by-default `qdrant` feature; **nothing wired in production** |
| Tests | 672 (169 unit / 296 integration / 207 e2e) | 941 cargo tests (812 core / 20 catalog / 72 ui / 26 xtask / 11 app) |
| UI routes | 26 (25 pages + 404) | **8** (7 pages + 404); 17 page components exist but unwired |
| Keyboard shortcuts | 5 global + ~11 local | 0 global + ~3 local |
| Demo endpoints | 4 | 4 (2 diverge from reference semantics) |
| Webhook signature | HMAC-SHA1 → **base64** | HMAC-SHA1 → **hex** |
| Tauri bundle targets | msi, nsis, dmg, appimage (reference) | deb, **rpm**, appimage (rpm out of scope) |
| Rate limit | 300 mutations/60 s, socket-keyed, 429 | same (verify by execution) |
| Body limit | 20 MB | **none** |

## Master F-ID checklist

Format: **F-NNN · [Category] Item — STATUS**
Reference behavior → reference files · expected behavior · verification method ·
port evidence · required fix.

---

### A. Server foundation

- **F-001 · [Server] Loopback HTTP server** — PARTIAL
  Ref: Fastify on `127.0.0.1:3000` (`src/server/app.ts`, `index.ts`). Port:
  Axum on `127.0.0.1:{PORT|3000}` (`crates/core/src/http/server.rs`). Verify
  by execution: bind, request, headers. Fix: none expected beyond F-005/F-006.
- **F-002 · [Server] Host-header DNS-rebinding guard** — PARTIAL (verify)
  Ref: 403 + exact message for non-loopback Host when bound loopback
  (`app.ts:61-95`). Port: `dns_guard` (`http/server.rs:98-118`). Verify
  message body parity by execution.
- **F-003 · [Server] Localhost-only CORS allowlist** — PARTIAL
  Ref: origins include `http://[::1]:{port}` and both hosts for 5173-5175
  (`app.ts:17-29`). Port: no `[::1]` origin; only `127.0.0.1:5173` for dev
  ports (`server.rs:147-172`). Fix: match allowlist exactly.
- **F-004 · [Server] Mutation rate limiter** — PARTIAL (verify)
  Ref: 300/60 s keyed on socket address, GET/HEAD/OPTIONS + webhook exempt,
  429 + `retry-after`, map prune at 512 (`app.ts:37-58`). Port:
  `http/rate_limit.rs` mirrors this. Verify by execution (301st mutation → 429).
- **F-005 · [Server] Request body limit 20 MB** — MISSING
  Ref: `bodyLimit: 20 * 1024 * 1024` (`app.ts:88`). Port: no limit at all.
  Fix: `DefaultBodyLimit` (webhook route may need the ref's 512 MB
  octet-stream exception — see F-035).
- **F-006 · [Server] Real HTTP status codes for errors** — DIFFERENT
  Ref: Fastify sets 400/401/403/404/409/422/429/500 with
  `{statusCode,error,message}` envelope (plus `OperationResult {ok,message}`
  shapes on ops routes). Port: most error JSON returned with **HTTP 200** +
  `_status` field; only 403/404/429 use real codes. Fix: set real status
  codes per route to match reference.

### B. System / onboarding / demo

- **F-007 · [System] /health + /health/detailed** — PARTIAL (verify shapes).
- **F-008 · [System] /api/system/db + capabilities + tables** — PARTIAL
  Ref capabilities returns the 40-row Help Scout upstream capability matrix
  (`routes/capabilities.ts`). Port returns an empty matrix. Fix: reproduce
  matrix.
- **F-009 · [System] Onboarding wizard API** — PARTIAL
  Ref: `GET/POST /api/onboarding(/step|/complete)` state machine
  (`routes/system.ts`). Port: routes exist persisted via settings; verify
  response shapes and step validation.
- **F-010 · [Demo] POST /api/demo/enable seeds demo data** — DIFFERENT
  Ref: enables demo mode **and seeds the demo world** (demoSeed.ts equivalent
  server-side; auto-runs for empty fake-provider DBs). Port: flips a flag,
  no seeding (`routes/system.rs`). Fix: seed on enable (parity with
  `demo.rs` fake world).
- **F-011 · [Demo] simulate-incoming** — DIFFERENT
  Ref: routes through sync pipeline (job). Port: direct row insert + SSE.
  Fix: use `demo.rs::run_demo_tool(SimulatedIncomingMessage)` which enqueues
  the real sync job.
- **F-012 · [Demo] simulate-rating persists CSAT + emits 2 SSE events** — DIFFERENT
  Port does not persist (admitted in comment). Fix: persist + emit both
  events via `demo.rs`.
- **F-013 · [Demo] simulate-webhook self-POSTs through real HMAC endpoint
  with nonce** — DIFFERENT
  Port inserts directly (with broken columns). Fix: reproduce self-POST via
  `demo.rs::SimulatedWebhookEvent` (computes real HMAC).

### C. SSE / real-time

- **F-014 · [SSE] Wire format `event:` + `data:`** — DIFFERENT
  Ref: `event: <name>\ndata: <json>\n\n` with named events
  (`routes/events.ts:19-73`). Port: single `data:` line with JSON `type`
  field, no `event:` name. Fix: emit named events for all 7 types.
- **F-015 · [SSE] Heartbeat** — PARTIAL: ref `: ping` 25 s; port
  `: keep-alive` 25 s. Fix: match comment text (observable on the wire).
- **F-016 · [SSE] 25-stream cap + `error` event** — MISSING. Fix: cap +
  error event on exceed.
- **F-017 · [SSE] Event catalog** — DIFFERENT: ref 7 (hello, ratings, sync,
  conversation, campaign, notification, error); port 3. Fix: add
  hello-on-connect, conversation, campaign, notification, ratings naming.
- **F-018 · [SSE] UI client** — BROKEN
  Port `ui/src/sse.rs:80` uses relative `/api/events` which resolves to the
  Trunk/Tauri asset origin, not the Axum server. Only Inbox subscribes.
  Fix: absolute `http://127.0.0.1:{port}/api/events` (respecting CSP),
  subscribe per reference `ServerEventsBridge` behavior (query refresh +
  toasts), keep ref's polling intervals where the reference polls.

### D. Webhooks

- **F-019 · [Webhook] HMAC-SHA1 signature encoding** — DIFFERENT
  Ref: **base64** (`services/webhookEndpoint.ts`). Port: **hex**
  (`webhook.rs:15`). Fix: base64.
- **F-020 · [Webhook] Timing-safe comparison** — MATCH (ConstantTimeEq after
  length check; verify by execution).
- **F-021 · [Webhook] Persist-before-processing** — BROKEN
  Port route inserts wrong column set (`routes/webhook.rs:61` vs schema in
  `webhook.rs:177`) → **500 on every HMAC-valid delivery**. Fix: use
  `webhook_handler::process_webhook` from the route.
- **F-022 · [Webhook] Dedup semantics** — DIFFERENT
  Ref: `sha256(eventType:payload)` dedup key, duplicate → 200
  `{received:true,duplicate:true}`; port uses INSERT OR IGNORE on id/ CRC32
  path. Fix: match reference key + response.
- **F-023 · [Webhook] Unsigned when no secret** — PARTIAL (verify): ref
  accepts without verification when secret unset (startup warning); port
  skips HMAC when empty secret — verify parity incl. 401 on bad signature.
- **F-024 · [Webhook] Event fan-out pipeline (20+ HS event types)** — MISSING
  Port route never calls the job pipeline. Fix: enqueue `webhook.process`
  and implement the reference's event-type handlers.
- **F-025 · [Webhook] Boot-time drainPending** — MISSING.
- **F-026 · [Webhook] 5,000-row prune** — MISSING.

### E. Sync / Help Scout / queue

- **F-027 · [Sync] initial/incremental/reconcile/cancel endpoints** — BROKEN
  (all return fixed canned JSON; `routes/sync.rs`). Fix: wire to sync
  coordinator + job runner.
- **F-028 · [Sync] Real Help Scout REST provider** — MISSING
  Only `FakeHelpScoutProvider` exists (`helpscout.rs`). Fix: implement
  `RealHelpScoutProvider` (api.helpscout.net, OAuth bearer).
- **F-029 · [Sync] OAuth flow** — BROKEN
  `exchange_code()` stub (fake token for "test_code"); loopback callback
  returns `{"ok":true,"todo":"M2"}`. Ref: `/oauth/callback` returns HTML
  with single-use CSRF state (`routes/sync.ts:332-382`). Fix: real token
  exchange + HTML callback.
- **F-030 · [Sync] Webhook registration/unregistration with HS API** — BROKEN
  (canned). Fix: call HS webhook API.
- **F-031 · [Sync] API rate limiter + queue** — MISSING
  Ref: `rateLimiter.ts` + `apiQueue.ts`; port hard-codes status JSON
  (`routes/sync.rs:148-154`). Fix: implement + real status.
- **F-032 · [Sync] Checkpointing/resumability/reconciliation** — PARTIAL
  (tables + handlers exist, unwired). Fix: wire into sync runs.
- **F-033 · [Sync] Queue management API semantics** — DIFFERENT
  Ref: retry of `awaiting_approval` patches `approved:true`; 409 when not
  retryable (`routes/sync.ts:156-177`). Port: plain UPDATE on jobs with
  mismatched state vocabulary (always zeros). Fix: match semantics + fix
  `jobs.state` vocabulary query (`pending/claimed/done/dead`).
- **F-034 · [Sync] Sync status shape** — DIFFERENT (hard-coded). Fix: real.
- **F-035 · [Sync] Encrypted sync upload/download (.sosync over HTTP)** — MISSING
  Ref: 512 MB octet-stream upload w/ `SOSYNC` magic check vs 20 MB JSON
  limit (`routes/sync.ts:238-262`). Fix: implement both routes.

### F. Database / persistence

- **F-036 · [DB] Migration system** — DIFFERENT
  Ref: 16 recorded migrations. Port: 2 recorded + 26 boot batches;
  `self_check` expects schema_version 27 while M028 writes 28. Fix: record
  all migrations; fix self_check; do not rewrite history — forward-only.
- **F-037 · [DB] Schema coverage for observable behavior** — PARTIAL
  ~47 vs 124 tables. Missing observable features: attachments, audit_log,
  saved replies CRUD tables, drafts, snooze/scheduled replies, workflows,
  products, customer_events mirror, ai prompts, oauth states (exists),
  encrypted_sync_log, application_errors, ratings mirror details. Fix:
  add what the missing routes need.
- **F-038 · [DB] FTS5 coverage** — PARTIAL
  Ref 8 FTS tables incl. threads, knowledge, known_issues, saved_replies,
  ai_analyses, docs, custom_objects. Port: 2. Fix: add missing FTS tables +
  index maintenance so search parity (F-061) is achievable.
- **F-039 · [DB] Timestamp discipline** — PARTIAL (verify)
  Ref: dual formats compared via `julianday()`; `jobs.run_at` space format.
  Port uses julianday in places. Verify by differential tests.
- **F-040 · [DB] Retention/cleanup** — PARTIAL
  Ref prunes webhook_events/application_errors/audit_log/ai_runs/
  notifications. Port: notifications only (365 d). Fix: add prunes.
- **F-041 · [DB] .sosync backup format interop** — DIFFERENT (BLOCKER for
  compatibility) Ref: magic `SOSYNC` (6 B) + UInt32BE header length +
  JSON header, scrypt **N=32768**, r=8, p=1, AES-256-GCM over `VACUUM INTO`
  snapshot, 16-byte tag, two-phase import, 5-bundle retention,
  `encrypted_sync_log`. Port: magic `SOSYNC1`, scrypt **N=2^17**, different
  layout (`backup.rs`). Fix: byte-compatible format; verify BOTH directions
  by execution.
- **F-042 · [DB] Attachments** — MISSING (storage + routes + body limit).
- **F-043 · [DB] Seed data** — MISSING on demo enable (see F-010).

### G. Security invariants

- **F-044 · [Sec] HTML sanitization of untrusted ticket HTML** — MISSING
  Ref `security/sanitize.ts` (blocks script/event-handlers/javascript:
  URLs). Port relies on Leptos auto-escaping; **API returns raw HTML**.
  Fix: server-side sanitizer equivalent + render-time guarantee.
- **F-045 · [Sec] Connector SSRF guard** — PARTIAL
  Ref: fail-closed, DNS resolution all-addresses-public, redirect errors,
  10 s/10 MB caps, checked at create AND fetch (`security/ssrfGuard.ts`).
  Port `data_tools::validate_ssrf`: no DNS resolution step, no fetch-time
  recheck, no redirect policy. Fix: match policy exactly.
- **F-046 · [Sec] Secrets redaction** — PARTIAL
  Ref: 6-pattern AI redaction layer + redaction from responses/logs/prompts.
  Port: settings/HelpScout config redaction only. Fix: AI prompt/output
  redaction + log scrubbing.
- **F-047 · [Sec] SQL safety** — MATCH-ish (parameterized, allowlists, LIKE
  escaping, FTS quoting; verify by tests incl. injection-shaped values).
- **F-048 · [Sec] No telemetry / no cloud AI** — MATCH (verify network
  surface).
- **F-049 · [Sec] Secrets never reach WASM state** — PARTIAL (verify).
- **F-050 · [Sec] Write-pipeline order** — PARTIAL
  Ref: validate → authorize → eval-mode gate → fresh-read → merge → write →
  confirm → persist → audit with sha256 idempotency. Port `ticket_ops` has
  transaction pipeline; verify idempotency + audit records.

### H. AI stack

- **F-051 · [AI] Provider set** — EXTRA (Ollama, Generic)
  Ref: LM Studio + Disabled only. Port adds Ollama + Generic providers.
  Per NO-SCOPE-EXPANSION these are EXTRA. Fix: remove or gate behind
  nothing — reference has exactly LM Studio + Disabled.
- **F-052 · [AI] analyze/draft/similar/rewrite/verify/feedback/cluster
  endpoints** — BROKEN (canned "not available" responses; `routes/ai.rs`).
  Ref returns real analysis when provider configured, honest 503 otherwise.
  Fix: wire `ai_analysis`/`ai_features` engines; register the 4 unregistered
  handlers.
- **F-053 · [AI] Embeddings + semantic pipeline** — MISSING (unwired).
  Fix: wire embeddings cache + vector store into search.
- **F-054 · [AI] Copilot chat LLM loop + tools** — PARTIAL
  22 tools match the reference list; chat endpoint has no live loop. Fix:
  real loop with tool allowlist, 5 rounds/8 calls/4000 chars.
- **F-055 · [AI] Evaluation mode** — DIFFERENT (canned shape; no write-block
  enforcement). Ref: blocks every remote write. Fix: enforce.
- **F-056 · [AI] Translation** — DIFFERENT (deterministic stubs; ref calls
  provider). Fix: wire provider.
- **F-057 · [AI] Coaching (10 checks)** — PARTIAL (review canned; ref has
  9 deterministic + 1 AI check, honest 503 + persisted deterministic layer).
- **F-058 · [AI] Safety: advisory-only, no auto-replies, "Unknown"
  legitimate** — PARTIAL (verify by execution).

### I. Vector store / search

- **F-059 · [Vector] Persistent vector store in production default** — MISSING
  Nothing is constructed; `POST /api/search` is FTS5-only
  (`used_semantic:false`). Ref: Qdrant-backed semantic + hybrid with honest
  degradation notes. Fix: wire QdrantEdgeVectorStore as default (feature
  flag acceptable only if release default reproduces reference behavior).
- **F-060 · [Vector] Qdrant-compatible behavior** — PARTIAL (adapter exists
  behind off-by-default feature; verify persistence/restart/snapshot/scroll
  by execution).
- **F-061 · [Search] Hybrid RRF search over HTTP** — MISSING (engine exists,
  unwired). Fix: wire with k=60 RRF, mode_note degradation strings.
- **F-062 · [Search] Exact ticket-number lookup** — MISSING. Fix: `#number`
  fast path.
- **F-063 · [Search] FTS content coverage** — PARTIAL (see F-038).
- **F-064 · [Search] Universal search scopes/filters/pagination** — PARTIAL
  (verify against ref 7 scope tabs + filters).

### J. Views

- **F-065 · [Views] 22 condition kinds** — MATCH (catalog) — verify SQL
  compilation per kind by differential tests.
- **F-066 · [Views] Tree limits** — DIFFERENT: ref max depth **10**; port
  `MAX_TREE_DEPTH=5`. Fix: 10.
- **F-067 · [Views] 14 activity fields + 15 date modes + DST** — PARTIAL
  (catalog present; verify calendar-in-tz/rolling/exact semantics + DST by
  differential tests).
- **F-068 · [Views] Preview/dry-run + save-time compile check** — PARTIAL
  (verify shapes/notes parity).

### K. Operations

- **F-069 · [Ops] All 16 tiles computed** — PARTIAL (7 NotAvailable).
  Fix: compute SLA tiles (needs F-083), repeated_issue, known_issue,
  ai_escalation, issue_spike, campaign_activity.
- **F-070 · [Ops] Tile == drill-down invariant** — PARTIAL (response_state
  fragment shared; verify by differential).
- **F-071 · [Ops] Workload/capacity** — PARTIAL (verify shapes).

### L. Notifications

- **F-072 · [Notif] 16 kinds** — DIFFERENT (port catalog 15). Identify the
  16th from `src/shared/collaboration.ts:30-46` and add.
- **F-073 · [Notif] Sweep emits all kinds (15 s default, boot catch-up,
  first-run silent cursor)** — PARTIAL (sweep engine emits 2 kinds; HTTP
  route is canned). Fix: full sweep + wire.
- **F-074 · [Notif] Preferences API** — BROKEN (reads/writes nonexistent
  `notification_prefs` table). Fix: use `application_settings` prefs.
- **F-075 · [Notif] Dedup/unread/badge/live push** — PARTIAL (verify).
- **F-076 · [Notif] Retention** — PARTIAL (ref `retention_days` setting;
  verify default parity).

### M. Inbox / conversations

- **F-077 · [Inbox] Full conversation route set (36)** — PARTIAL (17
  registered). Missing: snooze, scheduled replies (+DELETE with JSON body
  `{threadId}`), attachments, workflows, moves, drafts, saved-replies CRUD,
  bulk ops, customer context, ticket history, tag management. Fix: implement
  remaining ~19 routes.
- **F-078 · [Inbox] GET /:id marks read** — PARTIAL (verify side effect).
- **F-079 · [Inbox] Bulk ops return 200 + `ok:false` on failure** — MISSING.
- **F-080 · [Inbox] Saved replies** — MISSING (route list only).
- **F-081 · [Inbox] Snooze + scheduled replies** — MISSING.

### N. Analytics / reports

- **F-082 · [Reports] Metric × dimension catalog** — PARTIAL (port 21×14;
  verify reference actual from source; do not trust claims).
- **F-083 · [Reports] SLA business-minutes engine** — MISSING
  Ref: per-mailbox business hours, first-response/resolution windows,
  business-minute arithmetic. Port: raw config_json round-trip only. Fix:
  implement engine + `/api/reports/sla` computed output.
- **F-084 · [Reports] Computed reports (why-contacting, top-questions,
  doc-gaps, answer-reuse, issue-radar, effectiveness, friction)** — BROKEN
  (canned/static arrays). Fix: compute from DB.
- **F-085 · [Reports] Narrative + release correlation + HS report proxy** —
  MISSING (handlers unregistered/stub). Fix: register + implement.
- **F-086 · [Reports] CSV export** — MISSING. Fix: `format=csv` handling.
- **F-087 · [Reports] DST/timezone correctness** — PARTIAL (verify by
  differential with DST edge cases).
- **F-088 · [Reports] Dashboard metrics SQL** — BROKEN (column drift:
  `assignee_user_id`, `conversation_threads.type`, `ai_runs.result_json`
  → zeros). Fix columns; compute real arrays.

### O. Intelligence

- **F-089 · [Intel] Known issues + clusters + links** — PARTIAL (link
  handlers unregistered). Fix: register + implement.
- **F-090 · [Intel] Incidents: impact/notes/related/releases/conversation
  links (12 routes missing)** — PARTIAL. Fix: register + implement; impact
  must not be a stub.
- **F-091 · [Intel] Customer timeline + rebuild** — PARTIAL (rebuild canned).
- **F-092 · [Intel] Support graph (derived + human-asserted edges)** — PARTIAL
  (verify edge semantics).
- **F-093 · [Intel] Knowledge freshness/gaps/reimport/file import** — PARTIAL
  (file import + delete/review/verify handlers unregistered).
- **F-094 · [Intel] Customer memory + personality red line (22 patterns,
  write 422 + read quarantine)** — PARTIAL (verify pattern parity).
- **F-095 · [Intel] Interaction intelligence + evidence + profile
  overrides** — PARTIAL (overrides missing).
- **F-096 · [Intel] Customer support health + org timeline** — PARTIAL
  (org endpoints stubs).

### P. Outreach

- **F-097 · [Outreach] Segment preview/estimate/suggest + AI suggest** —
  BROKEN (stubs). Ref: preview with 5000-cap disclosed truncation.
- **F-098 · [Outreach] Campaign queue + throttling (batches of 5, USER_SEND
  priority) + retry semantics (3 attempts, unknown outcomes reconciled)** —
  MISSING (no send worker). Fix: implement.
- **F-099 · [Outreach] DNC enforcement at preview/validate/send** — PARTIAL.
- **F-100 · [Outreach] outreach_events audit trail** — MISSING.
- **F-101 · [Outreach] Campaign stats + monitor** — PARTIAL (verify).

### Q. Connectors / custom objects / docs / quality

- **F-102 · [Conn] refresh + test + rows** — BROKEN (canned/empty). Ref:
  real fetch w/ SSRF, path jail, 10 s/10 MB, JSON/CSV/text only.
- **F-103 · [Conn] First-run wizard** — MISSING.
- **F-104 · [Conn] allowed_ai gating for Copilot** — PARTIAL (verify).
- **F-105 · [CO] Custom object links (3 handlers unregistered)** — PARTIAL.
- **F-106 · [Docs] Docs mirror + hybrid search + collections** — PARTIAL
  (verify against ref docs routes incl. articles content).
- **F-107 · [Qual] QA analyze/rebuild, friction rebuild, gaps rebuild** —
  BROKEN (canned). Fix: wire engines.
- **F-108 · [Qual] Report builder + QA signals + effectiveness** — PARTIAL
  (verify computation parity).

### R. UI parity (Leptos vs React)

- **F-109 · [UI] Route `/` Dashboard** — PARTIAL (exists; verify KPI cards,
  chart, radar, scope URL state).
- **F-110 · [UI] `/inbox` + `/inbox/conversation/:id` 3-pane** — PARTIAL
  (list+thread exist; missing: AI/Customer/Copilot context pane, filters in
  URL, saved-view selector reading URL).
- **F-111 · [UI] `/search` 7 scope tabs + filter panel** — MISSING (page
  module exists, unwired).
- **F-112 · [UI] `/customers` + `/customers/:id`** — MISSING (unwired).
- **F-113 · [UI] `/organizations` + `/organizations/:id`** — MISSING (no
  page module at all).
- **F-114 · [UI] `/ai` 6 tabs** — MISSING (unwired).
- **F-115 · [UI] `/issues` 5 tabs + `?tab=`** — MISSING (unwired).
- **F-116 · [UI] `/incidents` + `/incidents/:id`** — MISSING (unwired).
- **F-117 · [UI] `/custom-objects`** — MISSING (unwired).
- **F-118 · [UI] `/connectors`** — MISSING (unwired).
- **F-119 · [UI] `/graph` explorer** — MISSING (unwired).
- **F-120 · [UI] `/knowledge` 4 tabs + `?doc=`** — MISSING (unwired).
- **F-121 · [UI] `/docs`** — MISSING (unwired).
- **F-122 · [UI] `/reports` 10 tabs** — MISSING (unwired).
- **F-123 · [UI] `/outreach` 4-step wizard** — MISSING (unwired).
- **F-124 · [UI] `/operations` 16 tiles + workload** — PARTIAL (wired;
  verify tiles render + drill-down links work).
- **F-125 · [UI] `/notifications` list/mentions/prefs** — PARTIAL (wired;
  Mark-as-read dead, prefs broken server-side).
- **F-126 · [UI] `/automation` rules + runs + engine toggle + approve/
  reject** — PARTIAL (approve/reject controls dead).
- **F-127 · [UI] `/sync-health` + 5 s polling + webhook register** — PARTIAL.
- **F-128 · [UI] `/settings` 8 tabs incl. backups + encrypted sync** — PARTIAL.
- **F-129 · [UI] `/onboarding` 6-step wizard, shell hidden, persisted** —
  DIFFERENT (overlay reappears every launch; not persisted).
- **F-130 · [UI] 404 page** — MATCH (verify content).
- **F-131 · [UI] Global keyboard shortcuts (⌘/Ctrl+K, /, g d, g i, g s)** —
  MISSING. Fix: implement.
- **F-132 · [UI] Command palette (openable, clickable items)** — BROKEN
  (palette_open never set; items not clickable).
- **F-133 · [UI] URL state (filters/tabs/pagination)** — MISSING.
- **F-134 · [UI] Light/dark theme** — MISSING (dark only; ref toggles).
- **F-135 · [UI] Toasts for SSE events (capped 5)** — MISSING.
- **F-136 · [UI] Destructive confirmations** — MISSING (bulk close, webhook
  delete fire immediately).
- **F-137 · [UI] IPC wiring (`withGlobalTauri`)** — BROKEN
  `window.__TAURI__.invoke` undefined in real webview → all 42 IPC calls
  fail once pages are routed. Fix: enable `withGlobalTauri` or use
  @tauri-apps/api bindings.
- **F-138 · [UI] Reference polling intervals (17 refetchIntervals)** —
  DIFFERENT (no polling anywhere; SSE-only). Fix: reproduce ref's actual
  strategy per query.
- **F-139 · [UI] Responsive behavior present in reference** — PARTIAL.

### S. Configuration

- **F-140 · [Conf] Settings API (18 ref routes vs 10)** — PARTIAL. Missing:
  ratings refresh, redaction toggle, AI redaction patterns, workers restart
  on interval change, strict-partial patch semantics, internal keys
  unwritable, `automatic_reply_sending` forced false. Fix per ref.
- **F-141 · [Conf] LM Studio settings + test** — PARTIAL (test stub).
- **F-142 · [Conf] Business hours per mailbox** — PARTIAL (round-trip only).
- **F-143 · [Conf] Data directories + demo mode env** — PARTIAL (Rust-native
  substitution acceptable; document as intentional difference).

### T. Packaging / CI / bootstrap (Linux-only scope)

- **F-144 · [Pkg] Tauri bundle targets exactly `["deb","appimage"]`** —
  DIFFERENT (currently deb, rpm, appimage). Fix config + icons list.
- **F-145 · [Pkg] `verify_config` REQUIRED_BUNDLE_TARGETS lockstep** —
  DIFFERENT (requires rpm today). Fix with F-144.
- **F-146 · [CI] Linux-only workflows; no windows/macos runners; no RPM
  jobs; package smoke tests** — DIFFERENT (rpm jobs + Python steps exist).
  Fix all 5 workflows.
- **F-147 · [Boot] bootstrap.sh Linux-only; delete bootstrap.ps1** —
  DIFFERENT (Darwin branch + dnf branch + Windows refs; ps1 file exists).
  Fix.
- **F-148 · [Pkg] DEB verification (install/launch/db/desktop entry/icons/
  removal)** — UNVERIFIED (environment lacks root; attempt local extraction
  verification).
- **F-149 · [Pkg] AppImage verification (launch/db/shutdown)** — UNVERIFIED
  (same constraint; document actually verified environments only).

### U. Repository cleanliness

- **F-150 · [Clean] Remove Windows material** — DIFFERENT
  (`bootstrap.ps1`, `icons/icon.ico`, cfg branches in `config.rs:102-119`,
  `not(target_os="macos")` gates, docs). Fix.
- **F-151 · [Clean] Remove macOS material** — DIFFERENT (`icon.icns`,
  Darwin bootstrap branch, docs). Fix.
- **F-152 · [Clean] Remove RPM material** — DIFFERENT (tauri.conf,
  verify_config, CI jobs, docs, ARTIFACTS.md). Fix.
- **F-153 · [Clean] Remove Python/PowerShell tooling** — DIFFERENT
  (8 Python scripts in scripts/). Fix: port needed drivers to Rust xtask;
  delete the rest.
- **F-154 · [Clean] Consolidate stale docs** — DIFFERENT (UI-PARITY.md,
  UI-GAP.md, FINAL-PARITY-AUDIT.md, TASKS.md superseded; PROGRESS.md/README
  contain false claims). Fix.
- **F-155 · [Clean] Dead code** — DIFFERENT (17 unwired UI pages are F-109
  –F-128 work, not deletions; but remove dead platform branches, unused
  `hyper` dep, `CopilotGlobal`-style dead exports). Fix.
- **F-156 · [Clean] Documentation matches reality** — DIFFERENT. Fix at end.

### V. Test porting

- **F-157 · [Test] Port reference behavioral coverage** — PARTIAL
  941 cargo tests exist but route-level integration is Python-script-based
  (to be removed). Port the reference's behavioral cases (API validation,
  DST, SLA, webhooks, AI safety, data compat) into Rust tests.

---

## Verdict

**Full parity not achieved.**

Counts (initial, from source inspection; execution verification in
progress): MATCH ~3 · PARTIAL ~60 · MISSING ~55 · DIFFERENT ~28 · BROKEN ~14
· EXTRA 1 (F-051). Blockers: F-006, F-014, F-019, F-021, F-027–F-035,
F-041, F-044, F-052, F-059, F-083, F-137, and the 19 missing UI routes
(F-111–F-123).

This file is updated as fixes land; each F-ID gains evidence + verification
command + result. See PROGRESS.md for session state.
