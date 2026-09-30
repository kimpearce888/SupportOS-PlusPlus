# Original Notes — CHANGELOG (condensed)

> Condensed from the reference repo's `CHANGELOG.md` (~135 KB, 660+ lines).
> The CHANGELOG is described by the spec as "the most detailed behavior spec" — read in full only when a milestone needs behaviour detail not captured elsewhere.

## Reference version history (top-level releases)

> Major releases correspond to milestones in the reference's own development plan. The reference is currently at `v2.2.1`.

| Version | Notable additions |
|---|---|
| `v2.2.x` | Custom report builder (21 metrics × 14 dimensions) with previous-period comparison; metrics ship their own definition + limitations in the response. |
| `v2.1.x` | Operations Center v2 (16 tiles, single-source-of-truth fragments); Notification Center v2 (15 types, per-type preferences, retention pruning). |
| `v2.0.x` | Incidents (M4): sev1–sev4, 5 statuses, 3 sources, related-kind links; support graph (12 node kinds). |
| `v1.9.x` | AI attributes (14 keys, 2-layer deterministic + AI); Copilot (22 read-only tools, bounded). |
| `v1.8.x` | Saved inbox views (22 condition kinds, AND/OR groups); response state; ticket states + transitions. |
| `v1.7.x` | Activity engine; events; derived timestamps; ticket priority; bug-fix: badge count must use same fragment as filter list. |
| `v1.6.x` | Audit hardening (NODE_ENV validation, secret redaction, FTS query escaping). |
| `v1.5.x` | Outreach (campaigns, segments, do-not-contact); custom objects. |
| `v1.4.x` | Business hours; SLA (risk + breach); docs semantic search; SLA reports. |
| `v1.3.x` | Chat channel; Beacon chat mirror; chat-scoped dashboard. |
| `v1.2.x` | Multi-mailbox; mailbox-level filtering; OAuth refresh-token handling. |
| `v1.1.x` | Local AI (LM Studio); embeddings; hybrid search. |
| `v1.0.x` | Initial Tauri shell; Help Scout OAuth; basic sync; SQLite. |

## Key bug-fix lessons (reproduced in our KNOWN PITFALLS section)

These are the reference's own bug fixes; SupportOS++ must not repeat them. The master spec already codifies them in KNOWN PITFALLS; the CHANGELOG adds context.

- `v1.7.0`: Operations Center tile count disagreed with the inbox filter list — root cause was two different SQL fragments. Fix: one fragment in `tileFragments.ts`, used by both the tile count and the inbox filter.
- `v1.6.0`: `NODE_ENV` blind-cast allowed typos to produce an invalid env value silently. Fix: validate against the literal union.
- `v1.6.0`: FTS5 queries with user input could break with unescaped special chars. Fix: quote FTS5 queries safely + cap query length.
- `v1.5.x`: Outreach campaign recipients that exhausted retries could livelock. Fix: "retry failed" resets the attempt budget; failed recipients fail, never livelock.
- `v1.7.x`: Notification sweep fired too early on first run, spamming the user. Fix: cursors must not init until the first sync settles.
- `v1.4.x`: Calendar-day boundary math broke on DST transitions. Fix: date-only arithmetic converted per zone; test spring-forward (23h), fall-back (25h), Lord Howe (24.5h), Kathmandu (+5:45).
- `v1.3.x`: Composer draft leaked between conversations. Fix: reset composer + draft state per conversation.
- `v1.2.x`: Sync edge-window misses (events at the boundary of the polling window were skipped). Fix: `SYNC_OVERLAP_MINUTES=10` overlap on resume.

## How SupportOS++ uses the CHANGELOG

- The themes above are enough to begin porting any capability.
- For behaviour detail not captured in our `docs/original-notes/`, the next session can read the relevant CHANGELOG section from the local reference checkout (never inside this repo) using `cargo xtask discover --changelog-section <version>`.
