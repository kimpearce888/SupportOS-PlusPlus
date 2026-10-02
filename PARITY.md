# PARITY.md — SupportOS++ vs Reference Audit

> Phase 1 master checklist. The reference repo is the ONLY source of truth.
> Reference: https://github.com/kimpearce888/supportos (TypeScript, v2.2.1)
> Port: https://github.com/kimpearce888/SupportOS-PlusPlus (Rust/Tauri 2)

## Reference Inventory Summary

| Metric | Count |
|---|---|
| HTTP routes | 310 (across 32 route files) |
| Client pages | 21 |
| Client components | 23 |
| Database migrations | 16 (001–016) |
| Repositories | 22 |
| Shared modules | 14 |
| Test files | 51 (14 unit + 24 integration + 13 E2E) |
| Test cases | 672 (169 unit + 296 integration + 207 E2E) |
| Scripts | 21 |
| Server modules | ~100 (ai, analytics, automation, collaboration, connectors, customobjects, database, graph, inbox, integrations, issues, knowledge, memory, notifications, operations, outreach, routes, search, security, segmentation, services, sync, timeline) |
| CI workflows | 2 (ci.yml: lint+typecheck+build+test+smoke; desktop-release.yml: 3-OS MSI/NSIS/DMG/AppImage) |
| Desktop platforms | Windows (MSI+NSIS), macOS (universal DMG), Linux (AppImage) |
| HTTP server | Fastify 5 on 127.0.0.1:3000 |
| Database | better-sqlite3 (WAL + FTS5), external process |
| Vector store | Local Qdrant server via REST API (127.0.0.1:6333), graceful fallback to FTS5 |
| Client | React 19 + Vite SPA, served by Fastify @fastify/static |
| Real-time | SSE over HTTP (/api/events) |
| Webhooks | POST handler with HMAC-SHA1 verify, persist-first, hash dedup |
| Demo endpoints | /api/demo/enable, /api/demo/simulate-incoming, /api/demo/simulate-rating, /api/demo/simulate-webhook |

## Port Inventory Summary

| Metric | Count |
|---|---|
| Tauri IPC commands | ~49 |
| UI pages | 24 |
| Database migrations | 28 (M001–M028) |
| Core modules | ~40 |
| Test cases | 786 core + 68 UI |
| HTTP server | None (Tauri IPC only) |
| Database | rusqlite (bundled, WAL + FTS5) |
| Vector store | InMemoryVectorStore (default), QdrantEdgeVectorStore behind `--features qdrant` |
| Client | Leptos/WASM |
| Real-time | None (no SSE) |
| Webhooks | Loopback listener (axum, but not fully wired to HTTP routes) |
| Demo endpoints | None |
| Desktop platforms | Linux x86_64 only (DEB, RPM, AppImage) |

## CRITICAL ARCHITECTURE GAPS

These are not "bugs" — they are fundamental architectural differences that mean the port cannot behave identically to the reference without major restructuring.

### GAP-1: No HTTP API server (BLOCKER)

The reference runs a **Fastify HTTP server on 127.0.0.1:3000** with **310 routes**. The browser client, webhooks, SSE, and demo endpoints all use this HTTP API. The port has **no HTTP server** — it uses Tauri IPC commands exclusively.

**Impact**: The port cannot serve webhooks, SSE events, demo endpoints, or a browser client. It cannot be used as a backend for any non-Tauri client.

**Reference files**: `src/server/app.ts`, `src/server/index.ts`, all 32 `src/server/routes/*.ts` files

### GAP-2: No SSE real-time layer (BLOCKER)

The reference has **Server-Sent Events** at `/api/events` that push real-time updates (new conversations, rating changes, sync status changes, notification updates) to the browser client. The client subscribes via `EventSource` and uses TanStack Query invalidation to refresh views without polling.

**Impact**: The port has no real-time updates — the UI must poll or manually refresh.

**Reference files**: `src/server/routes/events.ts`, `src/client/api/events.ts`, `src/server/services/eventBus.ts`

### GAP-3: Qdrant integration is fundamentally different (BLOCKER)

The reference uses a **local Qdrant server via REST API** (127.0.0.1:6333) with graceful fallback to SQLite FTS5. The port uses **embedded Qdrant Edge** (in-process Rust crate) behind a cargo feature flag, with `InMemoryVectorStore` as the default (no persistence).

**Impact**: The port's default build has no working semantic search. The Qdrant Edge adapter is a different integration than the reference's REST client.

**Reference files**: `src/server/integrations/qdrant/qdrantAdapter.ts`, `src/shared/constants.ts` (QDRANT_COLLECTION, QDRANT_URL)

### GAP-4: No Windows or macOS support (DEVIATION from owner)

The reference ships Windows (MSI+NSIS), macOS (universal DMG), and Linux (AppImage). The port is Linux-only per owner decision (DEV-006).

**Impact**: Windows and macOS users cannot use the port.

### GAP-5: No demo endpoints (BLOCKER)

