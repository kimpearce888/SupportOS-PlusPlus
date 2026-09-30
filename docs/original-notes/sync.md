# Original Notes — SYNC (A9 deep-dive)

> Deep-dive on the reference's sync + webhook behaviour, per spec amendment A9.
> Source: `src/server/sync/`, `src/server/routes/sync.ts`, `src/server/routes/webhooks` (if present), `.env.example`, `docs/API-INTEGRATION.md`, `docs/ARCHITECTURE.md`.

## Incremental sync (always the baseline)

### Cycle
- Default: 5 minutes (`SYNC_INTERVAL_MINUTES=5`). Clamped to a sane minimum.
- Reference runs the cycle as a long-lived background job in the Node process; SupportOS++ runs it as a Tauri async task in the Rust core (Tokio).

### Cursor + checkpoint model
- `sync_cursors` table: one row per resource type (conversations, customers, mailboxes, users, teams, ratings, etc.). Columns: `resource`, `last_page`, `last_seen_at`, `cursor_token`.
- `sync_checkpoints` table: records every successful page fetch so a restart can resume mid-resource.
- `sync_runs` table: one row per sync attempt with start/end timestamps, status, error.
- **Overlap window**: `SYNC_OVERLAP_MINUTES=10` — on resume, re-fetch events from `last_seen_at - 10 min` to prevent edge-window misses (Help Scout's "list updated since" endpoint is inclusive at boundaries).

### Resources synced (per Help Scout v2 API)
- Conversations (with threads, tags, custom fields, assignments)
- Customers (with emails, phones, addresses, social profiles, websites, properties)
- Organizations (with properties)
- Mailboxes
- Users (incl. system users)
- Teams + team members
- Tags
- Saved replies
- Workflows (for the automation mirror)
- Ratings (CSAT) — separate lightweight watcher, default 30s (`RATINGS_REFRESH_DEFAULT_SECONDS`)
- Docs (via Docs API key, separate host)

### Rate limiting
- 200 req/min/account. Concurrency limiter at 2. Backoff on 429.

## Webhook (optional, A9)

### How the reference obtains the public callback URL
- Reference uses the `LOCAL_APP_URL` env var as the URL the user must configure in Help Scout's Webhooks UI.
- There is NO bundled tunnel/relay; the user is responsible for making their machine reachable (port forward, reverse proxy, etc.).
- Reference docs explain this plainly in `docs/API-INTEGRATION.md` and on the Sync Health → Webhook push screen.

### What SupportOS++ reproduces
- Same model. The "Webhook push" screen (under Sync Health) shows:
  - Plain-language explanation of what address Help Scout needs.
  - The current registered URL (or "not configured").
  - The state machine: `not configured` → `registered` → `receiving` → `error`.
  - `Register` and `Delete` buttons (Tauri commands `webhook_register` / `webhook_delete`).
  - Last event received timestamp.
- The address is user-supplied (settings); it must be a public URL the user controls.
- **Never** claim real-time updates are active when they are not.

### Webhook pipeline (must match production code path)
1. Help Scout POSTs to the user's URL with HMAC-SHA1 in `X-Helpscout-Signature`.
2. Our loopback listener (D-002) validates Host header, applies rate limit, reads body.
3. **Persist-first**: write the raw event to `webhook_events` table BEFORE verifying signature (so a crash during verify doesn't lose the event).
4. Timing-safe HMAC-SHA1 verify against `HELPSCOUT_WEBHOOK_SECRET`.
5. **Dedup by event id**: if the event id already exists in `webhook_events`, discard.
6. Enqueue a job to process the event (same job queue as sync writes).
7. Job processes the event → updates the same SQLite tables sync would have.
8. Live update: emit a Tauri event so the UI refreshes.

### Restart-safe
- Unprocessed persisted webhook events (status = `pending`) are drained on boot, in order.
- Same dedup applies.

### Demo-mode tool (A10)
- A demo-only action "Simulate webhook event" pushes a synthetic event through the same pipeline (HMAC, dedup, job, sync, live update). Available only when the app is in demo mode.

## What SupportOS++ does NOT do
- Does not bundle, install, or start any third-party tunnel or relay (ngrok, cloudflared, etc.).
- Does not route event payloads through any third party.
- Does not silently switch to webhook mode if the user hasn't configured a URL.
- Does not use the webhook as a replacement for the 5-minute polling baseline — polling always runs.
