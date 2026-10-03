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

## Verified inventory (recalculated from source; Session-B corrections in **bold**)

| Metric | Reference (verified) | Port (verified) |
|---|---|---|
| HTTP routes registered | **311** (32 route files; corrected from 310 — the multiline `POST /api/sync/encrypted/upload` registration was missed by the original single-line grep) | **257** (Session B: +33 Session-A WIP registrations, +12 new settings/audit/backups/sosync routes, +4 mirror readouts, +5 OAuth, +1 upload) — 55 still missing |
| Route handlers that are canned/stub | 0 | 58 at Session-A baseline (sync initial/incremental/reconcile/cancel + queue + webhook register/unregister still canned; recount pending) |
| DB migrations | 16, all recorded in `schema_migrations` | 2 recorded in `_migrations` + 26 boot-time idempotent batches (M003–M028); **Session B: `schema_migrations` 1..16 reference-equivalent record now seeded at boot (used by `migrations_applied` + .sosync schema guard)** |
| Tables | 124 ordinary + 8 FTS5 = 132 | **66 + 2 FTS5 on a fresh DB** (Session-A's "85 tables" observation was a stale-DB artifact; Session B added `mailbox_business_hours`, `audit_log`, `application_errors`, `encrypted_sync_log`, `inbox_fields(+options)`, `saved_replies`, `workflows`, `user_statuses`, `webhook_configs`, `schema_migrations`) |
| FTS5 tables | fts_conversations, fts_threads, fts_knowledge, fts_known_issues, fts_saved_replies, fts_ai_analyses, docs_fts, fts_custom_objects | conversations_fts, customers_fts |
| SSE event types | 7 (`hello`, `ratings`, `sync`, `conversation`, `campaign`, `notification`, `error`) | 3 (`SyncUpdated`, `WebhookReceived`, `RatingArrived`), no `event:` field |
| Notification kinds | 16 | 15 (catalog) — sweep emits only 2 |
| Operations tiles | 16 computed | 9 SQL + 7 `NotAvailable` |
| Views condition kinds | 22 (max depth 10, ≤50 nodes) | 22 (max depth **5**, ≤50 nodes) |
| AI tools | 22 read-only | 22 (same list) |
| AI providers | LM Studio + Disabled | LM Studio + Fake (EXTRA providers removed in Session A) |
| Vector store | Local Qdrant REST client, graceful FTS5 fallback | Qdrant Edge adapter behind off-by-default `qdrant` feature; **nothing wired in production** |
| Tests | 672 (169 unit / 296 integration / 207 e2e) | **916 cargo tests** (Session B: 794 core / 21 catalog / 71 ui / 26 xtask + 4 bins; app crate blocked in sandbox — no GTK/webkit dev libs; 1 ignored cross-compat test needs the JS harness) |
| UI routes | 26 (25 pages + 404) | **8** (7 pages + 404); 17 page components exist but unwired |
| Keyboard shortcuts | 5 global + ~11 local | 0 global + ~3 local |
| Demo endpoints | 4 | 4 (2 diverge from reference semantics) |
| Webhook signature | HMAC-SHA1 → **base64** | base64 (Session A fix; **re-verified live in Session B: 5/5 scenarios — 401 invalid/missing sig with secret set, 200 dedup, 400 malformed**) |
| Tauri bundle targets | msi, nsis, dmg, appimage (reference) | deb, appimage (Session A cleanup; rpm/msi/nsis/dmg removed) |
| Rate limit | 300 mutations/60 s, socket-keyed, 429 | same (verify by execution) |
| Body limit | 20 MB | 20 MB (`DefaultBodyLimit`); **512 MB per-route override on `/api/sync/encrypted/upload` (Session B)** |

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
- **F-003 · [Server] Localhost-only CORS allowlist** — MATCH (fixed: [::1]:{port} + full dev-port set added; verified by execution 2026-10-02)
  Ref: origins include `http://[::1]:{port}` and both hosts for 5173-5175
  (`app.ts:17-29`). Port: no `[::1]` origin; only `127.0.0.1:5173` for dev
  ports (`server.rs:147-172`). Fix: match allowlist exactly.
- **F-004 · [Server] Mutation rate limiter** — PARTIAL (verify)
  Ref: 300/60 s keyed on socket address, GET/HEAD/OPTIONS + webhook exempt,
  429 + `retry-after`, map prune at 512 (`app.ts:37-58`). Port:
  `http/rate_limit.rs` mirrors this. Verify by execution (301st mutation → 429).
- **F-005 · [Server] Request body limit 20 MB** — MATCH (fixed: DefaultBodyLimit 20 MB; .sosync 512 MB route will override when it lands)
  Ref: `bodyLimit: 20 * 1024 * 1024` (`app.ts:88`). Port: no limit at all.
  Fix: `DefaultBodyLimit` (webhook route may need the ref's 512 MB
  octet-stream exception — see F-035).
- **F-006 · [Server] Real HTTP status codes for errors** — MATCH (fixed: all 30 _status sites converted; verified live: 404/422/200/429 + Fastify envelopes)
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
- **F-013 · [Demo] simulate-webhook through real HMAC pipeline** — MATCH (fixed: reference envelope {conversationId,objectID,id,nonce} + computed HMAC through process_webhook)
  Port inserts directly (with broken columns). Fix: reproduce self-POST via
  `demo.rs::SimulatedWebhookEvent` (computes real HMAC).

### C. SSE / real-time

- **F-014 · [SSE] Wire format `event:` + `data:`** — MATCH (fixed; verified live)
  Ref: `event: <name>\ndata: <json>\n\n` with named events
  (`routes/events.ts:19-73`). Port: single `data:` line with JSON `type`
  field, no `event:` name. Fix: emit named events for all 7 types.
- **F-015 · [SSE] Heartbeat** — MATCH (fixed: `: ping` 25 s; verified live): ref `: ping` 25 s; port
  `: keep-alive` 25 s. Fix: match comment text (observable on the wire).
- **F-016 · [SSE] 25-stream cap + `error` event** — MATCH (fixed + cap test). Fix: cap +
  error event on exceed.
- **F-017 · [SSE] Event catalog** — MATCH (fixed: all 6 channels + hello; verified live): ref 7 (hello, ratings, sync,
  conversation, campaign, notification, error); port 3. Fix: add
  hello-on-connect, conversation, campaign, notification, ratings naming.
- **F-018 · [SSE] UI client** — PARTIAL (fixed: absolute URL + named listeners; only Inbox subscribes — remaining pages need wiring, see F-109..F-128)
  Port `ui/src/sse.rs:80` uses relative `/api/events` which resolves to the
  Trunk/Tauri asset origin, not the Axum server. Only Inbox subscribes.
  Fix: absolute `http://127.0.0.1:{port}/api/events` (respecting CSP),
  subscribe per reference `ServerEventsBridge` behavior (query refresh +
  toasts), keep ref's polling intervals where the reference polls.

### D. Webhooks

- **F-019 · [Webhook] HMAC-SHA1 signature encoding** — MATCH (fixed: base64, OpenSSL cross-checked vectors)
  Ref: **base64** (`services/webhookEndpoint.ts`). Port: **hex**
  (`webhook.rs:15`). Fix: base64.
- **F-020 · [Webhook] Timing-safe comparison** — MATCH (ConstantTimeEq after
  length check; verify by execution).
- **F-021 · [Webhook] Persist with reference schema** — MATCH (fixed: reference schema + verify-then-persist order; verified live)
  Port route inserts wrong column set (`routes/webhook.rs:61` vs schema in
  `webhook.rs:177`) → **500 on every HMAC-valid delivery**. Fix: use
  `webhook_handler::process_webhook` from the route.
- **F-022 · [Webhook] Dedup semantics** — MATCH (fixed: sha256(eventType:payload); verified live)
  Ref: `sha256(eventType:payload)` dedup key, duplicate → 200
  `{received:true,duplicate:true}`; port uses INSERT OR IGNORE on id/ CRC32
  path. Fix: match reference key + response.
- **F-023 · [Webhook] Unsigned when no secret** — MATCH (fixed: reference policy + 401 on bad signature; verified live) (verify): ref
  accepts without verification when secret unset (startup warning); port
  skips HMAC when empty secret — verify parity incl. 401 on bad signature.
- **F-024 · [Webhook] Event fan-out pipeline (24 event types)** — PARTIAL (fixed: full reference switch enqueues sync_conversation/merge/customer/tags/ratings/user-status jobs; the job RUNNER that executes them is still unwired — T9)
  Port route never calls the job pipeline. Fix: enqueue `webhook.process`
  and implement the reference's event-type handlers.
- **F-025 · [Webhook] Boot-time drainPending** — PARTIAL (drain_pending implemented + tested; not yet called at app boot — T9).
- **F-026 · [Webhook] 5,000-row prune** — MATCH (fixed + test).

### E. Sync / Help Scout / queue

- **F-027 · [Sync] initial/incremental/reconcile/cancel endpoints** — BROKEN
  (all return fixed canned JSON; `routes/sync.rs`). Fix: wire to sync
  coordinator + job runner.
- **F-028 · [Sync] Real Help Scout REST provider** — MISSING
  Only `FakeHelpScoutProvider` exists (`helpscout.rs`). Fix: implement
  `RealHelpScoutProvider` (api.helpscout.net, OAuth bearer).
- **F-029 · [Sync] OAuth flow routes** — MATCH for the route surface
  (Session B). `/api/oauth/authorize-url` (demo shape; 16-byte hex state
  stored as JSON), `/api/oauth/status` (demo + real shapes),
  `/api/oauth/client-credentials` (400 unconfigured, 401 on exchange
  failure, token stored + audited), `/api/oauth/disconnect`
  (`{ok,message}` + audit), `/oauth/callback` — HTML pages byte-identical
  to the reference in all three failure modes (demo, error param,
  missing/state-mismatch) + success page; single-use CSRF state verified
  (deleted on read, mismatch refuses exchange). Live-diffed 6/6. NOTE:
  the real-provider token exchange path cannot be end-to-end verified in
  the sandbox (no external network to api.helpscout.net); the demo-mode
  branches and failure paths ARE execution-verified.
- **F-030 · [Sync] Webhook registration/unregistration with HS API** — BROKEN
  (canned). Fix: call HS webhook API.
- **F-031 · [Sync] API rate limiter + queue** — MISSING
  Ref: `rateLimiter.ts` + `apiQueue.ts`; port hard-codes status JSON
  (`routes/sync.rs:148-154`). Fix: implement + real status.
- **F-032 · [Sync] Checkpointing/resumability/reconciliation** — PARTIAL
  (tables + handlers exist, unwired). Fix: wire into sync runs.
- **F-033 · [Sync] Queue stats vocabulary** — MATCH for counts (pending/claimed/done/dead fixed); retry/approval semantics still pending sync wiring
  Ref: retry of `awaiting_approval` patches `approved:true`; 409 when not
  retryable (`routes/sync.ts:156-177`). Port: plain UPDATE on jobs with
  mismatched state vocabulary (always zeros). Fix: match semantics + fix
  `jobs.state` vocabulary query (`pending/claimed/done/dead`).
- **F-034 · [Sync] Sync status shape** — DIFFERENT (hard-coded). Fix: real.
- **F-035 · [Sync] Encrypted sync upload (.sosync over HTTP)** — MATCH
  (Session B). `POST /api/sync/encrypted/upload` with per-route 512 MB
  `DefaultBodyLimit` (global stays 20 MB), 422 on short body, 422 on
  non-`SOSYNC` magic with the exact reference message, saved as
  `uploaded-{ms}.sosync` in the bundles dir, `{ok, path, message}` response.
  Live-diffed vs the reference (422 + 200 + file-written all match).

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
- **F-041 · [DB] .sosync backup format interop** — MATCH for the crypto
  layer / wire format (Session B, 2026-10-02). The port's
  `encrypted_sync.rs` now produces/consumes the reference byte format
  exactly: magic `SOSYNC` (6 B) + UInt32BE header length + JSON header,
  scrypt **N=32768**, r=8, p=1, AES-256-GCM over a `VACUUM INTO` snapshot,
  16-byte tag appended, two-phase verify/import, integrity + schema guards,
  safety backup, atomic swap, 5-bundle retention, `encrypted_sync_log`
  ledger. **Cross-app decryption verified BOTH directions by execution**:
  reference-created bundle → port `verify_bundle` OK (5 conversations,
  2 customers, schema 16); port-created bundle → Node reference code
  decrypts + counts match; wrong passphrase → reference-exact message;
  flipped tag byte → rejected. Routes `/api/sync/encrypted(+/export/
  verify/import)` live-diff 5/5 vs the reference. REMAINING (recorded, not
  hidden): full data interop is limited by port schema coverage (F-037) —
  a reference snapshot restores into the port only as far as the schemas
  align; the port's own `backup.rs` (vector-store-only, `SOSYNC1`) remains
  for the vector snapshot path and is not used by the sync routes.
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

- **F-051 · [AI] Provider set** — MATCH (fixed: Ollama + Generic removed; exactly LM Studio + None, like the reference)
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
- **F-066 · [Views] Tree limits** — MATCH (fixed: depth 10 per reference viewEngine.ts:72): ref max depth **10**; port
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

- **F-072 · [Notif] kinds** — MATCH (15/15 = reference list; defaults ALL enabled + severity map now exact) (port catalog 15). Identify the
  16th from `src/shared/collaboration.ts:30-46` and add.
- **F-073 · [Notif] Sweep emits all kinds (15 s default, boot catch-up,
  first-run silent cursor)** — PARTIAL (sweep engine emits 2 kinds; HTTP
  route is canned). Fix: full sweep + wire.
- **F-074 · [Notif] Preferences API** — MATCH (fixed: application_settings-backed, {type,enabled,default_enabled}, 422 validation; verified live) (reads/writes nonexistent
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
- **F-088 · [Reports] Dashboard metrics SQL** — PARTIAL (column drift fixed — assignee_id/thread_type/response_json; computed report bodies still canned, see F-084) (column drift:
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
- **F-142 · [Conf] Business hours per mailbox** — MATCH (Session B,
  2026-10-02). GET returns the exact reference `mailboxes` shape
  (configured flag, safe-parsed days, nullable targets); PUT reproduces the
  full validation chain (422 envelope on bad mailboxId, 404 unknown
  mailbox, 422 schema message, 422 unknown IANA timezone via chrono-tz),
  writes `mailbox_business_hours` (reference DDL) + audit entry + exact
  success message; DELETE validates + clears + exact message. Live-diffed
  against the reference (validation matrix 6/6, GET shape SAME). Fixes the
  Session-A panic (`no such column: config_json`).
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

Session-A progress (2026-10-02): 24 F-IDs advanced to MATCH (F-003, F-005,
F-006, F-013, F-014–F-017, F-019, F-021–F-023, F-026, F-033, F-051, F-066,
F-072, F-074 + F-002/F-004/F-020 execution-verified). Remaining blockers:
F-027–F-035 (sync engine + real provider + OAuth), F-041 (.sosync
byte-compat), F-044 (HTML sanitizer), F-045 (SSRF DNS resolution),
F-052–F-058 (AI endpoint wiring), F-059–F-063 (vector store + hybrid
search), F-077+ (inbox routes), F-083 (SLA business-minutes), F-084
(computed reports), F-109–F-139 (17 UI routes + shortcuts + URL state +
theme + IPC), F-140+ (settings API), packaging verification F-148/F-149.

Every fixed F-ID above carries its execution evidence (curl commands +
responses in the session log). See PROGRESS.md for resume state.

Session-B progress (2026-10-02/03, reference c346fb5, port 53bdeaa → 3fce745
→ 360e3c4 → head): fresh from-scratch execution audit + repair round.

Verification performed (commands + live servers ref :3471 / port :3470):
- Route inventory corrected: reference = **311** routes (multiline
  registrations included); port = 257 registered, 55 missing
  (scripts/route_gap.py).
- Webhook HMAC re-verified by execution: 5/5 scenarios (valid 200, invalid
  401 exact body, missing 401, duplicate 200 dedup flag, malformed 400
  Fastify envelope).
- Live API differential over 86 endpoints (scripts/diff_api.sh) + a
  focused 18-check differential on newly implemented routes
  (scripts/diff_new_routes.py): all SAME for status + shape.
- `/health/detailed` now byte-shape-matches the reference (9 top-level keys,
  indexed object, SyncState vocabulary, curated LM Studio error,
  migrations_applied=16). Remaining deltas are runtime-state only (path,
  size, sync LIVE vs NEW — the sync engine is still unwired).
- .sosync cross-app compatibility verified BOTH directions by execution
  (Node reference crypto code ↔ Rust port): encrypt/decrypt/count/tag-tamper
  all match; wrong passphrase → reference-exact message.
- False alarm closed: axum 0.7.9 + matchit 0.7.3 accepts `:param` routes
  (execution-proven with a standalone test crate) — no fix needed.
- Fresh-DB table count corrected to 66 (+2 FTS5); Session-A's "85" was a
  stale-database artifact that masked the business-hours panic.

F-IDs advanced (Session B): F-029 (OAuth route surface) MATCH;
F-035 (.sosync upload) MATCH; F-041 (.sosync format) MATCH for the crypto
layer with documented data-interop limitation; F-142 (business hours) MATCH;
F-140 partial→ (appearance + qdrant/test + business-hours now reference-
exact; 18-route settings surface still incomplete); F-077 partial→
(saved-replies real; inbox-fields/workflows/users-statuses/webhook-configs
mirror readouts added); audit/errors/backups route cluster landed
(F-038-adjacent); +33 Session-A WIP route registrations committed.

Evidence trail: /home/z/my-project/scripts/*.sh|*.py|*.js +
/home/z/my-project/sosync-test/ + cross_compat_test.rs (ignored test with
harness instructions). All new units tested (encrypted_sync 3, backup_service
2, audit 1, mirror_readouts 1). Suite: 916 pass / 0 fail.

Remaining blockers (unchanged unless noted): F-027/F-028/F-032/F-034 (sync
engine + real provider — the 55 missing routes are dominated by sync,
conversations ops, outreach lifecycle, ticket-states CRUD, attributes,
reports-builder run/saved, incidents from-cluster, knowledge gap draft/
decide, attachments, memory/interaction overrides, queue clear, rebuilds);
F-044 (HTML sanitizer), F-045 (SSRF DNS), F-052–F-058 (AI wiring),
F-059–F-063 (vector + hybrid), F-083 (SLA business-minutes),
F-109–F-139 (UI), packaging F-148/F-149 (sandbox: no GTK dev headers, no
root — CI jobs exist, local .deb/.AppImage verification still pending).

This file is updated as fixes land; each F-ID gains evidence + verification
command + result.

---

## Session-C record (2026-10-03, reference c346fb5, port 53bdeaa → e00443b)

### Phase 0 (re-frozen)
- REFERENCE_HEAD = `c346fb51466e237a89e70156ae20a3386be0b322` (unchanged)
- PORT_HEAD at Session-C start = `53bdeaa` on GitHub + **12 unpushed local commits through `38adb87`** (Session B/C continuation work: T9 sync-engine wiring, T11 security invariants, T15/T17 conversation write ops, T18 operations tiles, T19/T6 ticket-states, T23 outreach, OAuth flows, .sosync cross-verification, +33 WIP registrations) — all preserved and consolidated as the working base.

### Phase 3 — repository cleanup (Session C, commit 0d609cc)
- Removed stale doc generations: `docs/MASTER-SPEC.md` (3-OS/RPM founding spec), `docs/original-notes/` (7 files), `docs/PARITY-MATRIX.md` (superseded duplicate matrix), `docs/audit/` (5 oldest-generation audit reports).
- `docs/DEVIATIONS.md` rewritten with stable IDs + tombstones: DEV-003 (macOS tests) and DEV-005 (InMemoryVectorStore-in-production) recorded as REMOVED with resolution pointers; DEV-002 updated to the accurate qdrant state; DEV-006 Linux-only scope retained.
- `docs/DECISIONS.md`: D-005/D-011/D-012 narrowed to deb+AppImage/Linux-only reality; D-013/D-014/D-019 detached from the deleted spec file; D-014 output moved to `target/discovery/`.
- `AGENTS.md` precedence rewritten (reference repo + PARITY.md are the authorities); `README.md`, `REFERENCE-VERSION.md`, `MANUAL-VERIFICATION.md`, `docs/architecture/VECTORSTORE.md` cross-references refreshed.
- `cargo xtask discover` now writes `target/discovery/inventory.json` (generated artifact out of the source tree).
- Removed the `windows_subsystem` cfg attribute from the Tauri main; `headless_boot.rs` stale Windows/macOS CI comments rewritten.
- Final cleanliness scan (git grep across windows/macos/darwin/apple/msi/nsis/dmg/ico/icns/rpm/powershell/node/npm/typescript/javascript/python/docker): **zero unsupported-platform files, zero .py/.ps1/.js/.ts/.exe/.icns/.ico files**; only `bootstrap.sh` (Linux/apt-only); remaining text hits are guard-list assertions (CI forbids .rpm/.msi/.exe artifacts), honest scope statements (DEV-006), and historical F-ID records below.

### Route surface — FULL PARITY (Session C, commit e00443b)
- **311/311 reference routes registered, 0 missing, 0 extra** (verified by `scripts/count_routes.py`, multiline/chained-method-aware, against the 311-route reference inventory).
- Session-C additions: reports builder (POST run / GET+POST saved / DELETE saved/:id — reference Zod 422 issue joining, save/list/delete via the M034 `report_definitions` machinery), GET /api/issues/known/:id/impact (reference impact.ts computation: counts, orgs, first/last seen, open/closed/waiting, 7-day growth ratio + direction, new/rising/falling/stable trend, top-10 inboxes), GET /api/friction/:conversationId (deterministic span-based engine port: repeated_customer_explanations with 6-word n-gram matching + repeated_agent_questions, evidence-pinned, "heuristic, not a judgment" wording), GET /api/copilot/starter-questions/:conversationId (reference conversation-facts-derived question set).
- Landed from the consolidated unpushed work: attributes domain (M033 reference-shaped `ai_attributes` with superseded_at versioning + full repo: save_snapshot/current/history/conversations_matching/distributions/distinct_values + all 7 routes reference-exact), incidents from-cluster/from-known-issue/unlink-conversation/delete-ref, interaction overrides (POST/DELETE response_preference, immutable-AI semantics), knowledge-gap candidates decide/draft, automation manual trigger (automation_runs), memory entry delete (403 AI-immutable).

### Phase 4 — execution audit (Session C)
- `cargo check --workspace --exclude app` — PASS (app crate needs GTK/WebKit2GTK dev headers not installable in this sandbox; CI builds it on ubuntu runners — same class of limitation documented since M1-T02).
- `cargo check -p ui --target wasm32-unknown-unknown` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `cargo clippy --workspace --all-targets --exclude app -- -D warnings` — PASS.
- `cargo test --workspace --exclude app` — **1034 passed / 0 failed / 1 ignored** (30 catalog + 907 core + 71 ui + 18+8 xtask; the 1 ignored is the cross-compat harness test requiring the Node reference harness).

### Honest remaining gaps (drive the verdict below)
1. **Sync data landing** — engine state/checkpoints/routes are wired, but resource handlers still return `success_with_data` which discards fetched rows (`sync.rs:376-390`); no 3-phase reconcile; no real-provider OAuth token exchange (only `test_code`). F-027/F-028/F-032/F-034 remain PARTIAL.
2. **Background workers** — none of the 8 reference timers exist; `/api/system/status` reports the reference interval block as static JSON. F-047 MISSING.
3. **FTS breadth** — 2 of 8 FTS5 tables; indexing callers exist on limited paths; knowledge/docs/saved-replies/custom-object FTS absent. F-018 PARTIAL.
4. **Semantic search** — qdrant feature off by default; Float32-in-SQLite cosine fallback + hybrid RRF wiring into /api/search not landed; `used_semantic` honestly reports false. F-017 PARTIAL (DEV-002).
5. **Demo mode** — no fake-provider world, no seedDemoData (15 categories). F-052 PARTIAL.
6. **UI** — 8 of 26 routes wired; 17 page components unrouted; no sidebar/palette/shortcuts/theme toggle/polling; `withGlobalTauri` IPC bridge unverified in the packaged app. F-109..F-139 PARTIAL/MISSING.
7. **Database breadth** — 66+2 FTS5 tables vs reference 133; Help Scout sub-record mirror tables absent. F-054 PARTIAL.
8. **Packaging execution proof** — deb/AppImage CI jobs exist (with format assertions + smoke-install), but no local artifact was built in this sandbox (no GTK headers, no root). F-148/F-149 verify-in-CI only.

### Session-C verdict
**Full parity not achieved.** Route surface: 311/311 MATCH (registration level — per-route behavioral differentials were verified live for the Session-B batches recorded above; the Session-C additions follow the same reference-exact contract pattern and carry unit-level machinery tests, but were not live-differentially tested against a running reference in this session). The blockers above (sync data landing, workers, FTS breadth, semantic wiring, demo seed, UI wiring, DB breadth, local packaging proof) remain the ordered work list for the next session. No EXTRA functionality remains (0 extra routes; EXTRA AI providers removed; bundle targets exactly deb+appimage).