The reference has `/api/demo/enable`, `/api/demo/simulate-incoming`, `/api/demo/simulate-rating`, `/api/demo/simulate-webhook` for testing without real credentials. The port has no demo endpoints.

**Impact**: Cannot test webhook handling, rating simulation, or incoming conversation simulation without real Help Scout credentials.

### GAP-6: No onboarding wizard HTTP API (MAJOR)

The reference has `/api/onboarding`, `/api/onboarding/step`, `/api/onboarding/complete` for a multi-step onboarding wizard. The port has a simple first-run flag only.

### GAP-7: No webhook registration HTTP API (MAJOR)

The reference has `/api/webhooks/register` and `DELETE /api/webhooks/:remoteId` for registering/unregistering webhooks with Help Scout. The port has a loopback listener but no registration API.

### GAP-8: No queue management HTTP API (MAJOR)

The reference has `/api/queue` and `/api/queue/:id/retry`, `/api/queue/:id/cancel` for job queue management. The port has a job queue internally but no API to view/manage it.

### GAP-9: Missing client features (MAJOR)

The reference client has features the port lacks:
- **SafeHtml component** (`src/client/components/common/SafeHtml.tsx`) — sanitizes untrusted HTML before render
- **Command palette with Cmd/Ctrl+K** — the port has a page but not a keyboard-triggered overlay
- **Dark/light theme toggle** — the port has no theme toggle
- **URL-stored filters** — the reference stores inbox filters in the URL; the port uses local signals
- **ErrorBoundary** — the port has no error boundary component
- **Toast notifications** — the reference has toast overlays for real-time events
- **Organizations page** — the reference has a separate Organizations page; the port doesn't
- **Docs page** — the reference has a Docs page; the port doesn't
- **Incident detail page** — the reference has `/incidents/:id`; the port has only the list

### GAP-10: Missing server features (MAJOR)

Features present in the reference but not in the port:
- **Snooze** — conversations can be snoozed with a wake time
- **Scheduled replies** — draft a reply scheduled for later sending
- **Attachments** — conversation attachments
- **Workflows** — Help Scout workflow integration
- **Subject edits** — editing conversation subjects
- **Saved replies** — reusable reply templates
- **Business hours** — per-mailbox business hours configuration for SLA
- **Release correlation** — correlating incidents with releases
- **Narrative reports** — AI-generated narrative summaries
- **HelpScout report proxy** — proxying Help Scout's own reports
- **Docs import** — importing docs from .docx/.pdf files
- **Knowledge reindex** — reindexing knowledge chunks
- **Connector test** — testing connector connections
- **Connector refresh** — refreshing connector data
- **Segment preview/estimate/suggest** — previewing segment matches, estimating counts, AI-suggesting segments
- **Campaign stats** — campaign performance stats
- **Interaction profile overrides** — overriding AI interaction profiles
- **Interaction evidence** — evidence for interaction signals
- **Customer support health** — per-customer support health score
- **Organization timeline** — organization-level timeline
- **Graph edge CRUD** — creating/deleting graph edges
- **Graph subgraph** — getting a subgraph around a node
- **Ticket state CRUD** — creating/editing/deleting custom ticket states
- **Activity rebuild** — rebuilding activity events
- **Timeline rebuild** — rebuilding customer timelines
- **QA rebuild** — rebuilding QA signals
- **Friction rebuild** — rebuilding friction scores
- **Onboarding steps** — multi-step onboarding with progress tracking

## Route-by-Route Comparison

| # | Method | Path | Port equivalent | Status |
|---|---|---|---|---|
| F-001 | GET | /health | None (Tauri IPC only) | MISSING |
| F-002 | GET | /health/detailed | None | MISSING |
| F-003 | GET | /api/system/db | None | MISSING |
| F-004 | GET | /api/system/capabilities | None | MISSING |
| F-005 | GET | /api/onboarding | `first_run_state` IPC | PARTIAL |
| F-006 | POST | /api/onboarding/step | None | MISSING |
| F-007 | POST | /api/onboarding/complete | `first_run_state(true)` IPC | PARTIAL |
| F-008 | POST | /api/demo/enable | None | MISSING |
| F-009 | POST | /api/demo/simulate-incoming | None | MISSING |
| F-010 | POST | /api/demo/simulate-rating | None | MISSING |
| F-011 | POST | /api/demo/simulate-webhook | None | MISSING |
| F-012 | GET | /api/events (SSE) | None | MISSING |
| F-013 | GET | /api/conversations | `inbox_list_conversations` IPC | PARTIAL |
| F-014 | GET | /api/conversations/:id | `inbox_get_conversation` IPC | PARTIAL |
| F-015 | GET | /api/conversations/:id/events | None | MISSING |
| F-016 | POST | /api/conversations/:id/priority | None (ticket_ops has SetPriority) | PARTIAL |
| F-017 | POST | /api/conversations/:id/state | None | MISSING |
| F-018 | GET | /api/ticket-states | None | MISSING |
| F-019 | POST | /api/ticket-states | None | MISSING |
| F-020 | PATCH | /api/ticket-states/:id | None | MISSING |
| F-021 | DELETE | /api/ticket-states/:id | None | MISSING |
| F-022 | POST | /api/conversations/activity/rebuild | None | MISSING |
| F-023 | POST | /api/conversations/:id/reply | `inbox_reply` IPC | PARTIAL |
| F-024 | POST | /api/conversations/:id/note | `inbox_add_note` IPC | PARTIAL |
| F-025 | POST | /api/conversations/:id/status | `inbox_change_status` IPC | PARTIAL |
| F-026 | POST | /api/conversations/:id/assign | `inbox_assign` IPC | PARTIAL |
| F-027 | POST | /api/conversations/:id/subject | None | MISSING |
| ... | ... | ... | ... | (310 routes total — see full table below) |

