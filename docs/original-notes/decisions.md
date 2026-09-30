# Original Notes — DECISIONS (reference)

> Condensed from the reference repo's `docs/DECISIONS.md` (~23 KB, 60 ADRs).
> These are the reference's decisions, NOT ours. Our decisions live in `docs/DECISIONS.md`.
> Useful as a pattern catalogue when porting a capability — the reference usually has a documented reason for any oddity.

## Themes

The reference's 60 ADRs cluster into ~8 themes. Each theme below lists the reference decisions in that cluster; the SupportOS++ port may adopt, adapt, or supersede each one (record the choice in `docs/DECISIONS.md`).

### Theme 1 — Data integrity & honest time
- All timestamps stored as ISO-8601 UTC strings; never compared lexically against SQLite `datetime('now')` — always via `julianday()` / `unixepoch()` (KNOWN PITFALL).
- Calendar-day boundaries use date-only arithmetic converted per timezone; DST edge cases tested (23h spring-forward, 25h fall-back, 24.5h Lord Howe, +5:45 Kathmandu).
- `closed_at` stamped in the same transaction as the status write.
- Send timeouts become "unknown" and are reconciled before any retry.

### Theme 2 — Single source of truth for closed vocabularies
- Condition kinds, tile keys, notification types, metrics, dimensions, attribute keys, graph node kinds, Copilot tools — all in `src/shared/` as `as const` arrays. UI, validation, DB, tests all derive from them.
- Operations Center tile counts and inbox filters use the SAME SQL fragment (`tileFragments.ts`) so a tile number can never disagree with the list it links to.

### Theme 3 — SQL safety
- LIKE wildcards escaped; FTS5 queries quoted safely; query length capped.
- Every potentially large query is bounded, indexed, and has `EXPLAIN QUERY PLAN` + a perf test on a synthetic 2,000-conversation dataset.
- User input never becomes a SQL identifier. Conditions and metrics come from closed catalogs. Condition-tree depth and node counts are capped.

### Theme 4 — Sync & webhook idempotency
- Incremental sync (≈5-min cycle) is always the baseline — works with no webhook.
- Webhook is optional; user supplies the reachable address; payloads persist BEFORE processing; dedup by key.
- OAuth state is single-use.
- Re-embed only when content hash changes; failed embedding retries are capped.
- Derived events carry dedup keys so re-syncs are idempotent.

### Theme 5 — AI is advisory
- AI attributes never overwrite Help Scout source data.
- "Unknown" is a legitimate answer; never fabricated.
- AI responses cached by input hash + prompt version.
- Copilot is read-only with a bounded tool allowlist (22 tools, max 5 rounds, max 8 calls, 4000 chars/tool result).
- Customer-reply sending is permanently OFF.

### Theme 6 — Security & secrets
- Secrets stored encrypted in the `secrets` table; redacted on read.
- Never commit credentials, tokens, or user data.
- Loopback-only listener; Host-header validation; timing-safe HMAC; rate limiting.

### Theme 7 — Backup & restore
- AES-256-GCM + scrypt-derived key.
- Authenticated header with version + magic.
- Verify-first import (won't apply a corrupt bundle).
- Safety backup of the current DB before any restore.
- Atomic swap on success.

### Theme 8 — UI parity
- Every view has loading, empty, and error states.
- Escape closes only the topmost dialog.
- Stale async responses are sequenced (don't leak into a different view).
- Draft composer reset per conversation.
- Command palette for universal search (Cmd/Ctrl+K).

## What SupportOS++ inherits unchanged
- Every theme above is adopted verbatim into SupportOS++ (recorded as `D-006` … `D-012` in our `docs/DECISIONS.md` where relevant).

## What SupportOS++ changes
- Reference uses Node/Fastify + React; we use Rust/Tauri + Leptos (D-001, D-005).
- Reference uses external Qdrant process; we embed Qdrant Edge in-process (A4, D-003).
- Reference uses `.supportos` bundle format; we use `.sosync` (D-009, A6 — no compat required).
- Reference bundle targets are MSI/NSIS/DMG/AppImage; we add DEB + RPM per INSTALL AND PACKAGING (Linux is mandatory).
- Reference uses `com.supportos.local` bundle id; we use `com.supportos.plusplus` (D-006, A0).

## What SupportOS++ does NOT do
- No i18n / language switcher / RTL (A1) — reference has none either, but the spec makes this explicit.
- No telemetry, no cloud AI — reference already conforms; we keep the rule strict.

## Full ADR list (titles only)
> The 60 individual ADR titles will be extracted by `xtask discover` (M1-T01) into `docs/original-notes/decisions-full.md` for searchability. The themes above are enough to begin porting any capability without re-reading the reference.
