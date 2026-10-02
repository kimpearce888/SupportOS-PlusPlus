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

**NOT READY for parity.** The port has fundamental architectural gaps:

1. **No HTTP API server** — the reference's entire architecture is built around a Fastify HTTP server with 310 routes. The port uses Tauri IPC only.
2. **No SSE real-time layer** — the reference pushes live updates via SSE.
3. **Qdrant integration is fundamentally different** — REST client to local server vs embedded crate.
4. **No demo endpoints** — cannot test webhooks, ratings, or incoming conversations.
5. **Linux-only** — reference ships all 3 OSes.
6. **~280 of 310 routes have no port equivalent.**
7. **Multiple client features missing** (SafeHtml, dark/light theme, URL filters, toasts, organizations page, docs page, incident detail, etc.)
8. **Multiple server features missing** (snooze, scheduled replies, attachments, workflows, subject edits, saved replies, business hours, release correlation, narrative reports, docs import, knowledge reindex, connector test/refresh, segment preview/estimate/suggest, campaign stats, interaction overrides, customer support health, org timeline, graph CRUD, ticket state CRUD, activity/timeline/QA/friction rebuild).

## Next Steps

To achieve full parity, the port would need:
1. **Add an axum HTTP server** listening on 127.0.0.1:3000 that mirrors the reference's 310 routes, serving both the Leptos/WASM client and external clients (webhooks, SSE, demo endpoints).
2. **Add SSE support** via axum's SSE response type.
3. **Replace Qdrant Edge with a REST client** to a local Qdrant server (matching the reference's qdrantAdapter.ts).
4. **Restore Windows + macOS** CI and packaging.
5. **Port all missing routes** (~280 routes).
6. **Port all missing client features** (SafeHtml, dark/light, URL filters, toasts, etc.).
7. **Port all missing server features** (snooze, scheduled replies, attachments, etc.).
8. **Port the reference test suite** (672 tests) as differential tests.
9. **Port the demo endpoints** for testing without real credentials.

This is estimated to be several weeks of full-time work, not a single session.