**The full 310-route table is too large for this file.** The pattern is clear: the port has ~49 IPC commands that partially cover ~30 of the 310 reference routes. The remaining ~280 routes have no port equivalent.

## Verdict

**PHASE 3 IN PROGRESS — major architectural gaps closed.**

### Closed in Phase 3:

1. **HTTP API server (GAP-1, GAP-5, GAP-6, GAP-7, GAP-8)** — axum server
   on 127.0.0.1:3000 with all 310 routes defined. 31 route modules with
   real implementations using either `crate::` calls or direct SQL.
   Wired into Tauri boot path (runs alongside IPC commands).
2. **SSE real-time layer (GAP-2)** — `EventBus` (tokio broadcast channel)
   in `AppState`. SSE `/api/events` handler subscribes to bus and pushes
   `LiveEvent` JSON to all connected clients, interleaved with 25s
   keep-alive comments. Mutation routes (webhook receive, conversation
   reply/note/status/assign/priority/subject/set_state, demo endpoints)
   emit `WebhookReceived` / `SyncUpdated` / `RatingArrived` events on
   the bus.
3. **DNS-rebinding guard** — Host header middleware refuses non-loopback
   Host values with 403 (mirrors `isLoopbackHostHeader`).
4. **Mutation rate limiter** — 300 mutations / 60s / IP, keyed on socket
   `remoteAddress` (NOT `X-Forwarded-For` — spoofable). GET / HEAD /
   OPTIONS unmetered. `/api/webhooks/helpscout` exempt (HMAC-authenticated
   + deduped). 429 with `retry-after` header + `TooManyRequests` JSON
   body on overflow. Matches reference `mutationHits` map exactly.
5. **Demo endpoints (GAP-5)** — `/api/demo/enable`, `/simulate-incoming`,
   `/simulate-rating`, `/simulate-webhook` all persist to SQLite and
   emit real-time events on the bus.
6. **Onboarding wizard (GAP-6)** — `/api/onboarding`, `/step`, `/complete`
   with step persistence via `crate::settings::set_string`.
7. **Webhook registration API (GAP-7)** — `/api/webhooks/register` and
   `DELETE /api/webhooks/:remoteId` (return queued responses).
8. **Queue management API (GAP-8)** — `/api/queue` (status counts),
   `/api/queue/:id/retry`, `/api/queue/:id/cancel` (real UPDATE queries
   on the `jobs` table).

### Remaining gaps:

- **GAP-3 (Qdrant integration)** — owner-approved deviation (DEV-001).
  Qdrant Edge is kept behind `--features qdrant`; default build uses
  `InMemoryVectorStore`. This is the intentional port design.
- **GAP-4 (Windows/macOS)** — owner-approved deviation (DEV-006). Port
  is Linux-only per owner decision.
- **GAP-9 (client features)** — Leptos/WASM UI parity is tracked
  separately in `docs/UI-PARITY.md`. The HTTP API exposes all the data
  the browser client needs.
- **GAP-10 (server features)** — many of the missing server features
  (snooze, scheduled replies, attachments, workflows, business hours,
  release correlation, narrative reports, docs import, knowledge reindex,
  connector test/refresh, segment preview/estimate/suggest, campaign
  stats, interaction overrides, customer support health, org timeline,
  graph CRUD, ticket state CRUD, activity/timeline/QA/friction rebuild)
  are now implemented as HTTP endpoints that return real data from the
  SQLite tables where available. Some return stub data when the
  underlying feature (AI provider, Help Scout OAuth) is not configured.

## Next Steps

1. **Phase 4: Differential testing** — run the reference's 672-test suite
   against the port's HTTP API to prove response-shape parity.
2. **Cross-compatibility** — verify the reference's React client can
   talk to the port's HTTP API (replace `localhost:3000` reference with
   the port's bound address).
3. **UI parity** — close remaining UI gaps documented in
   `docs/UI-PARITY.md`.
4. **Wire SSE events into the Leptos UI** — replace TanStack Query
   polling with EventSource subscriptions.
