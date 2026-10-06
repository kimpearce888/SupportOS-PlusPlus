# progress.md — current work items

This file must contain **exactly the five items currently being worked on** (see `rule.md`).

| ID | Item | Status | Severity | What needs to be fixed |
|---|---|---|---|---|
| AU-03 | Auto-fire on conversation update (workers hook) | Missing | Major | Hook conversation-updated events to fire triggers (worker side) |
| AU-04 | Awaiting-approval gating (parked jobs + approve/reject) | Divergent | Major | Expose approve/reject routes; park via jobs (parity) or rewire tile+sweep to approvals |
| AU-02 | Trigger vocabulary (new_conversation/customer_reply/ai_low_confidence/manual) | Divergent | Major | Port MAIN trigger/condition/action vocabulary and risk tiers |
| DB-09 | jobRepo (enqueue/claim/complete/fail backoff min(300,5*2^n)s/park/recover) | Partial | Major | Port exact backoff seconds; add queue filter to claimNext; add max_attempts to enqueue |
| M15 | Thread delete skips FTS rows and runs outside a transaction | Defect | Major | Delete fts_threads rows for deleted threads inside one transaction (sync_engine.rs:1802-1807 vs MAIN coordinator.ts:698-704). |
