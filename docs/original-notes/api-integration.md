# Original Notes — API-INTEGRATION (Help Scout, Beacon, Docs)

> Condensed from the reference repo's `docs/API-INTEGRATION.md` + `src/server/integrations/` + `src/server/sync/`.

## Help Scout OAuth

- Authorization URL: `https://secure.helpscout.net/authentication/authorizeClient`
- Token URL: `https://api.helpscout.net/v2/oauth2/token`
- Scopes: conversations, customers, mailboxes, users, teams, reports, docs (separate Docs API key for `docsapi.helpscout.net`).
- Redirect URI: must be reachable from the user's browser. Reference default: `http://localhost:3000/oauth/callback`.
- `HELPSCOUT_CLIENT_ID` / `HELPSCOUT_CLIENT_SECRET` / `HELPSCOUT_REDIRECT_URI` env vars in reference.
- In SupportOS++: same OAuth flow but the redirect URI is the loopback listener (D-002) at `http://127.0.0.1:<port>/oauth/callback`. The port is chosen at startup and persisted.

## Help Scout API base & rate limits

- API base: `https://api.helpscout.net/v2/`
- Rate limit: 200 requests/minute per account (per Help Scout docs); reference uses a 2-concurrency limiter with backoff.
- Endpoints used (high-level): conversations list/get/threads, customers, mailboxes, users, teams, tags, workflows, ratings, Beacon chat, Docs.

## Incremental sync (A9 — always the baseline)

- Reference uses a ~5-minute cycle (`SYNC_INTERVAL_MINUTES=5`).
- Cursors + checkpoints: every successful page is recorded in `sync_cursors`; on restart, sync resumes from the last checkpoint with a small overlap window (`SYNC_OVERLAP_MINUTES=10`) to prevent edge-window misses.
- Works with NO webhook configured.

## Webhook (A9 — optional)

- Help Scout webhooks deliver to a user-supplied URL with HMAC-SHA1 signature in `X-Helpscout-Signature` header.
- Reference uses the `LOCAL_APP_URL` env var as the public callback URL the user configures in Help Scout.
- The user must make their machine reachable (port forward, tunnel, etc.); reference does NOT bundle any tunnel.
- HMAC verification is timing-safe; events are persisted BEFORE processing; dedup by event id.
- Restart-safe: unprocessed persisted events are drained on boot.

### SupportOS++ "Webhook push" screen (A9)
- Plain-language explanation of what address Help Scout needs.
- States: `not configured` · `registered` · `receiving` · `error`.
- Tauri commands: `webhook_register` (POSTs to Help Scout Webhooks API to register the URL) and `webhook_delete` (DELETEs it).
- Never claims real-time updates are active when they are not.

## Beacon chat + Docs

- Beacon chat: lives on `api.helpscout.net`, same OAuth token.
- Docs API: separate host `docsapi.helpscout.net`, uses HTTP Basic auth with a separate Docs API key (not OAuth). Reference env: `HELPSCOUT_DOCS_API_KEY`, `HELPSCOUT_DOCS_API_BASE`.
- SupportOS++: same model; the Docs key is stored in the encrypted secrets table.

## Demo-mode tools (A10)

Reference exposes three demo-mode-only actions that push simulated data through the REAL pipeline (HMAC, dedup, job, sync, live update):
1. Simulated webhook event
2. Simulated CSAT rating
3. Simulated incoming customer message

SupportOS++: reproduce as clearly labeled actions available only in demo mode, calling the same code paths as production. Also surface the Copilot read-only tool allowlist (22 tools) in the AI Center for transparency.

## Local AI providers (M5)

- LM Studio: `http://127.0.0.1:1234/v1` (OpenAI-compatible chat + embeddings).
- Ollama: `http://127.0.0.1:11434/api/...` (native API).
- SupportOS++: auto-detect, list models, select, test. App works fully without them. No bundled AI model.

## Connectors (M10)

Reference supports 4 connector kinds:
- `local_json` — read a local JSON file
- `csv` — read a local CSV file
- `sqlite` — read a local SQLite DB (read-only)
- `http` — fetch from an HTTP endpoint (with SSRF guard: blocks loopback, private ranges, link-local; respects allow-list)

SupportOS++: same 4 kinds; the SSRF guard must be ported as a tested matrix (TESTING section of spec).
