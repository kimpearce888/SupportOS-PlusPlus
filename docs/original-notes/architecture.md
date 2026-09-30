# Original Notes — ARCHITECTURE

> Condensed from the reference repo's `docs/ARCHITECTURE.md` (~35 KB). One file per area, per A7.
> These notes are the single source of truth about the reference; the reference never has to be re-read in full.

## High-level architecture (reference)

- **Frontend**: TypeScript + React + Vite, served as static assets by Tauri 2 (`../dist/client`).
- **Backend**: TypeScript Node process (Fastify) running on `127.0.0.1:3000`, hosting ~310 HTTP endpoints across 32 route files.
- **Tauri shell** (`src-tauri/`): thin Rust binary — just `tauri` + `tauri-plugin-single-instance`. Spawns the backend, opens a window, points it at the dev/build URL.
- **Database**: SQLite (better-sqlite3) with WAL + FTS5. 125 tables across 16 migrations.
- **Vector store**: external Qdrant at `http://127.0.0.1:6333` (NOT bundled).
- **AI**: LM Studio via local HTTP at `http://127.0.0.1:1234`. No cloud.

### How SupportOS++ differs (per spec)
- No backend process, no Node, no Fastify. The Tauri Rust core IS the backend. All UI calls go through Tauri IPC commands.
- One tiny embedded loopback HTTP listener (D-002) for webhook + OAuth only.
- Qdrant Edge is embedded in-process (A4), no separate process, no URL.
- Frontend is Leptos/WASM (D-001), not React.
- Same SQLite, same FTS5, same AI advisory model.

## Module map (reference `src/server/`)

| Module | Responsibility |
|---|---|
| `ai/` | LM Studio provider, copilot tools, attribute engine, evidence, friction, post-resolution QA, prompts, translation, interaction intelligence, segment suggestions |
| `analytics/` | Aggregations backing the dashboard |
| `automation/` | Workflow engine, approvals |
| `coaching/` | Coaching suggestions |
| `collaboration/` | Side threads, mentions |
| `config/` | AppConfig loader + logger |
| `connectors/` | External data sources (local JSON/CSV/SQLite/HTTP) with SSRF guard |
| `customobjects/` | Custom object types + fields |
| `database/` | Connection, migrations, 23 repositories |
| `graph/` | Support graph (12 node kinds) |
| `inbox/` | Saved views, response state, priority, ticket states |
| `integrations/` | Help Scout, Beacon chat, Docs |
| `issues/` | Known issues + clusters (Issue Radar) |
| `knowledge/` | Knowledge docs mirror + freshness + gaps |
| `memory/` | Customer memory |
| `notifications/` | Notification sweep engine (15 types) |
| `operations/` | Operations Center (16 tiles), workload, capacity |
| `outreach/` | Campaigns, segments, do-not-contact |
| `routes/` | 32 route files, ~310 HTTP endpoints |
| `search/` | Universal lexical search (FTS5) |
| `security/` | Secret storage, redaction |
| `segmentation/` | Saved segments |
| `services/` | Cross-cutting services (sync, jobs, etc.) |
| `sync/` | Help Scout incremental sync, cursors, checkpoints |
| `timeline/` | Customer timeline |
| `app.ts` / `index.ts` | Process bootstrap |

## Shared layer (`src/shared/`)

14 TypeScript modules — the canonical homes of the closed vocabularies (one source of truth per spec A12 / D-008). SupportOS++ ports each into a Rust enum in `crates/core::catalog`:

| File | Closed vocabularies |
|---|---|
| `activity.ts` | `ACTIVITY_FIELDS` (14), `DATE_MODES` (15 = 7 cal + 6 rolling + 2 exact), 22 condition kinds (via `z.literal`), `RESPONSE_STATES`, `AGE_METRICS`, `TICKET_PRIORITIES` |
| `collaboration.ts` | `OPERATIONS_TILE_KEYS` (16), `NOTIFICATION_TYPES` (15), notification severity map, `OperationsSnapshot` shape |
| `reporting.ts` | `REPORT_METRICS` (21), `REPORT_DIMENSIONS` (14) |
| `graph.ts` | `GRAPH_NODE_KINDS` (12) |
| `constants.ts` | `AI_ATTRIBUTE_CATALOG` (14 keys), `AI_INTENT_VALUES`, `RESPONSE_PREFERENCE_VALUES`, `URGENCY_VALUES`, `FRUSTRATION_VALUES`, `TECHNICAL_VALUES`, `AI_RISK_VALUES`, `PROMPT_VERSIONS`, Copilot bounds |
| `workspace.ts` | `INCIDENT_STATUSES` (5), `INCIDENT_SEVERITIES` (4), `INCIDENT_SOURCES` (3), `INCIDENT_RELATED_KINDS` (4), `CUSTOM_FIELD_TYPES` (6), `CUSTOM_OBJECT_LINK_TARGETS` (6), `CONNECTOR_KINDS` (4), `CONNECTOR_AUTH_MODES` (3), `CUSTOMER_EVENT_KINDS`, `CUSTOMER_EVENT_SOURCES` (5) |
| `coaching.ts` | Coaching vocabulary |
| `memory.ts` | Customer memory shapes |
| `quality.ts` | QA / friction / effectiveness |
| `schemas.ts` | Zod schemas (input validation) |
| `segmentation.ts` | Saved segment shapes |
| `translation.ts` | Translation request shapes (spec §65, included per A1) |
| `types.ts` | Shared TS types |
| `utils.ts` | Shared utilities (HTML to text, etc.) |

## Pipeline shape (reference)

1. Help Scout → OAuth → incremental sync (cursors + checkpoints) → SQLite tables.
2. Sync writes also enqueue derived-event jobs (the activity engine).
3. Activity engine derives `response_state`, `priority`, `closed_at` etc. via SQL fragments (`RESPONSE_STATE_SQL`) — single source of truth for tile counts AND inbox filters (KNOWN PITFALL: "badge must be computed by the same expression as the filter").
4. Webhook (optional, A9): HMAC-SHA1 verify → persist → dedup → enqueue → same job path as sync writes.
5. AI provider (LM Studio): cached by input hash + prompt version; advisory only; evidence-cited; never auto-sends.
6. VectorStore: derived from SQLite text; rebuildable.
7. Backup: `.supportos` file (AES-256-GCM, scrypt, authenticated header, atomic swap). SupportOS++ uses its own `.sosync` format (D-009).

## Key invariants (per ARCHITECTURE.md + KNOWN PITFALLS)

- Reads have no side effects.
- Rebuilds only via explicit commands.
- Status write and `closed_at` in one transaction.
- Send timeouts → "unknown", reconciled before retry.
- Notification sweep cursors must not init until the first sync settles.
- Draft composer is reset per conversation.
- Every view has loading, empty, error states.
- Webhook events persisted BEFORE processing (dedup-by-key).
- OAuth state single-use.
- Derived event dedup keys on every event (idempotent re-sync).
