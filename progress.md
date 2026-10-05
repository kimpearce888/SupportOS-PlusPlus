# progress.md — current work item

This file must contain **only the single item currently being worked on** (see `rule.md`).

## In progress

| ID | Item | Status | Severity | Effort | Audit ref | What needs to be fixed |
|---|---|---|---|---|---|---|
| OR-02 | Campaign send path (batch 5, attempts 3, provider.createConversation, sync-back, unknown-state reconcile) | Missing | Blocker | L | B3 | Implement send executor: batch 5, attempts 3, provider.createConversation, sync-back, unknown-state handling |

**Scope of the fix (started 2026-10-06 UTC):**

1. Add `create_conversation` to the `HelpScoutProvider` trait (Fake + Real impls; default impl returns "not supported" so existing test mocks still compile).
2. Add `outreach::send_batch(conn, provider, campaign_id)` — selects up to 5 recipients in `selected`/`queued` state, filters DNC + no-email → `skipped`, increments `attempts`, renders the template, calls `provider.create_conversation`, persists the new `hs_conversation_remote_id`/`number`/`sent_at`, writes `outreach_attempts` rows, logs `outreach_events`, enqueues `sync_conversation` for the new remote id (sync-back), classifies outcomes (`sent` / `failed` permanent 4xx / `queued` retryable 5xx/429 / `failed` after MAX_ATTEMPTS exhausted / `unknown` for null result), re-enqueues the next `outreach_send_batch` if more remain, otherwise marks the campaign `completed`.
3. Wire the `outreach_send_batch` job kind in `workers.rs` to call `outreach::send_batch`.
4. Tests: new unit tests in `outreach.rs` + new integration test `tests/outreach_send_batch.rs` booting the real loopback server against the Fake provider, asserting end-to-end that 3 queued recipients all reach `sent` with a real `hs_conversation_remote_id` + `hs_conversation_number`, `sent_at` set, the new conversation exists in the local `conversations` mirror, the campaign is `completed`, and a permanent 4xx failure path parks the recipient at `failed`.
